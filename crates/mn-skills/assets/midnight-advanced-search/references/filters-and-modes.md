# Filters & modes reference

The exact shapes for the `search` tool's `mode` and `filters`. For *recipes*
(how to combine them) see `advanced-techniques.md`. **Before building a
`filters` object, call the `facets` tool** to learn the values that actually
exist in the live corpus — every concrete value below is illustrative.

## Query modes

`mode` is one string per request; default `hybrid`.

| mode | runs | embedding | use it for |
|------|------|-----------|------------|
| `hybrid` | full-text **and** vector, RRF-fused | required | the default; best recall |
| `vector` | vector only | required | purely conceptual questions, paraphrase-heavy wording |
| `fts` | full-text only | **skipped entirely** | exact identifiers, flags, error strings; lowest latency |

- In `fts` mode you do not embed anything — it is the cheapest, lowest-latency
  mode. Reach for it when the literal characters matter (a symbol, a CLI flag, a
  verbatim error). `vector`/`hybrid` need an embedding.
- Rerank is orthogonal: it re-sorts whatever the fused set is, in any mode.
- Rate-limit cost is `max(1, distinct queries)` tokens in every mode. Filters
  are free. `mode` changes work/latency, not token cost.

## Filter model

`filters` is an object keyed by facet name. Across facets the combination is
**AND**; within a set facet's `any_of` it is **OR**; `none_of` excludes. A
missing facet means "no constraint". An empty `any_of: []` also means "no
constraint" (never "match nothing").

Five shapes, by facet type:

| Shape | Wire form | Notes |
|-------|-----------|-------|
| enum-set (closed) | `{ "any_of": [..], "none_of": [..] }` | values from a fixed list (below) |
| open-set | `{ "any_of": [..], "none_of": [..] }` | values are corpus-derived — discover via `facets` |
| object-set | `{ "any_of": [{..}], "none_of": [{..}] }` | structured element matchers |
| bool | `true` / `false` | direct |
| range | `{ "after": "YYYY-MM-DD", "before": "YYYY-MM-DD" }` or `{ "min": N, "max": N }` | inclusive; either bound optional |

A misspelled facet key or an invalid closed-set value is a **`400`**, not a
silent drop (see *Validation*).

## Facet catalog (v1)

All 17 facets. "Neg?" = supports `none_of`.

| key | type | filters on | Neg? | values |
|-----|------|-----------|------|--------|
| `attribution` | enum-set | who vouches for the doc | yes | `foundation`, `partner`, `third_party`, `community`, `unknown` |
| `content_type` | enum-set | the kind of content | yes | `doc`, `tutorial`, `reference`, `example`, `contract_source`, `sdk_source`, `test`, `readme`, `other` |
| `kind` | enum-set | chunk content kind | yes | `markdown`, `code`, `plaintext` |
| `source_kind` | enum-set | the kind of source | yes | `docs_site`, `code_repo`, `standalone`, `mixed` |
| `source_slug` | open-set | a specific source | yes | corpus-derived (`list_sources` / `facets`) |
| `language` | open-set | programming language | yes | corpus-derived (e.g. `compact`, `rust`, `typescript`) |
| `tags` | open-set | provenance tags | yes | corpus-derived |
| `heading_path` | open-set | a heading/section | yes | corpus-derived |
| `symbol` | object-set `{kind?, name?}` | named code symbols | yes | `kind` is an **open** chunker vocabulary (`fn`, `struct`, `impl`, `method`, `class`, …); `name` open |
| `package` | object-set `{kind, name}` | owning package | yes | `kind` ∈ `rust`/`npm`/`compact`/`other`; `name` corpus-derived |
| `verified` | bool | human-vetted flag | n/a | `true` / `false` |
| `deprecated` | bool | deprecation flag | n/a | `true` / `false` |
| `language_target` | object-set `{name, version_satisfies?}` | targeted language + version | no | `version_satisfies` is semver |
| `sdk_dependency` | object-set `{kind, name, version_satisfies?}` | an SDK dependency + version | no | `version_satisfies` is semver |
| `ingested_at` | range (temporal) | when we ingested it | no | `{after?, before?}` ISO dates |
| `source_modified_at` | range (temporal) | upstream last-modified | no | `{after?, before?}` ISO dates |
| `token_count` | range (numeric) | chunk size in tokens | no | `{min?, max?}` |

Sharp edges:

- `symbol.kind` is NOT a closed enum — do not hard-code a list; discover real
  kinds from results or `facets`. Either side of `{kind?, name?}` is optional.
- `version_satisfies` is a semver requirement (e.g. `">=0.23"`, `"^1.2"`)
  evaluated against the target/dependency's declared constraint. The two
  semver-bearing facets (`language_target`, `sdk_dependency`) cannot be negated.
- Ranges are inclusive; give one bound or both.

Example combining several:

```jsonc
"filters": {
  "kind":        { "any_of": ["code"] },
  "language":    { "any_of": ["compact"], "none_of": ["typescript"] },
  "symbol":      { "any_of": [{ "kind": "circuit" }, { "name": "deployContract" }] },
  "deprecated":  false,
  "ingested_at": { "after": "2026-05-01" },
  "token_count": { "min": 50 }
}
```

## Discovery: the `facets` tool

`facets` (cloud `GET /v1/facets`) returns the live facet universe:

```jsonc
{
  "modes": ["hybrid", "vector", "fts"],
  "filters": [
    { "key": "kind",        "type": "enum",       "negatable": true,  "values": ["markdown","code","plaintext"] },
    { "key": "language",    "type": "open_set",   "negatable": true,  "values": ["compact","rust","typescript"] },
    { "key": "tags",        "type": "open_set",   "negatable": true,  "values": ["quickstart","privacy"], "truncated": true, "total": 142 },
    { "key": "ingested_at", "type": "range_temporal", "negatable": false }
    // … one entry per facet
  ]
}
```

- Closed-enum facets carry their full value list. Open-set facets carry
  corpus-derived values.
- High-cardinality sets (`tags`, `symbol.name`, `package.name`) are top-N by
  frequency and flagged `truncated: true` with a `total`. Treat the listed
  values as **examples, not the closed universe** — a value not shown may still
  exist.

## Validation (fail-fast)

Invalid filters return a `400` naming the offending facet and a remediation that
points back at `facets`. The recovery loop is: bad filter → read the message →
call `facets` → fix the key/value → retry. Violations:

- Unknown facet key (lists valid keys).
- Invalid closed-set value (lists valid values).
- Wrong shape for the facet type.
- `none_of` on a non-negatable facet (`language_target`, `sdk_dependency`,
  `verified`, `deprecated`, ranges).
- Contradictory range (`min > max`, `after > before`) or a malformed date.
- Malformed `version_satisfies` semver.
- `mode=vector`/`hybrid` with no vector supplied. `mode=fts` with a vector is
  accepted and the vector ignored.

A *valid* filter that matches nothing returns an empty result set, not an error.

## CLI mapping (`mnm`)

For shell users, `mnm search` exposes the same surface as flags, and `mnm
facets` prints the discovery output:

```
--mode <hybrid|vector|fts>
--kind <markdown|code|plaintext>                 (repeatable → any_of)
--language <lang> / --exclude-language <lang>    (any_of / none_of)
--tag <tag> / --exclude-tag <tag>
--symbol <kind[:name]>                           e.g. --symbol circuit | --symbol :deployContract
--source <slug>  --content-type <t>  --attribution <a>
--no-deprecated  --verified
--ingested-after / --ingested-before <YYYY-MM-DD>
--min-tokens / --max-tokens <n>
--filter-json '<json>'                           escape hatch (mutually exclusive with the granular flags)
```

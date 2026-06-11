# Filters & modes reference

The exact shapes for `mode` (on both `search` and `advanced_search`) and
`filters` (on `advanced_search` only). For *recipes* (how to combine them) see
`advanced-techniques.md`. **Before building a `filters` object, call the
`facets` tool** to learn the values that actually exist in the live corpus —
every concrete value below is illustrative.

## Query modes

`mode` is one string per request; default `hybrid`.

| mode | runs | embedding | use it for |
|------|------|-----------|------------|
| `hybrid` | full-text **and** vector, RRF-fused | required | the default; best recall |
| `vector` | vector only | required | purely conceptual questions, paraphrase-heavy wording |
| `fts` | full-text only | **skipped entirely** | exact identifiers, flags, error strings; lowest latency |

- In `fts` mode nothing is embedded — it is the cheapest, lowest-latency mode.
  Reach for it when the literal characters matter (a symbol, a CLI flag, a
  verbatim error). `vector`/`hybrid` need an embedding.
- Rerank is orthogonal: it re-sorts whatever the fused set is, in any mode.
  Basic `search` always reranks; `advanced_search` exposes a `rerank` toggle
  (default `true` — disable for lowest latency). The reranker loads lazily on
  the first reranked search of a session (one-time delay); `status` reports
  whether it is loaded.
- Rate-limit cost is one token per distinct query in `queries` (basic `search`
  is always one), in every mode. Filters are free. `mode` changes work/latency,
  not token cost.

## Filter model

`filters` is an `advanced_search` object keyed by facet name. Across facets the
combination is **AND**; within a set facet's `any_of` it is **OR**; `none_of`
excludes. A missing facet means "no constraint". An empty `any_of: []` also
means "no constraint" (never "match nothing").

Five shapes, by facet type:

| Shape | Wire form | Notes |
|-------|-----------|-------|
| enum-set (closed) | `{ "any_of": [..], "none_of": [..] }` | values from a fixed list (below) |
| open-set | `{ "any_of": [..], "none_of": [..] }` | values are corpus-derived — discover via `facets` |
| object-set | `{ "any_of": [{..}], "none_of": [{..}] }` | structured element matchers |
| bool | `true` / `false` | direct |
| range | `{ "after": "YYYY-MM-DD", "before": "YYYY-MM-DD" }` or `{ "min": N, "max": N }` | inclusive; either bound optional |

A misspelled facet key or an invalid closed-set value is **rejected before any
network call**, not silently dropped (see *Validation*).

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
  kinds from the `symbol_path` of results you've already retrieved (the
  `facets` tool does not enumerate symbol kinds). Either side of
  `{kind?, name?}` is optional.
- `version_satisfies` is a semver requirement (e.g. `">=0.23"`, `"^1.2"`)
  evaluated against the target/dependency's declared constraint. The two
  semver-bearing facets (`language_target`, `sdk_dependency`) cannot be negated.
- Ranges are inclusive; give one bound or both.

Example `advanced_search` call combining several:

```jsonc
{
  "queries": ["deploy a compact contract"],
  "filters": {
    "kind":        { "any_of": ["code"] },
    "language":    { "any_of": ["compact"], "none_of": ["typescript"] },
    "symbol":      { "any_of": [{ "kind": "circuit" }, { "name": "deployContract" }] },
    "deprecated":  false,
    "ingested_at": { "after": "2026-05-01" },
    "token_count": { "min": 50 }
  }
}
```

## Discovery: the `facets` tool

`facets` with **no arguments** returns the overview — every filter dimension
with its type and negatability:

```jsonc
{
  "modes": ["hybrid", "vector", "fts"],
  "filters": [
    { "key": "kind",        "type": "enum",       "negatable": true,  "values": ["markdown","code","plaintext"] },
    { "key": "language",    "type": "open_set",   "negatable": true,  "values": ["compact","rust","typescript"], "truncated": false, "total": 3 },
    { "key": "tags",        "type": "open_set",   "negatable": true,  "values": ["quickstart","privacy" /* …≤10 samples */], "truncated": true, "total": 142 },
    { "key": "ingested_at", "type": "range_temporal", "negatable": false }
    // … one entry per facet
  ]
}
```

- Closed-enum facets carry their full value list.
- The enumerated open-set facets (`language`, `source_slug`, `tags`,
  `package`) show **up to 10 sample values** plus an exact `total`;
  `truncated: true` means more exist. Treat the samples as **examples, not the
  closed universe** — a value not shown may still exist. The extreme-
  cardinality facets (`heading_path`, `symbol`) advertise their type only —
  no values, no total.
- To see *every* value of an enumerated open-set dimension, drill down with
  `{facet, cursor?, limit?}` where `facet` ∈ `source_slug` | `language` |
  `tags` | `package` (`limit` 1–200, default 50). Each page returns
  `{facet, values, total, next_cursor}`; pass `next_cursor` back as `cursor`
  until it is `null`.

Worked example — paging every tag value (total 142):

1. `facets {"facet": "tags"}` →
   `{"facet": "tags", "values": [/* 50 tags */], "total": 142, "next_cursor": "eyJ…"}`
2. `facets {"facet": "tags", "cursor": "eyJ…"}` → the next 50 + a new
   `next_cursor`
3. `facets {"facet": "tags", "cursor": "<newer cursor>"}` → the last 42, no
   `next_cursor` — you now have all 142 and can build exact `tags` filters.

## Discovery: the `list_sources` tool

`list_sources` is paginated the same way: `{cursor?, limit?}` (`limit` 1–100,
default 20) → `{sources, total, next_cursor}`. It also takes its own filters:

- `kind` — `docs_site` | `code_repo` | `standalone` | `mixed`
- `created_after` / `created_before` — RFC3339 instants on registration time
- `retired: true` — include retired sources (excluded by default)

Use it to discover what material exists and to harvest exact `source_slug`
values for `advanced_search` filters.

## Validation (fail-fast)

Invalid `advanced_search` filters are rejected immediately — before any
network call — with an error naming the offending facet. The recovery loop is:
bad filter → read the message → call `facets` → fix the key/value → retry.
Violations:

- Unknown facet key (lists valid keys).
- Invalid closed-set value (lists valid values).
- Wrong shape for the facet type.
- `none_of` on a non-negatable facet (`language_target`, `sdk_dependency`;
  for bools and ranges `none_of` isn't a representable shape, so those reject
  as shape errors).
- Contradictory range (`min > max`, `after > before`) or a malformed date.
- Malformed `version_satisfies` semver.

Related fail-fasts on the search pair:

- Passing `queries`, `rerank`, or `filters` to basic `search` is rejected with
  a pointer at `advanced_search`.
- Passing single-string `query` to `advanced_search` is rejected — use
  `queries` (one query = a one-element array).

A *valid* filter that matches nothing returns an empty result set, not an error.

## CLI mapping (`mnm`)

For shell users, `mnm search` exposes the same advanced surface as flags, and
`mnm facets` prints the discovery output. One asymmetry: CLI reranking is
**opt-in** via `--rerank` (off by default), the opposite of both MCP search
tools:

```
--mode <hybrid|vector|fts>
--query <text>                                   (repeatable → extra fused queries)
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

# Search Facets & Query-Mode Switching — Design

- **Date:** 2026-06-04
- **Status:** Approved (brainstorming) — ready for implementation planning
- **Driver:** Agent/MCP precision — let AI clients tightly constrain what they retrieve
- **Scope:** v1 (phased). Net-new indexed facets and the recall harness are deferred (see *Out of Scope*).

## 1. Context & Motivation

`POST /v1/search` runs hybrid retrieval: for each query it runs **both** a pgvector
cosine search and a Postgres full-text search, then fuses every ranked list (across
modes and across multi-query pairs) in a single Reciprocal Rank Fusion pass at k=60
(`crates/mn-server/src/routes/search.rs`, `crates/mn-retrieval/src/rrf.rs`).

Two limitations block agent precision:

1. **Filtering is barely reachable and not discoverable.** The server supports seven
   filters (`attribution`, `verified`, `content_type`, `source_slug`, `language_target`,
   `sdk_dependency`, `package`) via `mn_retrieval::filters::SearchFilters`. But the CLI
   hardcodes `SearchFilters::default()` (no flags), and the MCP `search` tool exposes
   `filters` as an opaque `{ "type": "object" }` passthrough with no schema. An agent
   cannot discover what facets exist or what values are valid, and a misspelled key is
   **silently dropped** server-side (`SearchFilters` has no `deny_unknown_fields`), so the
   search runs unfiltered with no error.

2. **No query-mode control.** Both retrieval halves always run; a caller cannot request
   FTS-only or vector-only. There is no `mode` field on `SearchRequest`.

A large amount of useful metadata is already stored but not filterable: `document.kind`
(markdown/code/plaintext), `document.language`, `chunk.symbol_path` (JSONB, GIN-indexed
via migration 0007), `chunk.heading_path`, `provenance.tags`, `provenance.deprecation`,
`source.kind`, ingest/modified timestamps, and token counts.

> **Project note:** midnight-manual is unreleased/pre-1.0. No external clients exercise
> the `filters` shape today (CLI sends empty, MCP forwards blindly), so this design does a
> clean breaking cutover of the filter wire shape — no shims, no dual-shape parsing.

## 2. Goals / Non-Goals

**Goals (v1):**
- Per-request query-mode switching: `hybrid` (default) | `vector` | `fts`.
- A uniform, expressive, **discoverable** filter model over data already stored.
- Filter expressiveness: positive match (AND across facets, OR within), **negation**
  (exclude-lists), and **ranges** (recency, size).
- Surface filters + modes end-to-end: server, typed MCP schema + introspection tool,
  CLI flags.
- Fail-fast validation — no more silent drops.

**Non-Goals (deferred to Phase 2):**
- Net-new indexed facets requiring ingest-pipeline work + corpus backfill
  (verified-code flag, referenced-API-symbols, difficulty/audience level).
- A full boolean/query DSL (nested AND/OR/NOT trees).
- Per-query-pair modes (mode is per-request).
- The recall/eval harness needed to *quantify* retrieval-quality improvement.

## 3. Decisions (locked during brainstorming)

| # | Decision |
|---|----------|
| D1 | Primary driver: **agent/MCP precision**. |
| D2 | Scope: **phased** — wire existing data + modes + discoverability now; defer net-new facets. |
| D3 | Discoverability: **hybrid** — static enums in the MCP `inputSchema` + a dynamic `facets` tool/endpoint for corpus-derived values. |
| D4 | Filter logic: positive match + **negation** + **ranges** (no DSL). |
| D5 | Architecture: **per-facet match objects** backed by a lean **facet registry** (single source of truth for SQL + discoverability). |
| D6 | Breaking cutover of the `filters` wire shape (unreleased; no back-compat). |
| D7 | `fts` mode makes `vector` *and* `client_embedding_model` optional/ignored so agents skip embedding entirely. |
| D8 | MCP validates filters at its own boundary (fail fast); server is authoritative. |
| D9 | CLI `--filter-json` is mutually exclusive with granular flags (no merge). |

## 4. Design

### 4.1 Filter model

Every facet is keyed by name and carries one of five shapes by **facet type**:

| Type | Shape | Semantics |
|------|-------|-----------|
| Enum-set (closed) | `{ any_of: [...], none_of: [...] }` | OR within `any_of`; exclude `none_of` |
| Open-set | `{ any_of: [...], none_of: [...] }` | same; values corpus-derived |
| Object-set | `{ any_of: [{...}], none_of: [{...}] }` | structured element matchers |
| Boolean | `true` / `false` | direct |
| Range | `{ after, before }` (temporal) / `{ min, max }` (numeric) | inclusive, either bound optional |

Across facets the combination is **AND**. The semver-bearing facets (`language_target`,
`sdk_dependency`) are modeled as **object-sets** whose element matchers carry an optional
`version_satisfies` field evaluated in the Rust post-match, not SQL (Postgres has no semver
type); they are not negatable. Example:

```jsonc
"filters": {
  "kind":        { "any_of": ["code"] },
  "language":    { "any_of": ["compact"], "none_of": ["typescript"] },
  "symbol":      { "any_of": [{ "kind": "circuit" }, { "name": "deployContract" }] },
  "tags":        { "any_of": ["quickstart"] },
  "deprecated":  false,
  "ingested_at": { "after": "2026-05-01" },
  "token_count": { "min": 50 }
}
```

#### v1 facet catalog (all backed by stored data)

| Facet | Type | Backing | Negatable | New? |
|-------|------|---------|-----------|------|
| `attribution` | enum | `provenance.attribution` | yes | existing |
| `content_type` | enum | `provenance.content_type` | yes | existing |
| `verified` | bool | `provenance.verified` | n/a | existing |
| `kind` | enum (markdown/code/plaintext) | `document.kind` | yes | **new** |
| `source_kind` | enum (docs_site/code_repo/standalone/mixed) | `source.kind` | yes | **new** |
| `deprecated` | bool | `provenance.deprecation.is_deprecated` | n/a | **new** |
| `source_slug` | open-set | `source.slug` | yes | existing |
| `language` | open-set | `document.language` | yes | **new** |
| `tags` | open-set | `provenance.tags` | yes | **new** |
| `heading_path` | open-set | `chunk.heading_path` (text[]) | yes | **new** |
| `symbol` | object-set `{kind?, name?}` | `chunk.symbol_path` (JSONB `@>`) | yes | **new** |
| `package` | object-set `{kind, name}` | `package` join | yes | existing |
| `language_target` | object-set `{name, version_satisfies?}`¹ | `provenance.language_targets` | no | existing |
| `sdk_dependency` | object-set `{kind, name, version_satisfies?}`¹ | `provenance.sdk_dependencies` | no | existing |
| `ingested_at` | range (temporal) | `source_version.ingested_at` | no | **new** |
| `source_modified_at` | range (temporal) | `document.source_modified_at` | no | **new** |
| `token_count` | range (numeric) | `chunk.token_count` | no | **new** |

`symbol.kind` is a small closed enum (`fn`, `struct`, `circuit`, `witness`, `ledger`,
`module`, `enum`); `symbol.name` is open. `package.kind` is closed (rust/npm/compact/other);
`package.name` is open.

¹ The `version_satisfies` element field is evaluated in the Rust post-match, not SQL; these
facets are not negatable.

#### Facet registry (lean slice of a generic engine)

A single `FACETS` table of descriptors: `{ key, type, sql_mapping, negatable, value_source }`
where `value_source` is `Closed(enum_variants)` or `Open(corpus_query)`. Both the SQL
predicate builder and the `/v1/facets` discovery endpoint read from this registry, so the
advertised facet set cannot drift from what the SQL enforces. Phase-2 facets register here.

### 4.2 Query-mode model

`mode: hybrid | vector | fts` on `SearchRequest`, default `hybrid`, per-request.

Retrieval loop (`search.rs` ~375–428) keys off `mode`:
- `hybrid` → run + push both ranked lists per query (unchanged).
- `vector` → pgvector only; one list per query.
- `fts` → FTS only; one list per query.

RRF is unchanged (it fuses an arbitrary number of lists).

**Embedding contract (D7):**
- In `fts` mode, `vector` and `client_embedding_model` are **optional and ignored**; the
  embedding model-match guard and the vector-dimension guard (`search.rs:287`, `:302`) are
  **skipped**. The agent skips the Voyage round-trip entirely.
- Candidate restriction to the live corpus still applies via the *server-side* resolved
  `corpus_model_id` (`search.rs:781`), which gates rows to the active corpus snapshot in
  every mode.
- `vector` and `hybrid` keep requiring `vector` + `client_embedding_model` and run the guards.

Rerank is orthogonal — it re-sorts whatever the fused set is, for any mode.

### 4.3 Discoverability (D3)

1. **Static enums** in the MCP `search` tool `inputSchema`: replace the opaque `filters`
   object with a typed schema, closed-set facets carrying enum values inline, `mode` as a
   top-level enum. Self-documenting; bad keys/values rejected at the boundary.

2. **`GET /v1/facets` + MCP `facets` tool** for corpus-derived values. One call returns:

```jsonc
{
  "modes": ["hybrid", "vector", "fts"],
  "filters": [
    { "key": "kind",        "type": "enum",       "negatable": true,  "values": ["markdown","code","plaintext"] },
    { "key": "language",    "type": "open_set",   "negatable": true,  "values": ["compact","rust","typescript"] },
    { "key": "tags",        "type": "open_set",   "negatable": true,  "values": ["quickstart","privacy","..."], "truncated": true, "total": 142 },
    { "key": "symbol",      "type": "object_set", "negatable": true,  "element": { "kind": ["fn","struct","circuit","witness","ledger","module","enum"], "name": "open" } },
    { "key": "ingested_at", "type": "range_temporal", "negatable": false },
    { "key": "token_count", "type": "range_numeric",  "negatable": false }
    // … one entry per registry facet
  ]
}
```

Mirrors the existing `sources` tool (`tools.rs:101`). Open-set values come from
registry-declared `SELECT DISTINCT` queries against the active corpus. High-cardinality
sets (`tags`, `symbol.name`, `package.name`) are bounded to top-N by document frequency,
flagged with `truncated: true` + `total`, and framed as *examples, not the closed universe*
(no silent caps). Response cached with a short in-memory TTL (~60s) keyed on the active
corpus-model id; ingest-triggered invalidation is a later refinement.

### 4.4 Surfaces

**Server (`mn-server` / `mn-retrieval`):** `SearchRequest` gains `mode` + redesigned
`filters`. `SearchFilters` rewritten to the per-facet model + `FACETS` registry; SQL builder
extends to positive (`= ANY`), negation (`<> ALL` / `NOT @>`), JSONB containment (`symbol`),
and range (`BETWEEN` / `>=` / `<=`) clauses. New `GET /v1/facets` route.

**MCP (`mn-mcp`):** typed `search` `inputSchema`; new `facets` tool; filter args validated
against the schema before forwarding (fail fast).

**CLI (`mn-cli`):** `mnm search` assembles filters from flags (no longer hardcodes
`SearchFilters::default()`). Representative flags:

```
--mode <hybrid|vector|fts>
--kind <markdown|code|plaintext>                 (repeatable → any_of)
--language <lang> / --exclude-language <lang>    (repeatable → any_of / none_of)
--tag <tag> / --exclude-tag <tag>
--symbol <kind[:name]>                           e.g. --symbol circuit | --symbol fn:deployContract
--source <slug>  --package <kind:name>  --content-type <t>  --attribution <a>
--no-deprecated  --verified
--ingested-after / --ingested-before / --modified-after / --modified-before <date>
--min-tokens / --max-tokens <n>
--filter-json '<json>'                           escape hatch (mutually exclusive with granular flags)
```

Repeatable flags → `any_of`; `--exclude-*` → `none_of`. New `mnm facets` subcommand prints
`GET /v1/facets`.

### 4.5 Error handling & validation

No more silent drops. All violations → `400` with the offending key/value, the reason, and
a remediation hint pointing at `GET /v1/facets`, via the existing `CoreError` + `ErrorCode`
builder:

- Unknown facet key (lists valid keys).
- Invalid closed-set value (lists valid values).
- Wrong shape — strict, one canonical shape per facet type; no scalar shorthands.
- Negation on a non-negatable facet.
- Contradictory range (`min > max`, `after > before`) or bad date.
- Malformed `version_satisfies` semver (tightened to fail-fast).
- Mode/vector consistency: `mode=vector|hybrid` with missing/empty `vector` → 400;
  `mode=fts` with a vector present → accepted, ignored.

**Deliberate leniency:** empty `any_of: []` is treated as *absent* (no constraint), never as
"match nothing." Validation runs at the MCP boundary (JSON-schema, fast-fail) and
authoritatively in `mn-retrieval` against the registry. A valid filter matching nothing
returns an empty result set, not an error.

### 4.6 Testing

Scoped to **correctness**, not retrieval quality.

- **Unit (`mn-retrieval`):** every validation rule; serde round-trip; SQL fragment
  construction (positive / negation / JSONB / range); a **registry-drift guard** asserting
  every facet has a SQL mapping, appears in `/v1/facets`, and that closed enums match the
  MCP `inputSchema`; mode → list-assembly logic.
- **Integration (testcontainers PG+pgvector, `--features integration`):** fixture corpus
  spanning all facets; each facet narrows correctly (positive/negation/range), AND-across,
  OR-within; `mode=fts` returns hits with no vector supplied (embedding-skip end-to-end);
  `vector`/`hybrid` behave; `/v1/facets` returns correct closed enums + fixture open-set
  values with truncation flagged; a regression test asserting bad filters now `400` (silent
  drop gone). Runs in CI per-PR + nightly, not the local sandbox.
- **Property (`proptest`):** filter round-trip identity; monotonicity (adding a facet never
  adds results); negation invariant (`none_of:[x]` never yields `x`).
- **MCP / CLI:** schema-rejection at the MCP boundary; `facets` tool against a stubbed
  client; CLI flag → filter mapping and `--filter-json` mutual-exclusion error.

## 5. Out of Scope / Phase 2

- **Net-new indexed facets** (each needs ingest-pipeline work + a migration + corpus
  backfill, and registers into the `FACETS` table):
  1. **Code-compiles/runs-verified flag** — distinct from `provenance.verified` (human
     vetting); records whether code was compiled *and executed* against a compiler/SDK
     version. Highest-value agent-precision facet.
  2. **Referenced API symbols** — stdlib/SDK identifiers a chunk *uses* (distinct from
     `symbol_path`, where it *lives*).
  3. **Difficulty / audience level** — beginner/intermediate/advanced.
  - Runners-up: upstream authorship date (VCS commit time), source license (SPDX).
- **Recall harness** — this feature makes single-mode retrieval first-class and selectable,
  which is the precondition **SC-014** (hybrid vs single-mode recall@10) needs to measure.
  The harness (labelled query/relevant-chunk sets, `recall@k`/`nDCG@k`, the `fuse_with_k`
  sweep seam) does not exist yet (`tests/recall/`, `benches/` are absent). Recommended as a
  separate follow-up spec so mode/facet quality — and the long-standing RRF `k` value — can
  actually be quantified rather than assumed.

## 6. Risks & Notes

- **`symbol_path` population.** Migration 0007 noted "no code chunks exist yet." Before the
  `symbol` facet is useful, confirm `mn-content`'s code chunker actually populates
  `symbol_path` for code chunks; otherwise the facet matches nothing. Verify during
  implementation; backfill if needed.
- **Open-set cardinality.** `tags` / `symbol.name` / `package.name` can be large — bounding
  + truncation flags keep `/v1/facets` responses bounded.
- **OpenAPI / contracts.** `specs/001-rag-platform/contracts/openapi.yaml` documents the old
  flat filter shape and must be updated to the new model + `mode` + `/v1/facets`.

---
title: MCP tools
sidebar_label: MCP tools
---

# MCP tools reference

The Midnight Manual MCP server exposes **13 tools** across four categories. This page documents each tool's purpose, input parameters, and output shape, sourced directly from `crates/mnm-mcp/src/tools.rs`.

---

## Search

### `search`

Search the Midnight Network documentation and code corpus (docs, SDK references, Compact language material, code examples). Returns ranked excerpts with confidence scores and source attribution. This is the simple 90%-surface tool; for multi-query fusion, facet filters, or rerank control, use `advanced_search`.

**Input parameters**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `query` | string (required) | — | What you want to find, as natural language or code terms. |
| `mode` | `hybrid` \| `vector` \| `fts` | `hybrid` | `hybrid` fuses keyword + semantic; `fts` is keyword-only (lowest latency); `vector` is semantic-only. |
| `code_mode` | `on` \| `off` \| `exclusive` | on (for hybrid/vector) | Code-vector fusion. `on` fuses a `voyage-code-3` list alongside general results; `off` = general retrieval only; `exclusive` replaces the general vector list. Incompatible with `mode=fts`. |
| `limit` | integer 1–50 | `10` | Max results returned. |

**Output** — same envelope as `advanced_search`: ranked results with chunk ids, scores, and source attribution.

---

### `advanced_search`

Full-control search: fuse multiple queries (HyDE, expansion, step-back), restrict by facet filters, switch retrieval mode, and toggle reranking. Use when basic search comes up short or when the `midnight-advanced-search` skill prescribes a pattern. Call `facets` first to discover valid filter values.

**Input parameters**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `queries` | array of 1–10 strings (required) | — | Query variants fused with RRF. One query = one-element array. Rate-limit cost is one token per distinct query. |
| `mode` | `hybrid` \| `vector` \| `fts` | `hybrid` | Same as `search`. |
| `code_mode` | `on` \| `off` \| `exclusive` | on (for hybrid/vector) | Same as `search`. |
| `limit` | integer 1–50 | `10` | Max results returned. |
| `rerank` | boolean | `true` | Apply VoyageAI reranking against the first query (server-side, or locally with your own `VOYAGE_API_KEY`). Disable for lowest latency. |
| `version_match` | `strict` \| `permissive` | `permissive` | `permissive` biases ranking and drops only breaking mismatches; `strict` hard-filters to version-satisfying content only. |
| `rerank_instructions` | string (max 400 chars) | — | Optional rerank instruction. Guides relevance: emphasize aspects, filter document kinds, or disambiguate terms. Replaces the derived default instruction. |
| `filters` | object | — | Per-facet filters. AND across keys, OR within `any_of`, exclude `none_of`. See the `facets` tool for corpus-derived values. |

**Filter dimensions** available in `filters`:

- `kind`, `source_kind`, `attribution`, `content_type` — closed-set enum filters
- `language`, `tags`, `source_slug`, `heading_path` — open-set string filters
- `verified`, `deprecated` — boolean flags
- `symbol` — object filter matching `{kind, name}` pairs
- `package` — object filter matching `{kind, name}` pairs
- `language_target`, `sdk_dependency` — object filters with optional `version_satisfies` semver constraint
- `ingested_at`, `source_modified_at` — date-range filters (`after`, `before`)
- `token_count` — integer-range filter (`min`, `max`)

**Output** — ranked results with chunk ids, scores, source attribution, and (when reranked) `rerank_score` per result.

---

## Chunk navigation

### `get_chunks`

Fetch the full content of one or more chunks by id, typically ids returned by search. Use this to read the actual text behind search results.

**Input parameters**

| Parameter | Type | Description |
|---|---|---|
| `ids` | array of 1–20 UUID strings (required) | Chunk ids to fetch. One id is a one-element array. |

**Output** — array of chunk objects with full body text.

---

### `get_chunk_next`

Fetch chunks that immediately follow a given chunk in its document's reading order. Use to continue reading past the end of a chunk you already have.

**Input parameters**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `id` | UUID string (required) | — | Anchor chunk id (from search results or another chunk tool). |
| `count` | integer 1–100 | `5` | Number of chunks to return. Calling past the document edge returns an empty list, not an error. |

**Output** — ordered list of chunk objects.

---

### `get_chunk_prev`

Fetch chunks that immediately precede a given chunk in its document's reading order. Use to read the context leading up to a chunk you already have.

**Input parameters**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `id` | UUID string (required) | — | Anchor chunk id. |
| `count` | integer 1–100 | `5` | Number of chunks to return. Calling past the document edge returns an empty list. |

**Output** — ordered list of chunk objects.

---

### `get_chunk_neighbors`

Fetch the chunks immediately before and after a given chunk in one call. Use when a search hit needs surrounding context to be understood.

**Input parameters**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `id` | UUID string (required) | — | Anchor chunk id. |
| `count` | integer 1–100 | `2` | Chunks to fetch on each side of the anchor. A side past the document edge comes back empty. |

**Output** — object with `prev`, `anchor`, and `next` arrays.

---

### `get_chunk_parents`

Show where a chunk sits in its source's structure: the chain of containing nodes (document, folders) up to the source root. Use to orient a chunk within its source and find its containing document.

**Input parameters**

| Parameter | Type | Description |
|---|---|---|
| `id` | UUID string (required) | Chunk id. |

**Output** — ordered ancestry chain from the chunk up to the source root.

---

## Document

### `get_document`

Fetch a document's metadata plus an ordered skeleton of its chunks (ids, positions, token counts, no bodies). Use to size up a document before reading it with `get_document_chunks`.

**Input parameters**

| Parameter | Type | Description |
|---|---|---|
| `id` | UUID string (required) | Document id. |

**Output** — document metadata with chunk skeleton (ids, positions, and token counts; no bodies).

---

### `get_document_chunks`

Read a window of a document's chunk bodies by position. Use after `get_document` to read a document section by section.

**Input parameters**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `id` | UUID string (required) | — | Document id (from search results or `get_document`). |
| `from` | integer ≥ 0 | `0` | Zero-based chunk position to start from. A position past the end returns an empty window with accurate `total_chunks`. |
| `limit` | integer 1–100 | `20` | Number of chunk bodies to return. |

**Output** — windowed array of chunk bodies plus `total_chunks` for pagination.

---

## Discovery

### `list_sources`

List the sources that make up the corpus (paginated). Use to discover what material exists and to get source slugs for `advanced_search` filters.

**Input parameters**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `cursor` | string | — | Opaque pagination token from a previous response's `next_cursor`. |
| `limit` | integer 1–100 | `20` | Sources per page. |
| `created_after` | RFC3339 date-time | — | Only sources registered after this instant. |
| `created_before` | RFC3339 date-time | — | Only sources registered before this instant. |
| `kind` | `docs_site` \| `code_repo` \| `standalone` \| `mixed` | — | Filter by source kind. |
| `retired` | boolean | `false` | Include retired sources. |

**Output** — paginated list of source metadata objects with optional `next_cursor`.

---

### `facets`

Discover the filter dimensions available to `advanced_search` and the values present in the corpus. Call without arguments for an overview; pass a `facet` name to page through all values of one dimension.

**Input parameters**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `facet` | `source_slug` \| `language` \| `tags` \| `package` \| `language_target` \| `sdk_dependency` | — | Drill into one open-set facet's full value list. Omit for the overview. |
| `within` | string | — | Second drill level: enumerate declared version constraints within one name (`language_target`/`sdk_dependency`) or one package name. These values are supplied to `advanced_search` via a filter's `version_satisfies` field. |
| `cursor` | string | — | Opaque token from a previous drill-down response. |
| `limit` | integer 1–200 | `50` | Values per page. |

**Note:** `facet` is a **parameter** of this tool, not a separate tool.

**Output** — facet overview or paginated value list, with optional `next_cursor` for drill-down responses.

---

## Diagnostics

### `status`

Diagnose the retrieval setup: cloud reachability, authentication and rate-limit state, VoyageAI key validity, and rerank configuration. Call when searches fail, return errors, or before starting a long session.

**Input parameters** — none.

**Output** — structured diagnostic report covering cloud health, auth state, embedding model, and rerank readiness.

---

## Skill install

### `install_search_skill`

Install (or update) the `midnight-advanced-search` skill, a retrieval playbook teaching effective corpus search patterns, into the user's AI harness(es). Use when search results are poor or the user asks for better search guidance.

**Input parameters**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `harness` | array of `claude-code` \| `codex` \| `opencode` \| `cursor` | — | Harnesses to install for. Omit to auto-detect. |
| `scope` | `user` \| `project` | `user` | Install scope. |

**Output** — installation report: which harnesses were updated and where the skill file was written.

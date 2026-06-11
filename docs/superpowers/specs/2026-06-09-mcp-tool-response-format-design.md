# MCP Tool Response Format — Design

- **Date:** 2026-06-09
- **Status:** Approved (brainstorming)
- **Scope:** `mn-mcp`, `mn-store`, `mn-retrieval`, `mn-server`, `mn-telemetry`
- **Branch:** `mcp-response-format`

## Problem

Every `mn-mcp` tool serializes its cloud/local response to a JSON string and dumps that
*entire blob* into a single `text` content block. For `search` that means the calling agent
must reason over a wall of per-result scoring internals (`rrf_score`, `vector_similarity`,
`confidence_factors`, `matched_queries`, …) to find the one or two fields it actually needs.
There is no `structuredContent`, no `outputSchema`, and tool failures come back as JSON-RPC
errors rather than `isError` results, so the model cannot self-correct in-conversation.

We bring tool responses in line with the MCP guidance: a concise, actionable `content` text
block plus a machine-readable `structuredContent` payload, `outputSchema` per tool, and
`isError` results for tool-execution failures. While we are in the response path we also
(a) enrich results with readable identity so summaries are meaningful, and (b) capture the
new retrieval-quality signal in telemetry — because we are about to go live and telemetry
cannot be collected retroactively.

## Goals

- Concise summary + machine-readable structured data on every tool result.
- `outputSchema` advertised per tool; `structuredContent` validated against it.
- Tool-execution failures become `isError: true` results with a structured error envelope.
- Search/chunk/document results carry readable identity (path, breadcrumb, source name).
- Retrieval-quality telemetry captured now and retained long-term.

## Non-goals

- No `inputSchema` changes (no caller-facing verbosity/`include_scoring` toggle — every
  response already carries both trimmed and full views; the client chooses which to read).
- No re-ingest and no corpus-table migration (readable identity already exists in the DB).
- No change to the privacy / client-opt-out telemetry model.

## Decisions (from brainstorming)

| # | Decision | Choice |
|---|---|---|
| 1 | Text-block strategy | One `content` text block = concise summary **+** trimmed JSON in a ` ```json ` fence. Full fidelity in `structuredContent`. |
| 2 | Error model | **All** tool-execution failures → `isError: true` results with a structured error envelope. JSON-RPC errors reserved for protocol faults only. |
| 3 | `outputSchema` | Advertise per tool + assert `structuredContent` conformance in tests. |
| 4 | Readable identity | Pulled into scope — enrich cloud responses (Phase 1). |
| 5 | Telemetry | Pulled into scope — capture retrieval-quality signal (Phase 3). |
| 6 | Telemetry retention | Extend the daily rollup to preserve the signal; bump raw retention 7 → 90 days. |

## Phasing overview

1. **Cloud enrichment** — readable identity into search + chunk/document responses (no migration).
2. **`mn-mcp` response reformat** — the MCP-facing change, consuming the enriched fields.
3. **Telemetry capture** — new event fields + dimensional rollup + retention bump.

Phase 2 depends on Phase 1's fields. Phase 3 is independent and can land in parallel.

---

## Phase 1 — Cloud enrichment

All readable identity already exists in the schema and is reachable per chunk via joins, so
this is a pure `SELECT`/join + response-shape change. No migration, no re-ingest.

**Fields to promote** (onto each search result and the chunk/document passthrough responses):

| Field | Source column |
|---|---|
| `source_display_name` | `source.display_name` |
| `source_path` | `document.source_path` (NOT NULL, e.g. `docs/intro.md`) |
| `published_url` / `source_url` | `document.published_url`, `document.source_url` |
| `heading_path` | `chunk.heading_path[]` (Markdown breadcrumb) |
| `symbol_path` | `chunk.symbol_path[]` (code breadcrumb) |

The join is `chunk → document → source_version → source`. `heading_path` / `symbol_path` are
already on `chunk`; add them to the projected columns alongside `content`.

**Touched:**

- `mn-store` — extend the search query (`SELECT` + join) and the row struct it decodes into;
  same for the chunk/document fetch queries.
- `mn-retrieval` — add the readable fields to the result type that flows out of hybrid query
  construction / scoring.
- `mn-server` — include the fields in the `/v1/search`, `/v1/chunks/*`, `/v1/documents/*`
  response bodies; update `specs/001-rag-platform/contracts/openapi.yaml` accordingly.

**Result:** a summary can read `docs/intro.md › "Compiling Witnesses" (foundation · 0.81)`
instead of naming a bare UUID.

---

## Phase 2 — `mn-mcp` response reformat

### Output contract is mn-mcp–owned (normalized, not raw passthrough)

`mn-mcp` transforms each cloud/local response into its own shape — useful fields promoted to
the top level, scoring internals preserved but nested under a `scoring` object. `outputSchema`
then describes a contract *we* own, decoupled from the cloud wire format.

```rust
struct ToolOutcome {            // success
    summary: String,            // concise human/agent guidance
    structured: Value,          // structuredContent — full canonical, conforms to outputSchema
    trimmed: Value,             // essentials only (the fenced JSON in the text block)
    next_actions: Vec<NextAction>,   // folded into structuredContent
}

struct ToolFailure {            // error
    code: ErrorCode,            // string enum (see envelope below)
    message: String,
    retryable: bool,
    details: Value,             // case-specific (mismatch / too_many_chunks / field …)
    next_actions: Vec<NextAction>,
    guidance: String,           // concise text for the content block
}
```

`server.rs` becomes a thin adapter: `Result<ToolOutcome, ToolFailure>` → `ToolCallResult`.
Protocol-level faults (parse error, unknown method/tool, malformed `tools/call`) stay as
JSON-RPC `Response::err`.

### Success result shape

One `content` text block: `summary` then the trimmed JSON in a ` ```json ` fence. Full
fidelity in `structuredContent` (adds `scoring`, `search_metadata`, `next_actions`).

```jsonc
{
  "content": [{ "type": "text",
    "text": "Search: 10 matches, corpus voyage-code-3@1. Top: docs/intro.md › \"Compiling Witnesses\" [foundation · 0.81] chunk 1f39fa7c — fetch with get_chunk.\n\n```json\n{\"results\":[{\"rank\":1,\"chunk_id\":\"1f39…\",\"document_id\":\"7d5c…\",\"source_path\":\"docs/intro.md\",\"heading_path\":[\"Compiling\",\"Witnesses\"],\"confidence\":0.81,\"attribution\":\"foundation\",\"content\":\"…\"}],\"match_count\":10}\n```" }],
  "structuredContent": {
    "corpus_embedding_model": "voyage-code-3@1",
    "results": [{ "rank": 1, "chunk_id": "1f39…", "document_id": "7d5c…",
      "source_display_name": "Compact Docs", "source_path": "docs/intro.md",
      "heading_path": ["Compiling","Witnesses"], "confidence": 0.81,
      "attribution": "foundation", "verified": true, "content": "…",
      "scoring": { "rerank_score": 0.54, "rrf_score": 0.0129, "vector_similarity": 0.539,
                   "trust_score": 1.0, "confidence_factors": { } } }],
    "search_metadata": { },
    "next_actions": [ { "tool": "get_chunk", "arguments": { "id": "1f39…" } },
                      { "tool": "get_document", "arguments": { "id": "7d5c…" } } ]
  },
  "isError": false
}
```

`trimmed` = the normalized fields **minus** `scoring` and `search_metadata`.

### Error result shape

Same single-text-block convention, mirrored. Shared envelope across all tools — **not** bound
to any tool's `outputSchema` (schemas constrain success output only).

```jsonc
{
  "content": [{ "type": "text",
    "text": "Chunk e4f1… not found — verify the id against a recent search result before retrying.\n\n```json\n{\"error\":{\"code\":\"NOT_FOUND\",\"retryable\":false}}\n```" }],
  "structuredContent": {
    "error": { "code": "NOT_FOUND", "retryable": false, "message": "no chunk with id e4f1…" },
    "next_actions": [ { "tool": "search", "arguments": { "query": "<your terms>" } } ]
  },
  "isError": true
}
```

| `error.code` | From | `retryable` | `details` / `next_actions` |
|---|---|---|---|
| `INVALID_INPUT` | `SearchError::InvalidInput`, `PassthroughError::InvalidInput`, bad install scope/harness | true | `field`; guidance names the fix |
| `NOT_FOUND` | `PassthroughError::NotFound` | false | next_action → `search` |
| `EMBEDDING_MODEL_MISMATCH` | `SearchError::Mismatch` | true (after remedy) | `{corpus_model, client_model, remediation}`; next_action → `pull_models` |
| `TOO_MANY_CHUNKS` | `PassthroughError::TooManyChunks` | true (via other tool) | `{chunk_count, cap, hint}`; next_action → `get_document_chunks` |
| `CLOUD_ERROR` | `*::Cloud`, list_sources/facets cloud err | true | transient; guidance suggests retry |
| `MODEL_LOAD_FAILED` | `run_pull_models` err | true | — |
| `INSTALL_FAILED` | `run_install_search_skill` err | varies | — |

**Retained as JSON-RPC errors (protocol faults only):** parse error, invalid request,
method-not-found, unknown tool name, missing `name` in `tools/call`. The existing
`mismatch_response` / `too_many_chunks_response` move out of the JSON-RPC `error.data` channel
and become `ToolFailure` builders.

### outputSchema

Shared normalized fragments compose the schemas rather than 14 from-scratch definitions:

```jsonc
Chunk = { chunk_id, document_id, source_version_id, chunk_index, total_chunks, content,
          source_display_name, source_path, published_url?, heading_path[], symbol_path[],
          attribution?, verified? }
SearchResult = Chunk + { rank, confidence, scoring{ } }
Document     = { document_id, source_display_name, source_path, chunk_count, … }
```

One `fn <tool>_output_schema() -> Value` per tool, wired into `list()` next to the existing
`inputSchema`. Each tool's `structured` payload validates against it (tests, via the
`jsonschema` workspace dep). Error results are exempt.

### Per-tool summary + next_actions

`trimmed` for every tool = its normalized fields minus `scoring`/`search_metadata`.

| Tool | Summary gist | next_actions |
|---|---|---|
| `search` | `N matches, corpus <model>. Top: <source_path> › <heading> [<attr>·<conf>] chunk <id>` | get_chunk(top), get_document(top doc) |
| `get_chunk` | `Chunk <id> — <source_path> › <heading> (idx i/total)` | next/prev/neighbors/parents, get_document |
| `get_chunk_next`/`_prev` | `<n> chunk(s) after/before <id>` | continue same direction |
| `get_chunk_neighbors` | `<n> neighbors around <id> (±k)` | get_document |
| `get_chunk_parents` | `<k> ancestor chunk(s) of <id>` | get_document |
| `get_document` | `<source_path> (<source_display_name>): <n> chunks` | get_document_full, get_document_chunks |
| `get_document_full` | `Full <source_path>: <n> chunks (~<len> chars)` | — |
| `get_document_chunks` | `Chunks <from>..<to> of <source_path>` | next window |
| `list_sources` | `<n> sources: <a>, <b>, …` | search |
| `facets` | `Facets: <dim>=<top values>…` | search w/ filter |
| `status` | `Embedder <x> loaded; reranker <y> <state>` | pull_models (if unloaded) |
| `pull_models` | `Pulled <list>. Ready.` | status |
| `install_search_skill` | `Installed search skill → <path> (<harness>)` | — |

### `protocol.rs` changes

- `ToolCallResult` gains `structured_content: Option<Value>`
  (`#[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]`).
- `ToolDescription` gains `output_schema: Option<Value>`
  (`#[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]`).
- `ContentBlock::Text` is unchanged — the trimmed JSON rides inside the text as a fenced block.

---

## Phase 3 — Telemetry capture

Telemetry is **client-emitted and opt-out at the client** (FR-111 / FR-107). The cloud server
is only a sink (`POST /v1/telemetry/events` → `telemetry_event_raw`); it emits no per-search
event. Retrieval-quality telemetry therefore rides on `mn-mcp`'s existing `McpToolCall` event,
honoring opt-out. Always-on server-side capture is **rejected** — it would bypass the
client-opt-out privacy model.

### New event fields (additive `Option` scalars on `McpToolCall`)

`Option` + `#[serde(default)]` so historical rows remain decodable; `None` for non-search tools.

| Field | Type | Notes |
|---|---|---|
| `corpus_model` | `Option<String>` | e.g. `voyage-code-3@1` |
| `reranker_used` | `Option<enum>` | which reranker actually ran (vs. `rerank_on` = requested) |
| `top_confidence` | `Option<enum bucket>` | bucketed, not raw float |
| `top_attribution` | `Option<enum>` | foundation / partner / community / unknown |
| `filtered_by_confidence` | `Option<u32>` | count dropped below threshold |
| `deduplicated_count` | `Option<u32>` | count removed by dedup |
| `top_source` | `Option<String>` | `source.display_name` of the top result — highest-value dimension; corpus metadata, canary-safe |

`telemetry_event_raw.fields` is JSONB → **no raw-table migration**. Regenerate
`specs/001-rag-platform/contracts/telemetry-events.json` (schemars) from the updated types.

### Dimensional rollup (new) + retention bump

Raw rows are purged after the retention window, and `telemetry_aggregate_daily` keeps only a
`count` per `(day, event_type, component)` — so the new signal would vanish without an
aggregation change.

- **New table** `telemetry_search_daily (day, corpus_model, attribution, reranker, top_source,
  confidence_bucket, count, PRIMARY KEY(day, corpus_model, attribution, reranker, top_source,
  confidence_bucket))` (migration under `crates/mn-store/migrations/`).
- **`sweep_once`** gains a second aggregate step: for expired `mcp_tool_call` rows whose
  `tool_name = "search"`, `GROUP BY` the dimensions above out of the JSONB `fields` and
  upsert into `telemetry_search_daily`, in the **same transaction** before the `DELETE`. The
  existing `(day, event_type, component, count)` rollup is untouched. The dimensional table
  retains the signal indefinitely.
- **Retention** `telemetry_raw_retention_days` default **7 → 90**. Deliberate deviation from
  FR-110's documented default of 7, recorded here. Hourly `SWEEP_INTERVAL` cadence unchanged.
- `routes/metrics.rs` may expose the new counters as additional Prometheus rows (optional,
  fast-follow).

### Privacy / canary

The canary suite (FR-112 / SC-061, release-gated) forbids in any log or event: query text,
chunk content, bearer tokens, user-machine filesystem paths, env values, IPs, user
identifiers. Every new field is an enum / bucket / count / corpus-catalog name — none carry
user content or paths. Extend `crates/mn-telemetry/tests/canary_suite.rs` to feed a canary
query (and a canary-named source) through search and assert none of the new fields leak.

> Note: `source_path` / `heading_path` / `symbol_path` are surfaced in **tool responses**
> (Phase 1/2) but are **not** emitted to telemetry — path-shaped values stay out of events.

---

## Testing

- **Conformance:** each tool's representative `structured` payload validates against its
  `outputSchema`.
- **Shape:** success → exactly one text block (`summary` + fenced `json`), `structuredContent`
  present, `isError` absent/false. Failure → `isError: true`, `structuredContent.error.code`
  set, summary is guidance (not a JSON dump).
- **Trimmed:** trimmed view excludes `scoring` / `search_metadata`; structured includes them.
- **Enrichment:** readable fields present end-to-end (store query → server response → mcp
  output). Integration-gated (CI) for the store/server legs.
- **Telemetry:** `McpToolCall` round-trips the new optional fields; canary suite covers them;
  sweep dimensional rollup populates `telemetry_search_daily` and survives the `DELETE`
  (integration, CI).
- **Rewrite** existing assertions in `tools_dispatch.rs` / `server_loop.rs` that expect the
  old raw-string dump.

## Files touched (by crate)

- `mn-store` — search/chunk/document queries + row structs; migration for
  `telemetry_search_daily`.
- `mn-retrieval` — result type carries readable identity.
- `mn-server` — response bodies for search/chunks/documents; `openapi.yaml`; `sweep_once`
  dimensional rollup; `config.rs` retention default 7 → 90; optional `metrics.rs` rows.
- `mn-mcp` — `protocol.rs` (`structured_content`, `output_schema`); `tools.rs`
  (`ToolOutcome` / `ToolFailure`, per-tool projection / summary / schema); `server.rs`
  (adapter + `ToolFailure` builders); tests.
- `mn-telemetry` — `events.rs` additive `McpToolCall` fields; regenerated
  `telemetry-events.json`; canary suite coverage.

## Risks / trade-offs

- **Redundant presentation:** a client that feeds *both* `content` and `structuredContent` to
  the model sees the trimmed and full views together. Mitigated by keeping `trimmed` lean; the
  back-compat duplication was the explicit goal.
- **Normalized contract coupling:** `mn-mcp` now transforms cloud payloads, so a cloud wire
  change requires updating the projection. Accepted in exchange for an mn-mcp-owned schema.
- **90-day raw retention** grows `telemetry_event_raw` ~13× vs. 7 days; volumes are small at
  v1 scale, but worth watching.
- **`top_source` cardinality:** bounded by the source catalog (~tens), so the dimensional
  table stays small.

## Out of scope

- `inputSchema` changes / caller verbosity toggle.
- Corpus re-ingest or corpus-table migration.
- Changing the privacy / client-opt-out telemetry model.
- Server-side always-on telemetry emission.

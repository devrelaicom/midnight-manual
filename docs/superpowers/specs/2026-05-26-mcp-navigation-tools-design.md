# MCP navigation tools — design

**Date:** 2026-05-26
**Status:** draft
**Touches:** `mn-mcp` (5 new tools + 1 deletion + 1 description refresh + new
typed error), `mn-telemetry` (5 new `McpToolName` variants + 1 deletion).

Companion to the CLI-side change in
[2026-05-25-chunk-document-navigation-design.md](2026-05-25-chunk-document-navigation-design.md).
The CLI grew six new navigation verbs in PR #51 and the underlying cloud
routes shifted at the same time. The MCP server didn't keep up, leaving a
broken tool and a five-tool coverage gap for AI agents.

## Problem

Two issues, addressed together:

1. **Regression.** The MCP tool `get_chunk_siblings` calls
   `GET /v1/chunks/:id/siblings`. PR #51 deleted that route. The tool will
   404 in production once the next mn-mcp release ships against the
   updated cloud.
2. **Coverage gap.** PR #51 also added five new cloud read endpoints —
   `/v1/chunks/:id/{next,prev}` and `/v1/documents/:id{,/full,/chunks}` —
   wrapped in the CLI as `mnm chunks {show,next,prev}` and
   `mnm documents {show,full,chunks}`. None of them are reachable from an
   AI agent. The agent surface has fallen behind the operator surface.

A third, smaller issue: `get_chunk`'s tool description is now stale.
`/v1/chunks/:id` was augmented to return a `document` sub-object and a
`source` sub-object, but the description still advertises "full metadata,
parent chain, and navigation pointers" — accurate at the time, misleading
now (parents are a separate tool; navigation pointers are not embedded).

## Scope

**In:**

- Delete the `get_chunk_siblings` tool, its cloud-client method, and its
  `PassthroughKind` variant.
- Add five new tools: `get_chunk_next`, `get_chunk_prev`, `get_document`,
  `get_document_full`, `get_document_chunks`.
- Refresh `get_chunk`'s description to match the augmented response
  shape.
- Update `tools.rs` module-level doc comment.
- Surface the cloud's 412 `too_many_chunks` body as a typed JSON-RPC
  error mirroring the existing `embedding_model_mismatch` precedent
  (structured `data` field with `next_tool` for the agent).
- Update `McpToolName`: remove `GetChunkSiblings`, add 5 variants.
- Update `tool_name_for_event` dispatch table.
- Tests: rewrite the broken siblings test, add positive + error-path
  tests for each new tool, update the 7-tool count assertion.

**Out (deferred):**

- A `get_chunk_neighbors` convenience tool that bundles
  `prev + current + next` in one call. The CLI deferred its sibling
  (`mnm chunks neighbors`) for the same reason: trivially composable
  from three calls. Defer here too for surface-parity.
- A `tools/list` schema-contract test against `contracts/mcp-tools.json`
  on disk. The contract document under `specs/001-rag-platform/contracts/`
  is intentionally out of date relative to the live tools; updating it is
  a docs concern, not a runtime one.

**Non-goals:**

- Backwards compatibility on `get_chunk_siblings`. The route is gone; the
  tool follows it. No deprecation window — this software is unreleased and
  no agent in production depends on the tool yet.
- Changing the cloud surface. All five new endpoints already exist; this
  spec is a pure wrap-and-route job in `mn-mcp`.

## §1 — Tool surface (final)

After this change the MCP server exposes **11 tools** (was 7).

```text
LOCAL (no cloud round-trip):
  status
  pull_models

CLOUD (search + per-chunk reads):
  search
  get_chunk
  get_chunk_next         -- NEW
  get_chunk_prev         -- NEW
  get_chunk_parents

CLOUD (document reads):
  get_document           -- NEW
  get_document_full      -- NEW
  get_document_chunks    -- NEW

CLOUD (catalog):
  list_sources

DELETED: get_chunk_siblings
```

## §2 — Tool descriptions and input schemas

The `tools/list` response carries one entry per tool. The description is
what an AI agent sees when deciding whether to call the tool — keep them
specific to behavior, not vague gestures at purpose.

### 2.1 `get_chunk` (description refresh)

```text
description:
"Fetch one chunk by id. Returns the chunk row (id, content, chunk_index,
total_chunks, content_hash, embedding_model_id, heading_path, symbol_path,
start_byte, end_byte, token_count, status, created_at, document_id,
source_version_id, node_id) plus a small `document` sub-object
(id, source_path, published_url, source_url, language, kind, provenance)
and a `source` sub-object (slug). For the chunk's parent chain call
get_chunk_parents; for adjacent chunks call get_chunk_next/get_chunk_prev."

input_schema: { type: object, required: [id],
                properties: { id: { type: string, format: uuid } },
                additionalProperties: false }
```

### 2.2 `get_chunk_next` (NEW)

```text
description:
"Fetch up to `count` chunks immediately following the given chunk in
chunk_index order, scoped to the same document. Returns
`{chunks: ChunkWithContext[]}` sorted ascending. Returns `{chunks: []}`
(not 404) when called on the last chunk. `embed_failed` chunks are skipped,
so the returned chunk_index sequence may have gaps. count defaults to 5 and
must be in [1, 100]; out-of-range values are rejected as InvalidParams
before the call reaches the cloud."

input_schema: { type: object, required: [id],
                properties: {
                  id: { type: string, format: uuid },
                  count: { type: integer, minimum: 1, maximum: 100, default: 5 }
                },
                additionalProperties: false }
```

### 2.3 `get_chunk_prev` (NEW)

```text
description:
"Fetch up to `count` chunks immediately preceding the given chunk in
chunk_index order, scoped to the same document. Returns
`{chunks: ChunkWithContext[]}` sorted ascending (reading order). Returns
`{chunks: []}` (not 404) when called on the first chunk. `embed_failed`
chunks are skipped, so the returned chunk_index sequence may have gaps.
count defaults to 5 and must be in [1, 100]; out-of-range values are
rejected as InvalidParams before the call reaches the cloud."

input_schema: <same as get_chunk_next>
```

### 2.4 `get_document` (NEW)

```text
description:
"Document overview: metadata (id, source_version_id, node_id, source_path,
published_url, source_url, language, kind, content_hash, char_count,
token_count, source_modified_at, created_at, frontmatter, provenance,
package_id), the source `{slug}`, and an ordered `chunk_ids` array of every
ready chunk. No chunk bodies. Use get_document_full for inline bodies or
get_document_chunks for a windowed slice."

input_schema: { type: object, required: [id],
                properties: { id: { type: string, format: uuid } },
                additionalProperties: false }
```

### 2.5 `get_document_full` (NEW)

```text
description:
"Complete document: every overview field except chunk_ids, plus a `chunks`
array with each chunk's `{chunk_id, chunk_index, content, heading_path,
token_count}` inline (no document/source sub-objects per chunk; the parent
document fields are at the top level). Capped at 500 ready chunks. For
documents over the cap the call fails with a structured `too_many_chunks`
error (see error.data.next_tool); fall back to get_document_chunks."

input_schema: { type: object, required: [id],
                properties: { id: { type: string, format: uuid } },
                additionalProperties: false }
```

### 2.6 `get_document_chunks` (NEW)

```text
description:
"Position-windowed chunk slice of a document. Returns
`{chunks: ChunkBody[], from, limit, total_chunks}`. from defaults to 0
(must be >= 0); limit defaults to 20 and must be in [1, 100]. Out-of-range
values are rejected as InvalidParams before the call reaches the cloud.
`from` past the end returns `chunks: []` with accurate `total_chunks`
(not 404). Use to page through documents larger than get_document_full's
500-chunk cap or to read a known offset."

input_schema: { type: object, required: [id],
                properties: {
                  id: { type: string, format: uuid },
                  from: { type: integer, minimum: 0, default: 0 },
                  limit: { type: integer, minimum: 1, maximum: 100, default: 20 }
                },
                additionalProperties: false }
```

JSON Schema usage notes:

- `count`, `from`, `limit` use `integer` (not `number`) to reject floats at
  the agent's call site.
- `additionalProperties: false` matches the existing convention on
  `get_chunk` / `list_sources`.
- The schema's bounds (e.g. `count` max 100) and the server-side clamps
  agree. Even so, the client-side dispatcher *also* validates the values
  before calling the cloud — silent server clamping is fine for genuine
  out-of-range queries from a typo, but a malformed type (`count: "five"`)
  needs to surface as `InvalidParams` immediately.

## §3 — Response shapes

All responses are pass-through `serde_json::Value` from the corresponding
cloud route. The shapes are documented in full in the CLI spec's §1
(`/v1/chunks/:id/{,next,prev}`, `/v1/documents/:id{,/full,/chunks}`). The
MCP tools do not transform the bodies — same pattern as `get_chunk` and
`get_chunk_parents` today.

The one exception: when the cloud returns `412 too_many_chunks` from
`/v1/documents/:id/full`, the MCP layer translates the body into a typed
JSON-RPC error (see §5).

## §4 — Dispatch and cloud-client structure

### 4.1 `cloud_client.rs`

```rust
// Remove.
// async fn get_chunk_siblings(&self, id: &str) -> Result<Value, CloudError>;

// New (id-only; share get_json plumbing).
async fn get_document(&self, id: &str) -> Result<Value, CloudError>;
async fn get_document_full(&self, id: &str) -> Result<Value, CloudError>;

// New (id + count). Encodes ?count=N into the URL.
async fn get_chunk_next(&self, id: &str, count: u32) -> Result<Value, CloudError>;
async fn get_chunk_prev(&self, id: &str, count: u32) -> Result<Value, CloudError>;

// New (id + from + limit). Encodes ?from=K&limit=N into the URL.
async fn get_document_chunks(
    &self,
    id: &str,
    from: u32,
    limit: u32,
) -> Result<Value, CloudError>;
```

`get_document_full` cannot reuse `get_json` verbatim because it must
detect `412 Precondition Failed` and translate the body. Either:

- (a) inline the GET in `get_document_full` and call a new helper
  `parse_too_many_chunks(&body) -> Option<CloudError>` analogous to
  `parse_mismatch`, or
- (b) lift `get_json` into a more general `get_json_with_typed_errors(path,
  &[StatusTranslator])` form.

Choose **(a)** — `get_json_with_typed_errors` would over-generalize when
only two endpoints in the whole client need typed status mapping (search's
409 and document-full's 412). One inline GET per typed-error endpoint is
the simpler pattern.

The new `CloudError` variant:

```rust
/// 412 from /v1/documents/:id/full — document exceeds the chunk cap.
/// Surfaced specially so the MCP layer can emit a typed JSON-RPC error
/// pointing the caller at get_document_chunks.
#[error("document too many chunks: {chunk_count} (cap {cap})")]
TooManyChunks {
    /// Reported ready-chunk count for the document.
    chunk_count: u32,
    /// Server's configured cap (currently 500).
    cap: u32,
    /// Operator-facing hint from the cloud (path to the windowing endpoint).
    hint: String,
},
```

Parser:

```rust
fn parse_too_many_chunks(body: &[u8]) -> Option<CloudError> {
    let v: Value = serde_json::from_slice(body).ok()?;
    if v.get("error")?.as_str()? != "too_many_chunks" { return None; }
    let chunk_count = v.get("chunk_count")?.as_u64()?.try_into().ok()?;
    let cap         = v.get("cap")?.as_u64()?.try_into().ok()?;
    let hint        = v.get("hint")?.as_str().unwrap_or("").to_owned();
    Some(CloudError::TooManyChunks { chunk_count, cap, hint })
}
```

### 4.2 `tools.rs`

```rust
pub enum PassthroughKind {
    Chunk,
    Parents,
    Document,       // NEW
    DocumentFull,   // NEW
    // Siblings REMOVED
}
```

`run_passthrough_id` is unchanged in shape — it covers the four id-only
GETs. The match in the helper adds two arms (Document, DocumentFull) and
loses one (Siblings). When the cloud client returns the new
`CloudError::TooManyChunks { .. }` from `get_document_full`, the helper
translates it into a new `PassthroughError::TooManyChunks { .. }` variant
which the server layer renders as the typed JSON-RPC error (§5).

Two new helpers for the parameterized tools:

```rust
pub enum ChunkNavDirection { Next, Prev }

pub async fn run_chunk_nav(
    args: &Value,
    cloud: &Arc<CloudClient>,
    dir: ChunkNavDirection,
) -> Result<Value, PassthroughError>;

pub async fn run_document_chunks(
    args: &Value,
    cloud: &Arc<CloudClient>,
) -> Result<Value, PassthroughError>;
```

Both parse `id` exactly like `run_passthrough_id`, then layer on:

- `run_chunk_nav` — `count` (optional, default 5, range [1, 100]).
  Out-of-range values are rejected as `InvalidInput`, *not* silently
  clamped. The server clamps too; the client clamp is a defensive double-up
  so a typo surfaces as `InvalidParams` immediately rather than producing
  a quietly-different result set.
- `run_document_chunks` — `from` (optional, default 0, range `>= 0`) and
  `limit` (optional, default 20, range [1, 100]). Same rejection policy.

### 4.3 `server.rs`

```rust
fn tool_name_for_event(name: &str) -> Option<McpToolName> {
    match name {
        "search"               => Some(McpToolName::Search),
        "get_chunk"            => Some(McpToolName::GetChunk),
        "get_chunk_next"       => Some(McpToolName::GetChunkNext),      // NEW
        "get_chunk_prev"       => Some(McpToolName::GetChunkPrev),      // NEW
        "get_chunk_parents"    => Some(McpToolName::GetChunkParents),
        "get_document"         => Some(McpToolName::GetDocument),       // NEW
        "get_document_full"    => Some(McpToolName::GetDocumentFull),   // NEW
        "get_document_chunks"  => Some(McpToolName::GetDocumentChunks), // NEW
        "list_sources"         => Some(McpToolName::ListSources),
        "pull_models"          => Some(McpToolName::PullModels),
        "status"               => Some(McpToolName::Status),
        // GetChunkSiblings — REMOVED
        _ => None,
    }
}
```

`dispatch_tool_inner` gains five arms; the `get_chunk_siblings` arm is
deleted. Three of the new arms (`get_document`, `get_document_full`) go
through `run_passthrough_dispatch` with their new `PassthroughKind`; two
(`get_chunk_next`, `get_chunk_prev`) call a new
`run_chunk_nav_dispatch(direction)`; one (`get_document_chunks`) calls
`run_document_chunks_dispatch`. The two new dispatch helpers follow
exactly the shape of `run_passthrough_dispatch` — invoke the tool, map the
typed error to a `Response`.

## §5 — Error mapping

| Cloud / parse outcome | MCP error code | Body / data |
|---|---|---|
| 200 OK | (success) — `ToolCallResult` with body as text | n/a |
| `InvalidInput` (uuid parse, range check, missing field) | `InvalidParams` | message only |
| `CloudError::NotFound` (404) | `ToolFailed` | `"not found: <body>"` |
| `CloudError::TooManyChunks` (412, document-full only) | `ToolFailed` | typed `data` — see below |
| `CloudError::Transport`, `Decode`, `Status (5xx)` | `ToolFailed` | error string only |

Typed 412 envelope, mirroring `mismatch_response`:

```rust
fn too_many_chunks_response(
    id: RequestId,
    chunk_count: u32,
    cap: u32,
    hint: &str,
) -> Response {
    let data = json!({
        "kind": "too_many_chunks",
        "chunk_count": chunk_count,
        "cap": cap,
        "hint": hint,
        "next_tool": "get_document_chunks",
    });
    Response {
        jsonrpc: JSONRPC,
        id,
        result: None,
        error: Some(JsonRpcError {
            code: ErrorCode::ToolFailed as i32,
            message: format!("document has {chunk_count} chunks (cap {cap})"),
            data: Some(data),
        }),
    }
}
```

Agents that recognize the `data.next_tool` convention (already established
by the embedding-mismatch path on `search`) get a one-hop remediation:
re-issue against `get_document_chunks` with `from=0, limit=<= 100`.
Agents that don't can still read the human-readable `message`.

## §6 — Telemetry

`mn-telemetry::events::McpToolName`:

```rust
pub enum McpToolName {
    Search,
    GetChunk,
    GetChunkNext,        // NEW
    GetChunkPrev,        // NEW
    GetChunkParents,
    GetDocument,         // NEW
    GetDocumentFull,     // NEW
    GetDocumentChunks,   // NEW
    ListSources,
    PullModels,
    Status,
    // GetChunkSiblings — REMOVED
}
```

The DB `CHECK` constraint on `telemetry_event_raw.event_type` covers
event-type strings only (`mcp_tool_call`, etc.) — `tool_name` is a JSON
field with no schema-level constraint, so no migration is needed. The
server-side telemetry validator in
`crates/mn-server/src/routes/telemetry.rs` similarly only validates
`event_type` and `component`, not nested `tool_name` values.

`telemetry_canary.rs` only exercises `Search`, so the canary tests need
no change.

## §7 — Tests

### 7.1 Updates to existing tests

- `crates/mn-mcp/src/tools.rs::tests::tool_list_has_all_seven_tools` →
  rename to `tool_list_has_all_eleven_tools`. Update the expected name
  list. Assert the count matches.
- `crates/mn-mcp/tests/tools_dispatch.rs::run_passthrough_id_maps_404` —
  the existing test uses `PassthroughKind::Siblings`. Repoint it at
  `PassthroughKind::DocumentFull` (or another retained kind) so the 404
  coverage survives.

### 7.2 New unit + dispatch tests

In `crates/mn-mcp/tests/tools_dispatch.rs`:

- `run_passthrough_id_hits_document_endpoint` — GET `/v1/documents/:id`,
  assert body round-trips.
- `run_passthrough_id_hits_document_full_endpoint` — GET
  `/v1/documents/:id/full`, success path.
- `run_passthrough_id_maps_document_full_412` — mock the cloud's 412 body,
  assert `PassthroughError::TooManyChunks { chunk_count, cap, hint }`.
- `run_chunk_nav_uses_count_query_param` — assert `?count=7` makes it to
  the wire.
- `run_chunk_nav_defaults_count_to_five` — omit count, assert `?count=5`.
- `run_chunk_nav_rejects_out_of_range_count` — count `0`, `101`,
  `"five"` — all `InvalidInput`.
- `run_chunk_nav_routes_by_direction` — `Next` hits `/next`, `Prev` hits
  `/prev`.
- `run_document_chunks_uses_from_and_limit` — assert `?from=3&limit=7` on
  the wire.
- `run_document_chunks_defaults_from_zero_limit_twenty` — omit both.
- `run_document_chunks_rejects_negative_from` — `from: -1` →
  `InvalidInput`.
- `run_document_chunks_rejects_limit_out_of_range` — `0`, `101`.

In `crates/mn-mcp/src/cloud_client.rs::tests`:

- `parse_too_many_chunks_extracts_count_and_cap` — happy path on a typical
  cloud body.
- `parse_too_many_chunks_returns_none_for_unrelated_412` — body without
  the `too_many_chunks` shape returns `None` (caller falls through to
  `CloudError::Status`).

### 7.3 End-to-end server-loop test

In `crates/mn-mcp/tests/server_loop.rs`:

- Add a `tools/list` test that asserts exactly the 11 expected names
  (catches any future drift in either direction).
- Add a `tools/call` test for `get_document_full` against a wiremocked 412.
  Read the JSON-RPC error body; assert `error.code == ToolFailed`,
  `error.data.kind == "too_many_chunks"`,
  `error.data.next_tool == "get_document_chunks"`,
  `error.data.chunk_count == 1240`, `error.data.cap == 500`.

Existing `server_loop.rs` tests cover the happy `tools/list` + `tools/call`
shape; the new ones just extend that pattern.

## §8 — Crate layout / file delta

```text
MODIFIED:
  crates/mn-mcp/src/cloud_client.rs
    - get_chunk_siblings method                    DELETED
    + get_document / get_document_full              NEW (id-only)
    + get_chunk_next / get_chunk_prev               NEW (+ ?count)
    + get_document_chunks                           NEW (+ ?from + ?limit)
    + CloudError::TooManyChunks variant             NEW
    + parse_too_many_chunks helper                  NEW
    + inline GET in get_document_full               NEW
    + parse_too_many_chunks_* tests                 NEW

  crates/mn-mcp/src/tools.rs
    - PassthroughKind::Siblings variant            DELETED
    + PassthroughKind::Document / DocumentFull      NEW
    + PassthroughError::TooManyChunks variant       NEW
    + ChunkNavDirection enum                        NEW
    + run_chunk_nav, run_document_chunks helpers    NEW
    * get_chunk description string                  REFRESHED
    * tool list ordering / new entries              UPDATED
    * tool_list_has_all_*_tools test                UPDATED

  crates/mn-mcp/src/server.rs
    * tool_name_for_event match                     UPDATED
    * dispatch_tool_inner match                     UPDATED
    + run_chunk_nav_dispatch helper                 NEW
    + run_document_chunks_dispatch helper           NEW
    + too_many_chunks_response builder              NEW

  crates/mn-mcp/tests/tools_dispatch.rs
    * existing siblings 404 test                    REPOINTED
    + ~10 new tests across nav + window + 412       NEW

  crates/mn-mcp/tests/server_loop.rs
    + tools/list count + names test                 NEW
    + tools/call get_document_full 412 → typed err  NEW

  crates/mn-telemetry/src/events.rs
    - McpToolName::GetChunkSiblings                 DELETED
    + 5 new McpToolName variants                    NEW

UNCHANGED (intentionally):
  crates/mn-mcp/src/protocol.rs       (JSON-RPC framing)
  crates/mn-mcp/src/transport.rs      (stdio frame reader/writer)
  crates/mn-mcp/src/lib.rs            (only re-exports)
  crates/mn-server/*                  (cloud routes already shipped)
  crates/mn-store/*                   (no schema change)
  contracts/mcp-tools.json            (not consumed at runtime; refresh deferred)
```

No new third-party deps.

## §9 — Open follow-ups

These are explicitly out of scope for this spec but worth recording:

- **`get_chunk_neighbors` convenience tool.** Bundles
  prev + current + next in one call. Useful when an agent wants context
  around a search hit and a single round-trip matters. Deferred for
  parity with the CLI's deferred `mnm chunks neighbors`. Revisit if
  agent-side telemetry shows a tight `get_chunk_prev → get_chunk →
  get_chunk_next` triple becoming a common shape.
- **Contract file refresh.** `specs/001-rag-platform/contracts/mcp-tools.json`
  may be stale relative to the live `tools/list` response after this
  change. The runtime never reads the contract file; the gap is a docs
  problem and lands separately.
- **Schema generation from a single source.** The descriptions and
  schemas now live in three places per tool: the live `tools/list`
  output (`tools.rs`), the contract file, and this spec. A future cleanup
  could derive all three from one definition. Not now.

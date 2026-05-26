# Chunk + document navigation surface — design

**Date:** 2026-05-25
**Status:** approved
**Touches:** `mn-server` (3 new routes + 2 new handlers + 1 deletion), `mn-store`
(new chunk/document helpers + 1 deletion), `mn-cli` (new `chunks` and
`documents` top-level namespaces + 6 new commands), `mn-telemetry` (new
`CliCommandName` variants).

## Problem

The deployed server has no observable surface for inspecting ingested content
beyond `mnm search`. When a search result returns a chunk, the operator
cannot:

- See the chunk's full content and bundled document context (right now
  `GET /v1/chunks/:id` returns the bare chunk row — no `published_url`, no
  `source.slug`, no `document.source_path` — so even validating the
  `published_url` survives ingest requires a second hop the CLI doesn't
  wrap).
- Walk to the next or previous chunks in the same document to read context
  around a hit.
- Look at a document's overview to see what chunks it contains.
- Pull a complete document's chunks in one call to read it linearly.
- Page through a long document at a chosen offset.

The existing `GET /v1/chunks/:id/siblings` is the closest thing — but it
returns the document's *entire* chunk set unbounded, which is the wrong
shape for both "show me the next 5 chunks" and "page through this
1000-chunk document."

There is no CLI surface for any of this. Today the operator has to
`curl` raw HTTP endpoints, deserialize JSON in their head, and
cross-reference chunk IDs by hand.

Relevant requirements: US4 acceptance #3–5 (chunk context retrieval),
FR-058 (CLI ad-hoc retrieval), FR-117 (CLI read path symmetry), D17
(server-URL precedence — reused), D23 (admin-visibility — these are
non-admin reads, always visible).

## Scope

**In:**

- `GET /v1/chunks/:id` augmented to bundle `document` and `source`
  sub-objects (one extra indexed DB read per request).
- `GET /v1/chunks/:id/next?count=N` and `/prev?count=N` — chunk-anchored
  navigation, returning up to N chunks, empty array (not 404) when at the
  boundary.
- `GET /v1/documents/:id` — document overview: metadata + ordered
  `chunk_ids` array. No chunk bodies.
- `GET /v1/documents/:id/full` — complete document: metadata + every
  chunk inline. Capped at 500 chunks; > 500 returns
  `412 too_many_chunks` with a hint pointing at the window endpoint.
- `GET /v1/documents/:id/chunks?from=K&limit=N` — position-based window,
  defaults `from=0, limit=20`.
- `GET /v1/chunks/:id/parents` unchanged.
- **Deleted:** `GET /v1/chunks/:id/siblings` route + `chunk::list_siblings`
  store helper + the route's integration tests.
- New top-level CLI namespaces (always visible, like `mnm search`):
  - `mnm chunks {show, next, prev} <chunk-id>` (`--count N=5` on
    next/prev; `--full` on next/prev to disable 240-char preview
    truncation).
  - `mnm documents {show, full, chunks} <doc-id>` (`--from K=0
    --limit N=20` on `chunks`).
  - All commands support `--json` (raw server response, no transformation).
- Tests: unit on store helpers (testcontainers Postgres); two new
  `mn-server` integration tests (`chunks_navigation.rs`,
  `documents_full_cap.rs`); two new `mn-cli` wiremock smoke tests
  (`chunks_cli.rs`, `documents_cli.rs`); removal of the existing
  siblings integration test.
- `CliCommandName::Chunks` and `CliCommandName::Documents` added to the
  telemetry enum.

**Out (deferred):**

- A `mnm chunks neighbors` convenience verb (`prev` + `show` + `next`
  composed). Trivial to script if anyone wants it.
- Heading-outline view of a document (the design used a per-chunk
  `heading_path` already on each chunk, so a separate outline endpoint
  is not justified yet).
- Search-result → chunk navigation shortcut (e.g. piping search output
  into `mnm chunks next`). Operators can copy chunk-ids by hand for v1.
- Pagination cursor on long-document windowing (offset/limit is fine for
  the corpus sizes v1 targets; cursor pagination lands when a corpus
  hits ≥ 100k chunks).
- Document filtering on the overview (e.g. "show me documents touched in
  this ingest run"). That's a different concern.

**Non-goals:**

- Backwards compatibility on `/v1/chunks/:id` or `/v1/chunks/:id/siblings`.
  This software is unreleased; breaking changes are free. The augmented
  `/v1/chunks/:id` response shape is the only shape; siblings is removed.
- Auth-tiered visibility. All navigation endpoints are public reads; the
  bearer only affects rate-limit tier.

## Endpoint surface (final)

```
GET  /v1/chunks/:id                  -- augmented (see §1)
GET  /v1/chunks/:id/next?count=N     -- next N chunks (default 5; clamp [1,100])
GET  /v1/chunks/:id/prev?count=N     -- prev N chunks (default 5; clamp [1,100])
GET  /v1/chunks/:id/parents          -- unchanged

GET  /v1/documents/:id               -- overview
GET  /v1/documents/:id/full          -- complete (capped 500; 412 above)
GET  /v1/documents/:id/chunks?from=K&limit=N
                                     -- window (defaults from=0, limit=20;
                                     --   clamp limit [1,100], from >= 0)

-- DELETED: /v1/chunks/:id/siblings
```

All routes mount via the existing `crates/mn-server/src/routes/` pattern:
`chunks.rs` is updated; `documents.rs` is new; both register their routers
in `app.rs`.

## §1 — Response shapes

### 1.1 `GET /v1/chunks/:id` (augmented)

```json
{
  "id": "uuid",
  "source_version_id": "uuid",
  "document_id": "uuid",
  "node_id": "uuid",
  "chunk_index": 3,
  "total_chunks": 12,
  "content": "...",
  "content_hash": "sha256:...",
  "embedding_model_id": "uuid",
  "heading_path": ["Welcome", "What this file is for"],
  "symbol_path": [],
  "start_byte": 240,
  "end_byte": 612,
  "token_count": 42,
  "status": "ready",
  "created_at": "2026-05-25T...",
  "document": {
    "id": "uuid",
    "source_path": "welcome.md",
    "published_url": "https://example.invalid/sample/welcome/",
    "source_url": null,
    "language": "markdown",
    "kind": "markdown",
    "provenance": { "attribution": "foundation", "verified": true }
  },
  "source": { "slug": "sample" }
}
```

All existing chunk fields are preserved on the wire. The route now returns a
new `ChunkWithContext` struct (defined in §2) — a wrapper around the existing
`Chunk` plus the new `document` and `source` sub-objects — serialized
flat at the top level via `#[serde(flatten)]` so the original 16 chunk
fields stay at the response root. The `document` sub-object is a
deliberate subset of the document row, not the full 16 fields — keep it
small so the chunk endpoint stays cheap. The `source` sub-object carries
only `slug` for now; expand if and when a real use case appears.

### 1.2 `GET /v1/chunks/:id/next?count=N` and `/prev?count=N`

```json
{ "chunks": [ /* array of ChunkWithContext, sorted by chunk_index asc */ ] }
```

Returns up to `count` chunks. Boundary cases return `{"chunks": []}` with
`200`, not `404`. `count` clamped to `[1, 100]` (default `5`).
`embed_failed` chunks are skipped — `next`/`prev` walk in `chunk_index`
order over `ready` chunks only, matching the existing `get_by_id_ready`
semantics.

### 1.3 `GET /v1/documents/:id` (overview)

```json
{
  "id": "uuid",
  "source_version_id": "uuid",
  "node_id": "uuid",
  "source_path": "welcome.md",
  "published_url": "https://example.invalid/sample/welcome/",
  "source_url": null,
  "language": "markdown",
  "kind": "markdown",
  "content_hash": "sha256:...",
  "char_count": 1024,
  "token_count": 180,
  "source_modified_at": "2026-04-12T...",
  "created_at": "2026-05-25T...",
  "frontmatter": { ... },
  "provenance": { ... },
  "package_id": null,
  "source": { "slug": "sample" },
  "chunk_ids": ["uuid-0", "uuid-1", "uuid-2", "uuid-3"]
}
```

`chunk_ids` is ordered by `chunk_index` ascending and includes only
`ready` chunks (consistent with the rest of the read surface).

### 1.4 `GET /v1/documents/:id/full` (complete)

Same fields as overview, with `chunk_ids` replaced by `chunks` (full
bodies):

```json
{
  /* all overview fields except chunk_ids */
  "chunks": [
    {
      "chunk_id": "uuid",
      "chunk_index": 0,
      "content": "...",
      "heading_path": ["..."],
      "token_count": 42
    },
    ...
  ]
}
```

When the document has > 500 ready chunks, the server returns:

```
HTTP/1.1 412 Precondition Failed
Content-Type: application/json

{ "error": "too_many_chunks",
  "chunk_count": 1240,
  "cap": 500,
  "hint": "Use GET /v1/documents/:id/chunks?from=K&limit=L (default L=20)" }
```

The 500-chunk cap is a constant in `mn-server`, exposed as
`pub const DOCUMENT_FULL_CHUNK_CAP: usize = 500;` so the CLI can render
the hint consistently if needed (it just echoes the server's body).

### 1.5 `GET /v1/documents/:id/chunks?from=K&limit=N` (window)

```json
{
  "chunks": [ /* per-chunk shape from §1.4 */ ],
  "from": K,
  "limit": N,
  "total_chunks": M
}
```

`from` defaults to `0`, must be `>= 0`. `limit` defaults to `20`, clamped
to `[1, 100]`. Out-of-range `from` (≥ `total_chunks`) returns
`200` with `{"chunks": []}` and accurate `total_chunks` so the CLI can
render `none in range`.

## §2 — Store-side helpers

All new helpers live in `crates/mn-store/src/entities/`:

**`chunk.rs`** — add:

- `pub async fn get_with_context(pool: &PgPool, id: Uuid) -> Result<ChunkWithContext>` — single chunk + joined document + source row (one query, JOINs).
- `pub async fn list_next(pool: &PgPool, anchor: Uuid, count: usize) -> Result<Vec<ChunkWithContext>>` — chunks with `chunk_index > anchor.chunk_index AND status = 'ready'` in same document, ordered, limit `count`.
- `pub async fn list_prev(pool: &PgPool, anchor: Uuid, count: usize) -> Result<Vec<ChunkWithContext>>` — chunks with `chunk_index < anchor.chunk_index AND status = 'ready'` in same document. SQL fetches `ORDER BY chunk_index DESC LIMIT count` to pick the `count` immediately preceding the anchor, then the helper reverses the result so callers receive chunks in ascending `chunk_index` (reading) order.
- **Delete:** `chunk::list_siblings`.

**`document.rs`** — add:

- `pub async fn get_overview(pool: &PgPool, id: Uuid) -> Result<DocumentOverview>` — document row + source.slug + `chunk_ids` (ordered, `ready` only).
- `pub async fn get_full(pool: &PgPool, id: Uuid, cap: usize) -> Result<Either<DocumentFull, ChunkCount>>` — document row + source.slug + every chunk (≤ cap). If `count_ready_chunks(doc) > cap`, returns the count for the 412 path without paying the full read.
- `pub async fn list_chunks_window(pool: &PgPool, id: Uuid, from: usize, limit: usize) -> Result<DocumentChunkWindow>` — document row (for source.slug) + chunks in `[from, from+limit)`.

Types:

```rust
pub struct ChunkWithContext {
    pub chunk: Chunk,                 // existing Chunk struct
    pub document: DocumentSummary,    // subset, see §1.1
    pub source_slug: String,
}

pub struct DocumentOverview {
    pub document: Document,           // existing Document struct
    pub source_slug: String,
    pub chunk_ids: Vec<Uuid>,
}

pub struct DocumentFull {
    pub document: Document,
    pub source_slug: String,
    pub chunks: Vec<ChunkBody>,       // chunk_id, chunk_index, content, heading_path, token_count
}

pub struct DocumentChunkWindow {
    pub document: Document,
    pub source_slug: String,
    pub chunks: Vec<ChunkBody>,
    pub from: usize,
    pub limit: usize,
    pub total_chunks: usize,
}
```

`DocumentSummary` is intentionally smaller than `Document` — only the
fields the chunk endpoint surfaces. Keep them as separate structs so
extending one doesn't auto-bloat the other.

`Either<DocumentFull, ChunkCount>` can be a private enum in `document.rs`;
caller (the route handler) maps the `ChunkCount` arm to the 412 response.

All helpers return `Result<_, StoreError>` matching the existing pattern.

## §3 — Route handlers

Files:

```
crates/mn-server/src/routes/
  chunks.rs           # updated: get_chunk returns ChunkWithContext;
                      #          get_next, get_prev added; get_siblings removed
  documents.rs        # NEW: get_document, get_document_full, get_document_chunks
  mod.rs              # register documents::router()
app.rs                # mount documents::router() alongside chunks::router()
```

Each handler:
1. Parses path/query params.
2. Calls the store helper.
3. Maps `StoreError::NotFound` → `404 not_found` (existing `error::not_found` helper).
4. Maps `DocumentFull`-cap-overflow → `412` with the hint body.
5. Returns `Json(payload)` on success.

The route module exports `pub const DOCUMENT_FULL_CHUNK_CAP: usize = 500;`.

## §4 — CLI surface

```
mnm chunks show <chunk-id>                   [--json]
mnm chunks next <chunk-id>  [--count N=5]    [--full] [--json]
mnm chunks prev <chunk-id>  [--count N=5]    [--full] [--json]

mnm documents show <doc-id>                  [--json]
mnm documents full <doc-id>                  [--json]
mnm documents chunks <doc-id> [--from K=0] [--limit N=20]   [--json]
```

**Verbs are always visible** (not under the D23 admin gate) — these are
read commands, mirroring `mnm search` / `mnm sources` / `mnm versions`.

**Crate placement:**

```
crates/mn-cli/src/commands/
  chunks/
    mod.rs            # dispatcher
    show.rs
    next.rs
    prev.rs
  documents/
    mod.rs            # dispatcher
    show.rs
    full.rs
    chunks.rs
```

Each verb is a thin clap-args + reqwest-get + render veneer. Shared
helpers (the bearer resolver, the server-URL resolver) come from
`crate::shared`. Auth via `resolve_best_bearer` (admin → read-uplift →
anonymous; same fallback as `mnm search`).

**Rendering (TTY):**

- `chunks show`: 2-line header (`chunk K/N — <source>/<path>` then
  `URL: <published_url>`), heading_path line (`heading: > A > B > C`),
  blank line, then full `content` body.
- `chunks next` / `prev`: numbered list; each entry is the 2-line header
  + heading_path + a 240-char preview of content (one paragraph, joined
  on whitespace). `--full` swaps the preview for the full body.
- `documents show`: metadata block (id, source/path, URL, language, kind,
  char_count, token_count, chunk count) + a numbered list of chunk_ids
  with chunk_index labels.
- `documents full`: metadata block + every chunk rendered like
  `chunks show` (full body).
- `documents chunks`: header `chunks K..K+N of M total`, then chunks
  rendered like `documents full`.

**Rendering (`--json`):** the server response is printed verbatim
(`println!("{}", serde_json::to_string(&body))`). No transformation. Same
pattern as `mnm search --json`.

**Error translation:** the 412 from `/v1/documents/:id/full` is decoded by
the CLI into a one-line message:

```
error: document has 1240 chunks (cap 500). Use:
  mnm documents chunks <doc-id> --from 0 --limit 100
```

Exit code 1.

## §5 — Error / edge cases

| Case | Behavior |
|---|---|
| `chunk-id` not found | `404 not_found` (existing error envelope) |
| `doc-id` not found | `404 not_found` |
| Chunk `status='embed_failed'` | Invisible on all endpoints (matches existing `get_by_id_ready`). `next`/`prev` skip over silently. Because `chunk_index` is assigned at ingest time and not renumbered, skipping leaves visible gaps in the `chunk_index` sequence (e.g. response may show indices `3, 5, 6` when chunk 4 failed embedding). Treat that as a feature — the gap is informative — and document it in CLI help text rather than hiding it. |
| `/next` on last chunk / `/prev` on first | `200 {"chunks": []}` — CLI renders `(no further chunks)` |
| Window with `from >= total_chunks` | `200` with empty `chunks`, accurate `total_chunks` — CLI renders `none in range` |
| `/full` on doc > 500 chunks | `412 too_many_chunks` (see §1.4). CLI translates per §4. |
| `?count=N` out of `[1,100]` | Clamped server-side; no error |
| `?limit=N` out of `[1,100]` | Clamped server-side; no error |
| `?from=K` negative | Rejected at clap parse (usize); URL-tampering returns `400 invalid_query_param` |
| Document under retired source | Readable. Retirement is write-side; reads stay open. |

## §6 — Tests

**Store-level (testcontainers Postgres):**

- `crates/mn-store/tests/chunk_navigation.rs` — insert a 5-chunk document
  + 1 `embed_failed` chunk + an adjacent document; assert `get_with_context`,
  `list_next(2)`, `list_prev(2)`, ordering correctness, embed_failed skip,
  cross-document isolation.
- `crates/mn-store/tests/document_navigation.rs` — same fixture; assert
  `get_overview` returns ordered `chunk_ids`, `get_full` returns chunks
  in order, `list_chunks_window` with various `from`/`limit` combinations
  including past-the-end.

**Route-level (testcontainers Postgres + axum test client):**

- `crates/mn-server/tests/chunks_navigation.rs` — full path: ingest the
  sample corpus fixture; assert each new chunk endpoint round-trips and
  the `document.published_url` is non-null.
- `crates/mn-server/tests/documents_navigation.rs` — same fixture; assert
  the three document endpoints; one test seeds 600 chunks and asserts
  `/full` returns 412 with the hint.

**CLI-level (wiremock):**

- `crates/mn-cli/tests/chunks_cli.rs` — for each verb (`show`, `next`,
  `prev`): mock the corresponding GET, run the binary, assert exit 0 and
  the rendered output contains the expected substrings.
- `crates/mn-cli/tests/documents_cli.rs` — same for `show`, `full`,
  `chunks`. Includes a test for the 412 translation: mock a 412 with the
  spec'd body, assert exit 1 and the CLI output names the chunk count
  and suggests the window command.

**Removed:** `crates/mn-server/tests/chunk_endpoints.rs` siblings test
(the route is deleted; the test goes with it).

## §7 — Crate layout

Net file delta:

```
NEW:
  crates/mn-store/src/entities/chunk.rs   # +get_with_context, +list_next, +list_prev
                                          # -list_siblings
  crates/mn-store/src/entities/document.rs # +get_overview, +get_full, +list_chunks_window
  crates/mn-store/tests/chunk_navigation.rs
  crates/mn-store/tests/document_navigation.rs
  crates/mn-server/src/routes/documents.rs # NEW handler module
  crates/mn-server/src/routes/mod.rs       # register documents
  crates/mn-server/tests/chunks_navigation.rs
  crates/mn-server/tests/documents_navigation.rs
  crates/mn-cli/src/commands/chunks/{mod,show,next,prev}.rs
  crates/mn-cli/src/commands/documents/{mod,show,full,chunks}.rs
  crates/mn-cli/tests/chunks_cli.rs
  crates/mn-cli/tests/documents_cli.rs

MODIFIED:
  crates/mn-server/src/routes/chunks.rs    # augment get_chunk; +get_next,+get_prev; -get_siblings
  crates/mn-server/src/app.rs              # mount documents::router()
  crates/mn-cli/src/commands/mod.rs        # pub mod chunks; pub mod documents
  crates/mn-cli/src/cli.rs                 # Command::{Chunks,Documents}; cli_command_name routing
  crates/mn-telemetry/src/events.rs        # CliCommandName::{Chunks,Documents}

DELETED:
  crates/mn-server/tests/chunk_endpoints.rs siblings test (the file may
                                          # have other tests — keep those)
```

No new third-party deps.

## §8 — Out of scope (recap)

- `mnm chunks neighbors` (compose `prev` + `show` + `next`).
- Standalone heading-outline endpoint (per-chunk `heading_path` already exists).
- Search-result → chunk navigation shortcut.
- Cursor pagination on `/v1/documents/:id/chunks`.
- Document filtering on the overview.

## §9 — Open follow-up

The augmented `/v1/chunks/:id` response carries a small `document`
sub-object. If a future feature needs additional document fields (e.g.
`sdk_dependencies`, `language_targets`), they get added there
incrementally. The split between "lean `document` subset on chunks" and
"full `document` row on `/v1/documents/:id`" is deliberate — keep them
asymmetric so the chunk endpoint stays small.

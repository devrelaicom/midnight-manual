# Chunk + Document Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the navigation surface specified in `docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md` — augmented `/v1/chunks/:id` that bundles document + source context, new `/v1/chunks/:id/{next,prev}`, new `/v1/documents/:id{,/full,/chunks}` family, and new `mnm chunks {show,next,prev}` + `mnm documents {show,full,chunks}` CLI namespaces. Replaces the unbounded `/v1/chunks/:id/siblings` outright (no deprecation — unreleased software).

**Architecture:** Three new store-side helpers per noun (`chunk` + `document`) backed by simple JOINs against the existing schema. Two route modules (one updated, one new) thin-wrap them. Two new top-level CLI namespaces follow the established `mnm sources` / `mnm versions` shape. All read endpoints are public; bearer affects rate-limit tier only.

**Tech Stack:** Rust 1.91 stable, `axum`, `sqlx` (Postgres), `clap` v4, `reqwest`, `serde_json`, `wiremock` (tests). No new third-party deps.

**Phasing:** Store helpers (Phase 1) land independently with their own tests. Route handlers (Phase 2) bolt onto the helpers. CLI namespaces (Phase 3) bolt onto the routes via HTTP. Phase 4 is the cleanup pass.

---

## Phase 1 — Store-side helpers

### Task 1: `ChunkWithContext` type + `chunk::get_with_context`

**Files:**
- Modify: `crates/mn-store/src/entities/chunk.rs` (add types + new fn)
- Test: inline `#[cfg(test)] mod tests` against testcontainers Postgres

- [ ] **Step 1: Add the response types**

In `crates/mn-store/src/entities/chunk.rs`, add (above the existing `Chunk` struct or alongside it):

```rust
/// Document subset bundled into chunk read responses. Intentionally smaller
/// than [`super::document::Document`] — only the fields useful for
/// navigation/inspection. Spec §1.1 of the chunk+document navigation design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSummary {
    pub id: Uuid,
    pub source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub kind: mn_core::types::DocumentKind,
    pub provenance: serde_json::Value,
}

/// Source subset bundled into chunk read responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSummary {
    pub slug: String,
}

/// Chunk + bundled document + source context returned by the navigation
/// read endpoints (`GET /v1/chunks/:id`, `/next`, `/prev`).
///
/// The existing chunk fields stay at the top level via `#[serde(flatten)]`
/// so callers that deserialize into a struct containing only chunk fields
/// still work (they ignore the extra `document` and `source` keys).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkWithContext {
    #[serde(flatten)]
    pub chunk: Chunk,
    pub document: DocumentSummary,
    pub source: SourceSummary,
}
```

The existing `Chunk` struct in this file is unchanged.

- [ ] **Step 2: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block (or create one if absent):

```rust
#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_with_context_returns_chunk_plus_document_and_source() {
    let h = crate::tests::common::boot().await;
    let fx = crate::tests::fixtures::ingest_minimal_two_chunk_doc(&h.pool, "with-context").await;

    let row = get_with_context(&h.pool, fx.chunk_ids[0]).await.unwrap();

    assert_eq!(row.chunk.id, fx.chunk_ids[0]);
    assert_eq!(row.document.id, fx.document_id);
    assert_eq!(row.document.source_path, "first.md");
    assert_eq!(row.document.published_url.as_deref(), Some("https://example.com/with-context/first/"));
    assert_eq!(row.source.slug, "with-context");
}
```

Note: `ingest_minimal_two_chunk_doc` is a new test fixture helper (added in step 4 below). If a `tests::common::boot` / `tests::fixtures` module path doesn't yet exist in `mn-store`, look at how the existing `mn-server/tests/common/mod.rs` is structured (an unconditional `common` module under `tests/`) and mirror that. The `mn-store` crate already has integration test infrastructure under `crates/mn-store/tests/`.

- [ ] **Step 3: Run the test, confirm it fails**

Run: `cargo test -p mn-store --features integration get_with_context_returns_chunk_plus_document_and_source -- --nocapture`
Expected: FAIL with "function not defined" (or compile error if fixture isn't there yet — that's also a failure signal; proceed to step 4).

- [ ] **Step 4: Add the fixture helper**

In `crates/mn-store/tests/fixtures.rs` (create if absent — declare with `mod fixtures;` from the integration test root):

```rust
//! Shared test fixtures for store-level integration tests.
use mn_store::entities::{chunk, document, source, source_version};
use sqlx::PgPool;
use uuid::Uuid;

pub struct MinimalDocFixture {
    pub source_id: Uuid,
    pub source_version_id: Uuid,
    pub document_id: Uuid,
    pub chunk_ids: Vec<Uuid>,
}

/// Insert a fresh source with one source_version + one document + two chunks.
/// The document's published_url is `https://example.com/<slug>/first/`.
///
/// Both chunks have status='ready' and share the same node_id (root).
pub async fn ingest_minimal_two_chunk_doc(pool: &PgPool, slug: &str) -> MinimalDocFixture {
    // Insert source + source_version + node + document + 2 chunks via the
    // existing entity helpers. Use mn_content's resolver / planner if it's
    // ergonomic, OR insert directly via the entity::* fns to keep the
    // fixture independent of mn-content. Pick whichever is fewer lines.
    //
    // The implementation must:
    //   - Create source { slug, kind: docs_site, retention_count: 5 }
    //   - Create source_version { revision: 1, is_active: true, embedding_model_id: <the seeded default> }
    //   - Create root node for the document
    //   - Insert document { source_path: "first.md", published_url: Some("https://example.com/<slug>/first/"), kind: Markdown, ... }
    //   - Insert 2 chunks with chunk_index 0 and 1, both status='ready'
    //   - Return the ids
    todo!("port the relevant insert helpers — see ingest_integration tests in mn-server for prior art")
}
```

The fixture body is intentionally not transcribed here — read `crates/mn-server/tests/common/` and any helpers there for how the project inserts source + version + document + chunk during tests. The fixture should reuse those patterns.

- [ ] **Step 5: Implement `get_with_context`**

In `crates/mn-store/src/entities/chunk.rs`, add:

```rust
/// Get a chunk plus its document + source context. One JOIN query.
///
/// # Errors
///
/// Returns `StoreError::NotFound` when no chunk exists with that id (or
/// its status is `embed_failed`). Returns `StoreError::Database` on any
/// SQL failure.
pub async fn get_with_context(pool: &PgPool, id: Uuid) -> Result<ChunkWithContext> {
    let row = sqlx::query_as::<_, ChunkWithContextRow>(
        "SELECT \
            c.id, c.source_version_id, c.document_id, c.node_id, c.chunk_index, c.total_chunks, \
            c.content, c.content_hash, c.embedding_model_id, c.heading_path, c.symbol_path, \
            c.start_byte, c.end_byte, c.token_count, c.status, c.created_at, \
            d.source_path AS d_source_path, d.published_url AS d_published_url, \
            d.source_url AS d_source_url, d.language AS d_language, d.kind AS d_kind, \
            d.provenance AS d_provenance, \
            s.slug AS s_slug \
         FROM chunk c \
         JOIN document d ON c.document_id = d.id \
         JOIN source_version sv ON c.source_version_id = sv.id \
         JOIN source s ON sv.source_id = s.id \
         WHERE c.id = $1 AND c.status <> 'embed_failed'",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(StoreError::NotFound)?;
    row.try_into()
}
```

Add the row-conversion type alongside (private):

```rust
#[derive(sqlx::FromRow)]
struct ChunkWithContextRow {
    // 16 chunk columns (same as ChunkRow):
    id: Uuid,
    source_version_id: Uuid,
    document_id: Uuid,
    node_id: Uuid,
    chunk_index: i32,
    total_chunks: i32,
    content: String,
    content_hash: String,
    embedding_model_id: Uuid,
    heading_path: Vec<String>,
    symbol_path: Vec<String>,
    start_byte: i32,
    end_byte: i32,
    token_count: i32,
    status: String,
    created_at: time::OffsetDateTime,
    // 6 document columns:
    d_source_path: String,
    d_published_url: Option<String>,
    d_source_url: Option<String>,
    d_language: Option<String>,
    d_kind: String,
    d_provenance: serde_json::Value,
    // 1 source column:
    s_slug: String,
}

impl TryFrom<ChunkWithContextRow> for ChunkWithContext {
    type Error = StoreError;
    fn try_from(r: ChunkWithContextRow) -> Result<Self> {
        // Mirror the existing ChunkRow → Chunk pattern: round-trip the
        // status string through serde_json so the enum's existing serde
        // impl handles the decode.
        let status: crate::entities::chunk::ChunkStatus =
            serde_json::from_value(serde_json::Value::String(r.status))
                .map_err(|e| StoreError::Json(e.to_string()))?;
        let doc_kind: mn_core::types::DocumentKind =
            serde_json::from_value(serde_json::Value::String(r.d_kind))
                .map_err(|e| StoreError::Json(e.to_string()))?;
        let chunk = Chunk {
            id: r.id,
            source_version_id: r.source_version_id,
            document_id: r.document_id,
            node_id: r.node_id,
            chunk_index: r.chunk_index,
            total_chunks: r.total_chunks,
            content: r.content,
            content_hash: r.content_hash,
            embedding_model_id: r.embedding_model_id,
            heading_path: r.heading_path,
            symbol_path: r.symbol_path,
            start_byte: r.start_byte,
            end_byte: r.end_byte,
            token_count: r.token_count,
            status,
            created_at: r.created_at,
        };
        Ok(Self {
            chunk,
            document: DocumentSummary {
                id: r.document_id,
                source_path: r.d_source_path,
                published_url: r.d_published_url,
                source_url: r.d_source_url,
                language: r.d_language,
                kind: doc_kind,
                provenance: r.d_provenance,
            },
            source: SourceSummary { slug: r.s_slug },
        })
    }
}
```

The pattern above matches the existing `ChunkRow::try_into` impl in `chunk.rs`. If `mn_core::types::DocumentKind` doesn't serialize-round-trip cleanly, look at the existing document.rs `DocumentRow::try_into` impl for the exact incantation it uses on `kind` and copy that.

- [ ] **Step 6: Run the test, confirm it passes**

Run: `cargo test -p mn-store --features integration get_with_context_returns_chunk_plus_document_and_source`
Expected: PASS.

- [ ] **Step 7: Run the full store suite**

Run: `cargo test -p mn-store --features integration`
Expected: every existing test still passes.

- [ ] **Step 8: Commit**

```bash
git add crates/mn-store/src/entities/chunk.rs crates/mn-store/tests/
git commit -m "feat(mn-store): chunk::get_with_context + ChunkWithContext type

Single-query helper returning a chunk plus its document and source
metadata as a flat-flattened struct. Feeds the augmented
/v1/chunks/:id route. Embed-failed chunks are invisible (matches
get_by_id_ready).

Spec: docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md §2"
```

### Task 2: `chunk::list_next` + `chunk::list_prev` (delete `list_siblings`)

**Files:**
- Modify: `crates/mn-store/src/entities/chunk.rs`
- Modify: existing call sites of `chunk::list_siblings` (the `chunks.rs` route's `get_siblings` will be removed in Task 6 — but a `cargo build` after this task will be broken until then; that's fine for a feature branch as long as each commit's tests pass for the crates that compile)

- [ ] **Step 1: Write the failing tests**

Add to `crates/mn-store/tests/chunk_navigation.rs` (new file):

```rust
//! Store-level navigation tests for chunk::list_next / list_prev.

mod common;
mod fixtures;

use mn_store::entities::chunk;

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn list_next_returns_chunks_after_anchor_in_order() {
    let h = common::boot().await;
    // 5-chunk doc, indices 0..=4
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "next-test", 5).await;

    let rows = chunk::list_next(&h.pool, fx.chunk_ids[1], 2).await.unwrap();
    let idxs: Vec<i32> = rows.iter().map(|r| r.chunk.chunk_index).collect();
    assert_eq!(idxs, vec![2, 3]);
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn list_next_returns_empty_on_last_chunk() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "next-end", 3).await;

    let rows = chunk::list_next(&h.pool, fx.chunk_ids[2], 5).await.unwrap();
    assert!(rows.is_empty());
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn list_prev_returns_chunks_before_anchor_in_ascending_order() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "prev-test", 5).await;

    // Anchor at index 4; ask for 2 prev → expect indices [2, 3].
    let rows = chunk::list_prev(&h.pool, fx.chunk_ids[4], 2).await.unwrap();
    let idxs: Vec<i32> = rows.iter().map(|r| r.chunk.chunk_index).collect();
    assert_eq!(idxs, vec![2, 3]);
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn list_prev_returns_empty_on_first_chunk() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "prev-start", 3).await;

    let rows = chunk::list_prev(&h.pool, fx.chunk_ids[0], 5).await.unwrap();
    assert!(rows.is_empty());
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn list_next_skips_embed_failed_chunks() {
    let h = common::boot().await;
    // 5-chunk doc; mark chunk at index 2 as embed_failed.
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "next-skip", 5).await;
    fixtures::mark_chunk_failed(&h.pool, fx.chunk_ids[2]).await;

    // Anchor at index 1; ask for 3 next → expect indices [3, 4] (2 is skipped).
    let rows = chunk::list_next(&h.pool, fx.chunk_ids[1], 3).await.unwrap();
    let idxs: Vec<i32> = rows.iter().map(|r| r.chunk.chunk_index).collect();
    assert_eq!(idxs, vec![3, 4]);
}
```

Add `ingest_n_chunk_doc(pool, slug, n)` and `mark_chunk_failed(pool, chunk_id)` to `crates/mn-store/tests/fixtures.rs`. The first is a generalization of `ingest_minimal_two_chunk_doc` to N chunks. The second is `UPDATE chunk SET status = 'embed_failed' WHERE id = $1` via sqlx.

- [ ] **Step 2: Run, confirm fails**

Run: `cargo test -p mn-store --features integration --test chunk_navigation`
Expected: all 5 tests FAIL with "function not defined" (compile error is fine).

- [ ] **Step 3: Implement `list_next` and `list_prev`**

In `crates/mn-store/src/entities/chunk.rs`:

```rust
/// List the next `count` chunks after `anchor` in the same document,
/// ordered by `chunk_index` ascending. Skips `embed_failed` chunks.
///
/// # Errors
///
/// Returns `StoreError::NotFound` if the anchor doesn't exist.
pub async fn list_next(pool: &PgPool, anchor: Uuid, count: usize) -> Result<Vec<ChunkWithContext>> {
    let count = i64::try_from(count.clamp(1, 100)).unwrap_or(5);
    let rows = sqlx::query_as::<_, ChunkWithContextRow>(
        "WITH a AS (SELECT document_id, chunk_index FROM chunk WHERE id = $1) \
         SELECT \
            c.id, c.source_version_id, c.document_id, c.node_id, c.chunk_index, c.total_chunks, \
            c.content, c.content_hash, c.embedding_model_id, c.heading_path, c.symbol_path, \
            c.start_byte, c.end_byte, c.token_count, c.status, c.created_at, \
            d.source_path AS d_source_path, d.published_url AS d_published_url, \
            d.source_url AS d_source_url, d.language AS d_language, d.kind AS d_kind, \
            d.provenance AS d_provenance, \
            s.slug AS s_slug \
         FROM chunk c \
         JOIN document d ON c.document_id = d.id \
         JOIN source_version sv ON c.source_version_id = sv.id \
         JOIN source s ON sv.source_id = s.id, a \
         WHERE c.document_id = a.document_id \
           AND c.chunk_index > a.chunk_index \
           AND c.status <> 'embed_failed' \
         ORDER BY c.chunk_index ASC \
         LIMIT $2",
    )
    .bind(anchor)
    .bind(count)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// List the previous `count` chunks before `anchor` in the same document,
/// returned in ascending `chunk_index` (reading) order. SQL fetches the
/// `count` immediately-preceding rows via DESC LIMIT, then the helper
/// reverses to ascending. Skips `embed_failed` chunks.
///
/// # Errors
///
/// Returns `StoreError::NotFound` if the anchor doesn't exist.
pub async fn list_prev(pool: &PgPool, anchor: Uuid, count: usize) -> Result<Vec<ChunkWithContext>> {
    let count = i64::try_from(count.clamp(1, 100)).unwrap_or(5);
    let mut rows = sqlx::query_as::<_, ChunkWithContextRow>(
        "WITH a AS (SELECT document_id, chunk_index FROM chunk WHERE id = $1) \
         SELECT \
            c.id, c.source_version_id, c.document_id, c.node_id, c.chunk_index, c.total_chunks, \
            c.content, c.content_hash, c.embedding_model_id, c.heading_path, c.symbol_path, \
            c.start_byte, c.end_byte, c.token_count, c.status, c.created_at, \
            d.source_path AS d_source_path, d.published_url AS d_published_url, \
            d.source_url AS d_source_url, d.language AS d_language, d.kind AS d_kind, \
            d.provenance AS d_provenance, \
            s.slug AS s_slug \
         FROM chunk c \
         JOIN document d ON c.document_id = d.id \
         JOIN source_version sv ON c.source_version_id = sv.id \
         JOIN source s ON sv.source_id = s.id, a \
         WHERE c.document_id = a.document_id \
           AND c.chunk_index < a.chunk_index \
           AND c.status <> 'embed_failed' \
         ORDER BY c.chunk_index DESC \
         LIMIT $2",
    )
    .bind(anchor)
    .bind(count)
    .fetch_all(pool)
    .await?;
    rows.reverse();
    rows.into_iter().map(TryInto::try_into).collect()
}
```

- [ ] **Step 4: Delete `list_siblings`**

Remove the `pub async fn list_siblings(...)` function entirely from `crates/mn-store/src/entities/chunk.rs`. The route handler in `crates/mn-server/src/routes/chunks.rs` still references it — leave that broken; Task 6 removes the handler. `cargo build -p mn-store` should succeed now; `cargo build -p mn-server` will fail. That's expected.

- [ ] **Step 5: Run the new tests**

Run: `cargo test -p mn-store --features integration --test chunk_navigation`
Expected: 5 tests PASS.

- [ ] **Step 6: Run the full `mn-store` suite**

Run: `cargo test -p mn-store --features integration`
Expected: every test passes. (Tests that referenced `list_siblings` would need to be updated; if any exist, find them with `rg list_siblings crates/mn-store/` and delete the relevant test functions.)

- [ ] **Step 7: Commit**

```bash
git add crates/mn-store/
git commit -m "feat(mn-store): chunk::list_next + list_prev; delete list_siblings

list_next returns up to N chunks after the anchor in ascending order.
list_prev fetches DESC LIMIT N and reverses in-process to deliver
chunks immediately preceding the anchor in reading order. Both skip
embed_failed chunks. list_siblings is removed outright — the route
handler that called it goes in the next commit (mn-server temporarily
fails to build).

Spec: docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md §2"
```

### Task 3: `document::get_overview` + `DocumentOverview` type

**Files:**
- Modify: `crates/mn-store/src/entities/document.rs`

- [ ] **Step 1: Add the response types**

In `crates/mn-store/src/entities/document.rs`:

```rust
/// Document overview returned by `GET /v1/documents/:id` — full document
/// row + the source's slug + ordered chunk_ids. No chunk bodies.
///
/// Spec §1.3 of the chunk+document navigation design.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentOverview {
    #[serde(flatten)]
    pub document: Document,
    pub source: crate::entities::chunk::SourceSummary,
    pub chunk_ids: Vec<Uuid>,
}
```

- [ ] **Step 2: Write the failing test**

In `crates/mn-store/tests/document_navigation.rs` (new file):

```rust
//! Store-level navigation tests for document::* helpers.
mod common;
mod fixtures;

use mn_store::entities::document;

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_overview_returns_doc_plus_ordered_chunk_ids() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "overview", 4).await;

    let ov = document::get_overview(&h.pool, fx.document_id).await.unwrap();

    assert_eq!(ov.document.id, fx.document_id);
    assert_eq!(ov.source.slug, "overview");
    assert_eq!(ov.chunk_ids, fx.chunk_ids); // already in chunk_index order from the fixture
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_overview_omits_embed_failed_chunks() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "overview-skip", 4).await;
    fixtures::mark_chunk_failed(&h.pool, fx.chunk_ids[2]).await;

    let ov = document::get_overview(&h.pool, fx.document_id).await.unwrap();
    let expected: Vec<_> = vec![fx.chunk_ids[0], fx.chunk_ids[1], fx.chunk_ids[3]];
    assert_eq!(ov.chunk_ids, expected);
}
```

- [ ] **Step 3: Run, confirm fails**

Run: `cargo test -p mn-store --features integration --test document_navigation get_overview`
Expected: 2 tests FAIL.

- [ ] **Step 4: Implement `get_overview`**

In `crates/mn-store/src/entities/document.rs`:

```rust
/// Get a document overview: full document row + source slug + ordered chunk_ids.
///
/// # Errors
///
/// Returns `StoreError::NotFound` if no document has that id.
pub async fn get_overview(pool: &PgPool, id: Uuid) -> Result<DocumentOverview> {
    let document = get_by_id(pool, id).await?;
    let source_slug = sqlx::query_scalar::<_, String>(
        "SELECT s.slug FROM source s \
         JOIN source_version sv ON sv.source_id = s.id \
         WHERE sv.id = $1",
    )
    .bind(document.source_version_id)
    .fetch_one(pool)
    .await?;
    let chunk_ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM chunk \
         WHERE document_id = $1 AND status <> 'embed_failed' \
         ORDER BY chunk_index ASC",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(DocumentOverview {
        document,
        source: crate::entities::chunk::SourceSummary { slug: source_slug },
        chunk_ids,
    })
}
```

- [ ] **Step 5: Run, confirm pass**

Run: `cargo test -p mn-store --features integration --test document_navigation get_overview`
Expected: 2 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-store/src/entities/document.rs crates/mn-store/tests/document_navigation.rs
git commit -m "feat(mn-store): document::get_overview + DocumentOverview

Document row + source.slug + ordered chunk_ids (ready only). Powers
GET /v1/documents/:id. Embed-failed chunks are omitted from chunk_ids
for consistency with the rest of the read surface.

Spec: docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md §2"
```

### Task 4: `document::get_full` + `DocumentFull` type + cap-overflow signaling

**Files:**
- Modify: `crates/mn-store/src/entities/document.rs`

- [ ] **Step 1: Add the response types**

```rust
/// One chunk body in a document-full or document-window response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkBody {
    pub chunk_id: Uuid,
    pub chunk_index: i32,
    pub content: String,
    pub heading_path: Vec<String>,
    pub token_count: i32,
}

/// Complete document: metadata + every ready chunk inline.
///
/// Spec §1.4 of the chunk+document navigation design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFull {
    #[serde(flatten)]
    pub document: Document,
    pub source: crate::entities::chunk::SourceSummary,
    pub chunks: Vec<ChunkBody>,
}

/// Returned by `get_full` when the document exceeds the chunk cap, so the
/// route can map to a 412 without paying the cost of a full chunk fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullResult {
    Document(DocumentFull),
    TooManyChunks { count: usize, cap: usize },
}
```

Wait — `FullResult` carries `DocumentFull` (not `Copy`) so remove `#[derive(Clone, Copy, PartialEq, Eq)]`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullResult {
    Document(DocumentFull),
    TooManyChunks { count: usize, cap: usize },
}
```

- [ ] **Step 2: Write the failing tests**

Add to `crates/mn-store/tests/document_navigation.rs`:

```rust
#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_full_returns_document_with_all_chunks_in_order() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "full", 4).await;

    let res = document::get_full(&h.pool, fx.document_id, 500).await.unwrap();
    let full = match res {
        document::FullResult::Document(f) => f,
        document::FullResult::TooManyChunks { .. } => panic!("unexpected cap result"),
    };
    let idxs: Vec<i32> = full.chunks.iter().map(|c| c.chunk_index).collect();
    assert_eq!(idxs, vec![0, 1, 2, 3]);
    assert_eq!(full.source.slug, "full");
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_full_signals_too_many_chunks_above_cap() {
    let h = common::boot().await;
    // 6-chunk doc; cap at 5 to trigger the overflow without inserting hundreds.
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "full-cap", 6).await;

    let res = document::get_full(&h.pool, fx.document_id, 5).await.unwrap();
    match res {
        document::FullResult::TooManyChunks { count, cap } => {
            assert_eq!(count, 6);
            assert_eq!(cap, 5);
        }
        document::FullResult::Document(_) => panic!("expected TooManyChunks"),
    }
}
```

- [ ] **Step 3: Run, confirm fails**

Run: `cargo test -p mn-store --features integration --test document_navigation get_full`
Expected: 2 tests FAIL.

- [ ] **Step 4: Implement `get_full`**

```rust
/// Get a complete document with every ready chunk inline, capped at `cap`.
/// When the document has > `cap` ready chunks, returns `TooManyChunks`
/// (the caller maps to a 412 response).
///
/// # Errors
///
/// Returns `StoreError::NotFound` if no document has that id.
pub async fn get_full(pool: &PgPool, id: Uuid, cap: usize) -> Result<FullResult> {
    let document = get_by_id(pool, id).await?;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chunk WHERE document_id = $1 AND status <> 'embed_failed'",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    let count_usize = usize::try_from(count).unwrap_or(0);
    if count_usize > cap {
        return Ok(FullResult::TooManyChunks { count: count_usize, cap });
    }
    let source_slug = sqlx::query_scalar::<_, String>(
        "SELECT s.slug FROM source s \
         JOIN source_version sv ON sv.source_id = s.id \
         WHERE sv.id = $1",
    )
    .bind(document.source_version_id)
    .fetch_one(pool)
    .await?;
    let chunks = sqlx::query_as::<_, ChunkBodyRow>(
        "SELECT id AS chunk_id, chunk_index, content, heading_path, token_count \
         FROM chunk \
         WHERE document_id = $1 AND status <> 'embed_failed' \
         ORDER BY chunk_index ASC",
    )
    .bind(id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| ChunkBody {
        chunk_id: r.chunk_id,
        chunk_index: r.chunk_index,
        content: r.content,
        heading_path: r.heading_path,
        token_count: r.token_count,
    })
    .collect();
    Ok(FullResult::Document(DocumentFull {
        document,
        source: crate::entities::chunk::SourceSummary { slug: source_slug },
        chunks,
    }))
}

#[derive(sqlx::FromRow)]
struct ChunkBodyRow {
    chunk_id: Uuid,
    chunk_index: i32,
    content: String,
    heading_path: Vec<String>,
    token_count: i32,
}
```

- [ ] **Step 5: Run, confirm pass**

Run: `cargo test -p mn-store --features integration --test document_navigation get_full`
Expected: 2 PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-store/src/entities/document.rs crates/mn-store/tests/document_navigation.rs
git commit -m "feat(mn-store): document::get_full + cap-overflow signal

Returns FullResult::Document with metadata + every ready chunk inline
when the chunk count is <= cap; FullResult::TooManyChunks { count, cap }
above it so the route can map to 412 without paying the full chunk fetch.

Spec: docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md §2"
```

### Task 5: `document::list_chunks_window` + `DocumentChunkWindow` type

**Files:**
- Modify: `crates/mn-store/src/entities/document.rs`

- [ ] **Step 1: Add the response type**

```rust
/// Window of chunks at offset `from` with `limit` cap.
///
/// Spec §1.5 of the chunk+document navigation design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentChunkWindow {
    #[serde(flatten)]
    pub document: Document,
    pub source: crate::entities::chunk::SourceSummary,
    pub chunks: Vec<ChunkBody>,
    pub from: usize,
    pub limit: usize,
    pub total_chunks: usize,
}
```

- [ ] **Step 2: Write the failing tests**

Add to `crates/mn-store/tests/document_navigation.rs`:

```rust
#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn list_chunks_window_returns_requested_range() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "window", 10).await;

    let w = document::list_chunks_window(&h.pool, fx.document_id, 3, 4).await.unwrap();
    let idxs: Vec<i32> = w.chunks.iter().map(|c| c.chunk_index).collect();
    assert_eq!(idxs, vec![3, 4, 5, 6]);
    assert_eq!(w.from, 3);
    assert_eq!(w.limit, 4);
    assert_eq!(w.total_chunks, 10);
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn list_chunks_window_past_end_returns_empty_with_total() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "window-end", 5).await;

    let w = document::list_chunks_window(&h.pool, fx.document_id, 10, 5).await.unwrap();
    assert!(w.chunks.is_empty());
    assert_eq!(w.from, 10);
    assert_eq!(w.total_chunks, 5);
}
```

- [ ] **Step 3: Run, confirm fails**

Run: `cargo test -p mn-store --features integration --test document_navigation list_chunks_window`
Expected: 2 FAIL.

- [ ] **Step 4: Implement**

```rust
/// Get a windowed slice of a document's chunks, starting at chunk index
/// `from`, returning up to `limit` chunks. Also returns the document
/// metadata and total ready-chunk count so callers can render
/// "chunks K..K+N of M".
///
/// # Errors
///
/// Returns `StoreError::NotFound` if no document has that id.
pub async fn list_chunks_window(
    pool: &PgPool,
    id: Uuid,
    from: usize,
    limit: usize,
) -> Result<DocumentChunkWindow> {
    let limit = limit.clamp(1, 100);
    let document = get_by_id(pool, id).await?;
    let source_slug = sqlx::query_scalar::<_, String>(
        "SELECT s.slug FROM source s \
         JOIN source_version sv ON sv.source_id = s.id \
         WHERE sv.id = $1",
    )
    .bind(document.source_version_id)
    .fetch_one(pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chunk WHERE document_id = $1 AND status <> 'embed_failed'",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    let chunks = sqlx::query_as::<_, ChunkBodyRow>(
        "SELECT id AS chunk_id, chunk_index, content, heading_path, token_count \
         FROM chunk \
         WHERE document_id = $1 AND status <> 'embed_failed' \
         ORDER BY chunk_index ASC \
         OFFSET $2 LIMIT $3",
    )
    .bind(id)
    .bind(i64::try_from(from).unwrap_or(0))
    .bind(i64::try_from(limit).unwrap_or(20))
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| ChunkBody {
        chunk_id: r.chunk_id,
        chunk_index: r.chunk_index,
        content: r.content,
        heading_path: r.heading_path,
        token_count: r.token_count,
    })
    .collect();
    Ok(DocumentChunkWindow {
        document,
        source: crate::entities::chunk::SourceSummary { slug: source_slug },
        chunks,
        from,
        limit,
        total_chunks: usize::try_from(total).unwrap_or(0),
    })
}
```

- [ ] **Step 5: Run, confirm pass**

Run: `cargo test -p mn-store --features integration --test document_navigation list_chunks_window`
Expected: 2 PASS.

- [ ] **Step 6: Run the full mn-store suite**

Run: `cargo test -p mn-store --features integration`
Expected: every test passes.

- [ ] **Step 7: Commit**

```bash
git add crates/mn-store/src/entities/document.rs crates/mn-store/tests/document_navigation.rs
git commit -m "feat(mn-store): document::list_chunks_window

OFFSET/LIMIT window over a document's ready chunks. Reports the
total chunk count so callers can render 'chunks K..K+N of M'. Past-end
requests return an empty chunks array with accurate total.

Spec: docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md §2"
```

---

## Phase 2 — Server routes

### Task 6: Update `routes/chunks.rs` — augment + add next/prev + delete siblings

**Files:**
- Modify: `crates/mn-server/src/routes/chunks.rs`
- Modify: `crates/mn-server/tests/chunk_endpoints.rs` (delete the siblings test)

- [ ] **Step 1: Write the failing test**

Add to `crates/mn-server/tests/chunks_navigation.rs` (new file):

```rust
//! Integration tests for /v1/chunks/:id, /next, /prev.
mod common;
mod fixtures;

use axum_test::TestServer;

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_chunk_returns_document_and_source_context() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "ctx", 3).await;
    let server = TestServer::new(mn_server::app::router(h.state.clone())).unwrap();

    let resp = server.get(&format!("/v1/chunks/{}", fx.chunk_ids[1])).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["id"], serde_json::Value::String(fx.chunk_ids[1].to_string()));
    // F-bug check: document.published_url is non-null.
    assert!(body["document"]["published_url"].is_string());
    assert_eq!(body["source"]["slug"], "ctx");
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_next_returns_chunks_after_anchor() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "next", 5).await;
    let server = TestServer::new(mn_server::app::router(h.state.clone())).unwrap();

    let resp = server.get(&format!("/v1/chunks/{}/next?count=2", fx.chunk_ids[1])).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let chunks = body["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0]["chunk_index"], 2);
    assert_eq!(chunks[1]["chunk_index"], 3);
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_prev_returns_chunks_before_anchor() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "prev", 5).await;
    let server = TestServer::new(mn_server::app::router(h.state.clone())).unwrap();

    let resp = server.get(&format!("/v1/chunks/{}/prev?count=2", fx.chunk_ids[4])).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let chunks = body["chunks"].as_array().unwrap();
    let idxs: Vec<i64> = chunks.iter().map(|c| c["chunk_index"].as_i64().unwrap()).collect();
    assert_eq!(idxs, vec![2, 3]);
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_chunk_404_for_missing_id() {
    let h = common::boot().await;
    let server = TestServer::new(mn_server::app::router(h.state.clone())).unwrap();

    let resp = server.get(&format!("/v1/chunks/{}", uuid::Uuid::new_v4())).await;
    resp.assert_status_not_found();
}
```

`axum_test::TestServer` may already be used by the existing chunk_endpoints test — read it first to confirm the pattern; adapt if a different test harness shape is used.

If the test fixtures module (`fixtures.rs`) doesn't yet exist under `crates/mn-server/tests/`, copy or symlink the one from `crates/mn-store/tests/` (or just `mod fixtures;` referencing the same path via Cargo `path = "../mn-store/tests/fixtures.rs"` — pick whichever is cleaner; the simplest is to duplicate the fixture file).

- [ ] **Step 2: Run, confirm fails**

Run: `cargo test -p mn-server --features integration --test chunks_navigation`
Expected: tests FAIL (route changes not yet made; also `mn-server` may not build because Task 2 deleted `list_siblings`).

- [ ] **Step 3: Rewrite `routes/chunks.rs`**

Replace the entire contents of `crates/mn-server/src/routes/chunks.rs` with:

```rust
//! `GET /v1/chunks/:id` + `/next` + `/prev` + `/parents`.
//!
//! Each endpoint returns a chunk row with its document and source context
//! bundled. The `/next` and `/prev` endpoints walk in `chunk_index` order;
//! `embed_failed` chunks are skipped. `/siblings` (unbounded) was removed
//! in favor of position-windowed `/v1/documents/:id/chunks`.

use axum::extract::{Extension, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::{entities::chunk, entities::node, StoreError};
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Mount the chunk read routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/chunks/:id", get(get_chunk))
        .route("/v1/chunks/:id/next", get(get_next))
        .route("/v1/chunks/:id/prev", get(get_prev))
        .route("/v1/chunks/:id/parents", get(get_parents))
}

#[derive(Debug, Deserialize)]
struct CountQuery {
    #[serde(default = "default_count")]
    count: usize,
}

const fn default_count() -> usize { 5 }

async fn get_chunk(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match chunk::get_with_context(&state.pool, id).await {
        Ok(c) => Json(c).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("chunk `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_chunk failed");
            error::service_unavailable("chunk lookup failed", rid)
        }
    }
}

async fn get_next(
    Path(id): Path<Uuid>,
    Query(q): Query<CountQuery>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match chunk::list_next(&state.pool, id, q.count).await {
        Ok(chunks) => Json(serde_json::json!({ "chunks": chunks })).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("chunk `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_next failed");
            error::service_unavailable("next-chunks lookup failed", rid)
        }
    }
}

async fn get_prev(
    Path(id): Path<Uuid>,
    Query(q): Query<CountQuery>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match chunk::list_prev(&state.pool, id, q.count).await {
        Ok(chunks) => Json(serde_json::json!({ "chunks": chunks })).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("chunk `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_prev failed");
            error::service_unavailable("prev-chunks lookup failed", rid)
        }
    }
}

async fn get_parents(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    // get_with_context happens to fetch document_id; if it's expensive,
    // a smaller helper (chunk::get_node_id(id) -> Uuid) would do. For now
    // reuse get_with_context for simplicity.
    let parent_chunk = match chunk::get_with_context(&state.pool, id).await {
        Ok(c) => c,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("chunk `{id}` not found"), rid)
        }
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_chunk (for parents) failed");
            return error::service_unavailable("chunk lookup failed", rid);
        }
    };
    match node::parent_chain(&state.pool, parent_chunk.chunk.node_id).await {
        Ok(chain) => Json(chain).into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "parent_chain failed");
            error::service_unavailable("parent-chain lookup failed", rid)
        }
    }
}
```

- [ ] **Step 4: Delete the siblings test in `chunk_endpoints.rs`**

Run: `rg "siblings" crates/mn-server/tests/chunk_endpoints.rs`
Find the test function(s) that hit `/v1/chunks/:id/siblings`. Delete them. Keep any tests for `/v1/chunks/:id` (those still apply but their assertions about the response shape need updating — the response is now `ChunkWithContext`, so test bodies that check chunk fields stay valid; tests that check that the response does NOT contain a `document` key need to be removed or updated).

- [ ] **Step 5: Run the new tests**

Run: `cargo test -p mn-server --features integration --test chunks_navigation`
Expected: 4 tests PASS.

- [ ] **Step 6: Run the full mn-server suite**

Run: `cargo test -p mn-server --features integration`
Expected: every test passes.

- [ ] **Step 7: Commit**

```bash
git add crates/mn-server/
git commit -m "feat(mn-server): augment /v1/chunks/:id, add /next + /prev, delete /siblings

/v1/chunks/:id now returns ChunkWithContext (bundled document and
source). /v1/chunks/:id/next and /prev take ?count=N (default 5,
clamp [1,100]) and return up to N chunks in reading order. The
siblings route is removed outright — operators use
/v1/documents/:id/chunks?from=K&limit=L for windowed access (lands
in next commit).

Spec: docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md §3"
```

### Task 7: New `routes/documents.rs` + mount in `app.rs`

**Files:**
- Create: `crates/mn-server/src/routes/documents.rs`
- Modify: `crates/mn-server/src/routes/mod.rs`
- Modify: `crates/mn-server/src/app.rs`
- Create: `crates/mn-server/tests/documents_navigation.rs`

- [ ] **Step 1: Write the failing tests**

In `crates/mn-server/tests/documents_navigation.rs`:

```rust
mod common;
mod fixtures;

use axum_test::TestServer;

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_document_returns_overview_with_chunk_ids() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "doc-ov", 3).await;
    let server = TestServer::new(mn_server::app::router(h.state.clone())).unwrap();

    let resp = server.get(&format!("/v1/documents/{}", fx.document_id)).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["id"], serde_json::Value::String(fx.document_id.to_string()));
    assert_eq!(body["source"]["slug"], "doc-ov");
    let ids = body["chunk_ids"].as_array().unwrap();
    assert_eq!(ids.len(), 3);
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_document_full_returns_chunks_inline() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "doc-full", 2).await;
    let server = TestServer::new(mn_server::app::router(h.state.clone())).unwrap();

    let resp = server.get(&format!("/v1/documents/{}/full", fx.document_id)).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let chunks = body["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 2);
    assert!(chunks[0]["content"].is_string());
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_document_full_returns_412_above_cap() {
    let h = common::boot().await;
    // Use the over-cap helper that ingests 6 chunks and the route caps at 5
    // via a query-string override OR we re-use the existing constant. The
    // simplest: insert 6 chunks but set the cap to 5 via DOCUMENT_FULL_CHUNK_CAP
    // override in test config. Cleanest is a test that uses a fixture that
    // CAN exceed 500 chunks — but that's 500+ inserts. Instead, the test uses
    // a special test-only env var MIDNIGHT_MANUAL_DOCUMENT_FULL_CHUNK_CAP that
    // the route honors when set (read once at boot). Add that behavior to the
    // route handler.
    std::env::set_var("MIDNIGHT_MANUAL_DOCUMENT_FULL_CHUNK_CAP", "5");
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "doc-cap", 6).await;
    let server = TestServer::new(mn_server::app::router(h.state.clone())).unwrap();

    let resp = server.get(&format!("/v1/documents/{}/full", fx.document_id)).await;
    resp.assert_status(axum::http::StatusCode::PRECONDITION_FAILED);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["error"], "too_many_chunks");
    assert_eq!(body["chunk_count"], 6);
    assert_eq!(body["cap"], 5);
    std::env::remove_var("MIDNIGHT_MANUAL_DOCUMENT_FULL_CHUNK_CAP");
}

#[tokio::test]
#[cfg_attr(not(feature = "integration"), ignore)]
async fn get_document_chunks_returns_windowed_slice() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "doc-win", 10).await;
    let server = TestServer::new(mn_server::app::router(h.state.clone())).unwrap();

    let resp = server.get(&format!("/v1/documents/{}/chunks?from=3&limit=4", fx.document_id)).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let chunks = body["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 4);
    assert_eq!(body["from"], 3);
    assert_eq!(body["limit"], 4);
    assert_eq!(body["total_chunks"], 10);
}
```

- [ ] **Step 2: Run, confirm fails**

Run: `cargo test -p mn-server --features integration --test documents_navigation`
Expected: 4 tests FAIL (routes don't exist).

- [ ] **Step 3: Implement `routes/documents.rs`**

```rust
//! `GET /v1/documents/:id` + `/full` + `/chunks`.

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::{entities::document, StoreError};
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Hard cap on the number of chunks `/v1/documents/:id/full` will return
/// in one response. Above this, the endpoint returns 412 with a hint
/// pointing at the window endpoint.
pub const DOCUMENT_FULL_CHUNK_CAP: usize = 500;

fn effective_cap() -> usize {
    std::env::var("MIDNIGHT_MANUAL_DOCUMENT_FULL_CHUNK_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DOCUMENT_FULL_CHUNK_CAP)
}

#[derive(Debug, Deserialize)]
struct WindowQuery {
    #[serde(default)]
    from: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}

const fn default_limit() -> usize { 20 }

/// Mount the document read routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/documents/:id", get(get_document))
        .route("/v1/documents/:id/full", get(get_document_full))
        .route("/v1/documents/:id/chunks", get(get_document_chunks))
}

async fn get_document(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match document::get_overview(&state.pool, id).await {
        Ok(ov) => Json(ov).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("document `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_document failed");
            error::service_unavailable("document lookup failed", rid)
        }
    }
}

async fn get_document_full(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let cap = effective_cap();
    match document::get_full(&state.pool, id, cap).await {
        Ok(document::FullResult::Document(d)) => Json(d).into_response(),
        Ok(document::FullResult::TooManyChunks { count, cap }) => (
            StatusCode::PRECONDITION_FAILED,
            Json(serde_json::json!({
                "error": "too_many_chunks",
                "chunk_count": count,
                "cap": cap,
                "hint": format!("Use GET /v1/documents/{id}/chunks?from=K&limit=L (default L=20)"),
            })),
        ).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("document `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_document_full failed");
            error::service_unavailable("document lookup failed", rid)
        }
    }
}

async fn get_document_chunks(
    Path(id): Path<Uuid>,
    Query(q): Query<WindowQuery>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match document::list_chunks_window(&state.pool, id, q.from, q.limit).await {
        Ok(w) => Json(w).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("document `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_document_chunks failed");
            error::service_unavailable("document window failed", rid)
        }
    }
}
```

- [ ] **Step 4: Register the module**

In `crates/mn-server/src/routes/mod.rs`, add:

```rust
pub mod documents;
```

In `crates/mn-server/src/app.rs`, find the existing `.merge(routes::chunks::router())` call (or equivalent — the router-mounting pattern) and add:

```rust
.merge(routes::documents::router())
```

- [ ] **Step 5: Run the new tests**

Run: `cargo test -p mn-server --features integration --test documents_navigation`
Expected: 4 PASS.

- [ ] **Step 6: Run the full mn-server suite**

Run: `cargo test -p mn-server --features integration`
Expected: every test passes.

- [ ] **Step 7: Commit**

```bash
git add crates/mn-server/
git commit -m "feat(mn-server): GET /v1/documents/:id + /full + /chunks

Three new read routes for document-level navigation. /full caps at
500 chunks (override via MIDNIGHT_MANUAL_DOCUMENT_FULL_CHUNK_CAP env
for testing) and returns 412 too_many_chunks with a hint pointing at
/chunks?from=K&limit=L above the cap.

Spec: docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md §3"
```

---

## Phase 3 — CLI namespaces

### Task 8: CLI scaffolding — Command enum + telemetry variants

**Files:**
- Modify: `crates/mn-cli/src/cli.rs`
- Modify: `crates/mn-cli/src/commands/mod.rs`
- Modify: `crates/mn-telemetry/src/events.rs`
- Create: `crates/mn-cli/src/commands/chunks/mod.rs` (skeleton)
- Create: `crates/mn-cli/src/commands/documents/mod.rs` (skeleton)

- [ ] **Step 1: Add telemetry variants**

In `crates/mn-telemetry/src/events.rs`, add `Chunks` and `Documents` to the `CliCommandName` enum. Look for the existing variants (`Search, Sources, Models, Auth, …`) and add the two new ones in matching style. They serialize as `"chunks"` and `"documents"` via the enum's `#[serde(rename_all = "snake_case")]`.

- [ ] **Step 2: Create the skeleton dispatchers**

`crates/mn-cli/src/commands/chunks/mod.rs`:

```rust
//! `mnm chunks <subcommand>` dispatcher.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::TelemetryClient;

// Sub-modules added in Task 9:
//   pub mod show;
//   pub mod next;
//   pub mod prev;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: ChunksCmd,
}

#[derive(Debug, Subcommand)]
pub enum ChunksCmd {
    // Variants added in Task 9.
}

pub async fn run(
    _args: Args,
    _server: Option<&str>,
    _telemetry: &TelemetryClient,
    _cli_version: &str,
    _json: bool,
) -> Result<()> {
    unreachable!("no chunks subcommands yet — Task 9 wires Show/Next/Prev")
}
```

Mirror for `crates/mn-cli/src/commands/documents/mod.rs`.

These won't compile because `ChunksCmd` and `DocumentsCmd` are empty (clap requires at least one variant on a `#[derive(Subcommand)]`). Add a placeholder variant `__Reserved` with `#[command(hide = true)]` to keep the enum valid:

```rust
#[derive(Debug, Subcommand)]
pub enum ChunksCmd {
    #[command(hide = true, name = "__reserved")]
    __Reserved,
}
```

Task 9 / 10 replace this with real verbs.

- [ ] **Step 3: Wire the modules**

In `crates/mn-cli/src/commands/mod.rs`:

```rust
pub mod chunks;
pub mod documents;
```

- [ ] **Step 4: Add Command variants**

In `crates/mn-cli/src/cli.rs` `Command` enum:

```rust
/// Inspect chunks: show, next, prev.
Chunks(commands::chunks::Args),
/// Inspect documents: show, full, chunks.
Documents(commands::documents::Args),
```

Add to the dispatch `match cli.cmd`:

```rust
Command::Chunks(args) => commands::chunks::run(args, cli.server.as_deref(), &telemetry, crate::VERSION, cli.json).await,
Command::Documents(args) => commands::documents::run(args, cli.server.as_deref(), &telemetry, crate::VERSION, cli.json).await,
```

Add to `cli_command_name`:

```rust
Command::Chunks(_) => CliCommandName::Chunks,
Command::Documents(_) => CliCommandName::Documents,
```

`Chunks` and `Documents` are NOT admin-hidden — leave them OUT of `ADMIN_SUBCOMMANDS`.

- [ ] **Step 5: Verify build**

Run: `cargo build -p mn-cli`
Expected: builds.

Run: `cargo test --workspace`
Expected: existing tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-cli/ crates/mn-telemetry/
git commit -m "feat(mn-cli): scaffold chunks + documents top-level namespaces

Empty dispatcher shells; Tasks 9 and 10 add the actual verbs. Both
namespaces are always-visible (no admin gate). Adds
CliCommandName::Chunks + Documents to mn-telemetry.

Spec: docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md §4"
```

### Task 9: `mnm chunks {show, next, prev}` — full namespace

**Files:**
- Modify: `crates/mn-cli/src/commands/chunks/mod.rs`
- Create: `crates/mn-cli/src/commands/chunks/show.rs`
- Create: `crates/mn-cli/src/commands/chunks/next.rs`
- Create: `crates/mn-cli/src/commands/chunks/prev.rs`
- Create: `crates/mn-cli/tests/chunks_cli.rs`

- [ ] **Step 1: Write the failing tests**

`crates/mn-cli/tests/chunks_cli.rs`:

```rust
//! Wiremock-backed smoke tests for `mnm chunks {show, next, prev}`.

use std::process::Command;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn chunk_with_context_json() -> serde_json::Value {
    serde_json::json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "source_version_id": "22222222-2222-2222-2222-222222222222",
        "document_id": "33333333-3333-3333-3333-333333333333",
        "node_id": "44444444-4444-4444-4444-444444444444",
        "chunk_index": 0,
        "total_chunks": 3,
        "content": "Hello world body text.",
        "content_hash": "sha256:abc",
        "embedding_model_id": "55555555-5555-5555-5555-555555555555",
        "heading_path": ["Welcome"],
        "symbol_path": [],
        "start_byte": 0,
        "end_byte": 22,
        "token_count": 4,
        "status": "ready",
        "created_at": "2026-05-25T00:00:00Z",
        "document": {
            "id": "33333333-3333-3333-3333-333333333333",
            "source_path": "welcome.md",
            "published_url": "https://example.com/welcome/",
            "source_url": null,
            "language": "markdown",
            "kind": "markdown",
            "provenance": {}
        },
        "source": { "slug": "smoke" }
    })
}

#[tokio::test]
async fn chunks_show_renders_chunk_and_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/chunks/11111111-1111-1111-1111-111111111111"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chunk_with_context_json()))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["--server", &server.uri(), "chunks", "show",
               "11111111-1111-1111-1111-111111111111"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Hello world body text"), "stdout: {stdout}");
    assert!(stdout.contains("welcome.md") || stdout.contains("https://example.com/welcome/"),
            "expected document context in output: {stdout}");
}

#[tokio::test]
async fn chunks_next_renders_two_chunks() {
    let server = MockServer::start().await;
    let body = serde_json::json!({ "chunks": [chunk_with_context_json(), chunk_with_context_json()] });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/chunks/[0-9a-f-]+/next$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["--server", &server.uri(), "chunks", "next",
               "11111111-1111-1111-1111-111111111111", "--count", "2"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[tokio::test]
async fn chunks_prev_renders_two_chunks() {
    let server = MockServer::start().await;
    let body = serde_json::json!({ "chunks": [chunk_with_context_json()] });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/chunks/[0-9a-f-]+/prev$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["--server", &server.uri(), "chunks", "prev",
               "11111111-1111-1111-1111-111111111111"])
        .output()
        .unwrap();
    assert!(out.status.success());
}
```

- [ ] **Step 2: Run, confirm fails**

Run: `cargo test -p mn-cli --test chunks_cli`
Expected: 3 FAIL (commands don't exist).

- [ ] **Step 3: Implement `chunks/show.rs`**

```rust
//! `mnm chunks show <chunk-id>` — fetch and render one chunk with bundled context.

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use uuid::Uuid;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Chunk UUID.
    pub chunk_id: Uuid,
}

pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let url = format!("{server_url}/v1/chunks/{}", args.chunk_id);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let bearer = resolve_best_bearer_optional();
    let mut req = client.get(&url);
    if let Some(t) = bearer.as_deref() {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::anyhow!("{status} from {url}: {body}"));
    }
    if json {
        println!("{body}");
    } else {
        render_chunk(&body)?;
    }
    Ok(())
}

fn resolve_best_bearer_optional() -> Option<String> {
    use mn_core::config::StdEnv;
    let auth_path = mn_core::paths::auth_file_path(&StdEnv)?;
    let file = mn_core::auth_file::AuthFile::read_optional(&auth_path).ok().flatten()?;
    let now = time::OffsetDateTime::now_utc();
    file.active_admin_token(now)
        .or_else(|| file.active_read_uplift_token(now))
        .map(str::to_owned)
}

pub(super) fn render_chunk(body: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(body).context("parse response body")?;
    let chunk_index = v["chunk_index"].as_i64().unwrap_or(0) + 1;
    let total = v["total_chunks"].as_i64().unwrap_or(1).max(1);
    let slug = v["source"]["slug"].as_str().unwrap_or("?");
    let path = v["document"]["source_path"].as_str().unwrap_or("?");
    let url = v["document"]["published_url"].as_str().unwrap_or("(none)");
    let headings: Vec<&str> = v["heading_path"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    let content = v["content"].as_str().unwrap_or("");

    println!("chunk {chunk_index}/{total} — {slug}/{path}");
    println!("URL: {url}");
    if !headings.is_empty() {
        println!("heading: > {}", headings.join(" > "));
    }
    println!();
    println!("{content}");
    Ok(())
}
```

- [ ] **Step 4: Implement `chunks/next.rs`**

```rust
//! `mnm chunks next <chunk-id>` — fetch the next N chunks in the same document.

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use uuid::Uuid;

#[derive(Debug, ClapArgs)]
pub struct Args {
    pub chunk_id: Uuid,
    /// Number of chunks to fetch (clamped to [1,100] server-side).
    #[arg(long, default_value_t = 5)]
    pub count: u32,
    /// Show full content instead of a 240-char preview.
    #[arg(long)]
    pub full: bool,
}

pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    run_dir(args, server, json, "next").await
}

async fn run_dir(args: Args, server: Option<&str>, json: bool, dir: &str) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let url = format!("{server_url}/v1/chunks/{}/{}?count={}", args.chunk_id, dir, args.count);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let resp = client.get(&url).send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::anyhow!("{status} from {url}: {body}"));
    }
    if json {
        println!("{body}");
    } else {
        render_chunks(&body, args.full)?;
    }
    Ok(())
}

pub(super) fn render_chunks(body: &str, full: bool) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(body).context("parse response body")?;
    let chunks = v["chunks"].as_array().cloned().unwrap_or_default();
    if chunks.is_empty() {
        println!("(no further chunks)");
        return Ok(());
    }
    for (i, c) in chunks.iter().enumerate() {
        let n = i + 1;
        let chunk_index = c["chunk_index"].as_i64().unwrap_or(0) + 1;
        let total = c["total_chunks"].as_i64().unwrap_or(1).max(1);
        let slug = c["source"]["slug"].as_str().unwrap_or("?");
        let path = c["document"]["source_path"].as_str().unwrap_or("?");
        let url = c["document"]["published_url"].as_str().unwrap_or("(none)");
        let headings: Vec<&str> = c["heading_path"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        let content_full = c["content"].as_str().unwrap_or("");
        let preview = if full {
            content_full.to_owned()
        } else {
            preview_240(content_full)
        };

        println!("{n}. chunk {chunk_index}/{total} — {slug}/{path}");
        println!("   URL: {url}");
        if !headings.is_empty() {
            println!("   heading: > {}", headings.join(" > "));
        }
        println!();
        for line in preview.lines() {
            println!("   {line}");
        }
        println!();
    }
    Ok(())
}

fn preview_240(s: &str) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= 240 {
        one_line
    } else {
        let head: String = one_line.chars().take(237).collect();
        format!("{head}...")
    }
}
```

- [ ] **Step 5: Implement `chunks/prev.rs`**

Same shape as `next.rs` but call `run_dir` with `"prev"`. Easiest: re-export `super::next::run_dir`:

```rust
//! `mnm chunks prev <chunk-id>` — fetch the previous N chunks.

use anyhow::Result;
use clap::Args as ClapArgs;
use uuid::Uuid;

#[derive(Debug, ClapArgs)]
pub struct Args {
    pub chunk_id: Uuid,
    #[arg(long, default_value_t = 5)]
    pub count: u32,
    #[arg(long)]
    pub full: bool,
}

pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    // Reuse next::run_dir by reconstructing its Args shape — they're identical.
    let next_args = super::next::Args {
        chunk_id: args.chunk_id,
        count: args.count,
        full: args.full,
    };
    // Hack: call a shared internal helper. Refactor: extract `run_dir` to
    // `chunks/mod.rs` as `pub(super) async fn run_chunk_list(...)`.
    super::run_chunk_list(next_args, server, json, "prev").await
}
```

That requires extracting `run_dir` to `chunks/mod.rs` as `pub(super) async fn run_chunk_list(args: super::next::Args, ...)`. Alternatively, just duplicate the 25 lines — it's small enough. Pick whichever is cleaner; the structure-with-extraction is preferred.

- [ ] **Step 6: Wire the dispatcher**

Replace `crates/mn-cli/src/commands/chunks/mod.rs` with:

```rust
//! `mnm chunks <subcommand>` dispatcher.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::TelemetryClient;

pub mod show;
pub mod next;
pub mod prev;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: ChunksCmd,
}

#[derive(Debug, Subcommand)]
pub enum ChunksCmd {
    Show(show::Args),
    Next(next::Args),
    Prev(prev::Args),
}

pub async fn run(
    args: Args,
    server: Option<&str>,
    _telemetry: &TelemetryClient,
    _cli_version: &str,
    json: bool,
) -> Result<()> {
    match args.cmd {
        ChunksCmd::Show(a) => show::run(a, server, json).await,
        ChunksCmd::Next(a) => next::run(a, server, json).await,
        ChunksCmd::Prev(a) => prev::run(a, server, json).await,
    }
}

/// Shared helper used by next + prev. Extracted here to avoid duplicating
/// 25 lines of HTTP + render glue.
pub(super) async fn run_chunk_list(
    args: next::Args,
    server: Option<&str>,
    json: bool,
    dir: &str,
) -> Result<()> {
    use anyhow::Context as _;
    let server_url = crate::shared::resolve_server_url(server);
    let url = format!("{server_url}/v1/chunks/{}/{}?count={}", args.chunk_id, dir, args.count);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let resp = client.get(&url).send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::anyhow!("{status} from {url}: {body}"));
    }
    if json {
        println!("{body}");
    } else {
        next::render_chunks(&body, args.full)?;
    }
    Ok(())
}
```

Update `next.rs`'s `run` to call `super::run_chunk_list(args, server, json, "next")`. Drop the in-file `run_dir`. `render_chunks` stays in `next.rs` (re-used by prev).

- [ ] **Step 7: Run the tests**

Run: `cargo test -p mn-cli --test chunks_cli`
Expected: 3 PASS.

- [ ] **Step 8: Run the full mn-cli suite**

Run: `cargo test -p mn-cli`
Expected: every test passes.

- [ ] **Step 9: Commit**

```bash
git add crates/mn-cli/
git commit -m "feat(mn-cli): mnm chunks {show, next, prev}

Three verbs in the new always-visible chunks namespace. show renders
one chunk with bundled document+source context; next/prev fetch
windowed neighbors with 240-char preview (--full to expand). --json
on every verb dumps the server response verbatim.

Spec: docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md §4"
```

### Task 10: `mnm documents {show, full, chunks}` — full namespace

**Files:**
- Modify: `crates/mn-cli/src/commands/documents/mod.rs`
- Create: `crates/mn-cli/src/commands/documents/show.rs`
- Create: `crates/mn-cli/src/commands/documents/full.rs`
- Create: `crates/mn-cli/src/commands/documents/chunks.rs`
- Create: `crates/mn-cli/tests/documents_cli.rs`

- [ ] **Step 1: Write the failing tests**

`crates/mn-cli/tests/documents_cli.rs`:

```rust
use std::process::Command;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn overview_json() -> serde_json::Value {
    serde_json::json!({
        "id": "33333333-3333-3333-3333-333333333333",
        "source_version_id": "22222222-2222-2222-2222-222222222222",
        "node_id": "44444444-4444-4444-4444-444444444444",
        "source_path": "welcome.md",
        "published_url": "https://example.com/welcome/",
        "source_url": null,
        "language": "markdown",
        "kind": "markdown",
        "content_hash": "sha256:abc",
        "char_count": 100,
        "token_count": 20,
        "source_modified_at": null,
        "created_at": "2026-05-25T00:00:00Z",
        "frontmatter": null,
        "provenance": {},
        "package_id": null,
        "source": { "slug": "smoke" },
        "chunk_ids": [
            "11111111-1111-1111-1111-111111111111",
            "11111111-1111-1111-1111-111111111112"
        ]
    })
}

#[tokio::test]
async fn documents_show_renders_overview() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/documents/33333333-3333-3333-3333-333333333333"))
        .respond_with(ResponseTemplate::new(200).set_body_json(overview_json()))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["--server", &server.uri(), "documents", "show",
               "33333333-3333-3333-3333-333333333333"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("welcome.md"));
    assert!(stdout.contains("2 chunks") || stdout.contains("chunk_ids"));
}

#[tokio::test]
async fn documents_full_translates_412_to_friendly_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/documents/[0-9a-f-]+/full$"))
        .respond_with(
            ResponseTemplate::new(412).set_body_json(serde_json::json!({
                "error": "too_many_chunks",
                "chunk_count": 1240,
                "cap": 500,
                "hint": "Use GET /v1/documents/33333333-3333-3333-3333-333333333333/chunks?from=K&limit=L (default L=20)"
            }))
        )
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["--server", &server.uri(), "documents", "full",
               "33333333-3333-3333-3333-333333333333"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr);
    assert!(combined.contains("1240"), "expected chunk count in error: {combined}");
    assert!(combined.contains("--from") || combined.contains("documents chunks"),
            "expected window suggestion: {combined}");
}

#[tokio::test]
async fn documents_chunks_renders_window() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "id": "33333333-3333-3333-3333-333333333333",
        "source_path": "welcome.md",
        "source": { "slug": "smoke" },
        "from": 3,
        "limit": 2,
        "total_chunks": 10,
        "chunks": [
            { "chunk_id": "11111111-1111-1111-1111-111111111111",
              "chunk_index": 3, "content": "third chunk", "heading_path": [], "token_count": 5 },
            { "chunk_id": "11111111-1111-1111-1111-111111111112",
              "chunk_index": 4, "content": "fourth chunk", "heading_path": [], "token_count": 5 }
        ]
    });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/documents/[0-9a-f-]+/chunks$"))
        .and(query_param("from", "3"))
        .and(query_param("limit", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["--server", &server.uri(), "documents", "chunks",
               "33333333-3333-3333-3333-333333333333",
               "--from", "3", "--limit", "2"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("3..5 of 10") || stdout.contains("3..=4 of 10")
            || stdout.contains("chunks 3..5 of 10 total"),
            "expected window header: {stdout}");
}
```

- [ ] **Step 2: Run, confirm fails**

Run: `cargo test -p mn-cli --test documents_cli`
Expected: 3 FAIL.

- [ ] **Step 3: Implement `documents/show.rs`**

```rust
//! `mnm documents show <doc-id>` — render the overview.

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use uuid::Uuid;

#[derive(Debug, ClapArgs)]
pub struct Args {
    pub document_id: Uuid,
}

pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let url = format!("{server_url}/v1/documents/{}", args.document_id);
    let body = super::fetch(&url).await?;
    if json {
        println!("{body}");
    } else {
        render_overview(&body)?;
    }
    Ok(())
}

fn render_overview(body: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(body).context("parse response body")?;
    let path = v["source_path"].as_str().unwrap_or("?");
    let slug = v["source"]["slug"].as_str().unwrap_or("?");
    let url = v["published_url"].as_str().unwrap_or("(none)");
    let lang = v["language"].as_str().unwrap_or("?");
    let chunk_ids = v["chunk_ids"].as_array().cloned().unwrap_or_default();

    println!("document: {slug}/{path}");
    println!("URL:      {url}");
    println!("language: {lang}");
    println!("chunks:   {} chunks", chunk_ids.len());
    println!();
    for (i, id) in chunk_ids.iter().enumerate() {
        println!("  {}. chunk_index={i}  id={}", i + 1, id.as_str().unwrap_or(""));
    }
    Ok(())
}
```

- [ ] **Step 4: Implement `documents/full.rs`**

```rust
//! `mnm documents full <doc-id>` — render the complete document with chunks.

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use uuid::Uuid;

#[derive(Debug, ClapArgs)]
pub struct Args {
    pub document_id: Uuid,
}

pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let url = format!("{server_url}/v1/documents/{}/full", args.document_id);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build HTTP client")?;
    let resp = client.get(&url).send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::PRECONDITION_FAILED {
        let v: serde_json::Value = serde_json::from_str(&body).context("parse 412 body")?;
        let count = v["chunk_count"].as_u64().unwrap_or(0);
        let cap = v["cap"].as_u64().unwrap_or(0);
        return Err(anyhow::anyhow!(
            "document has {count} chunks (cap {cap}). Use:\n  mnm documents chunks {} --from 0 --limit 100",
            args.document_id
        ));
    }
    if !status.is_success() {
        return Err(anyhow::anyhow!("{status} from {url}: {body}"));
    }
    if json {
        println!("{body}");
    } else {
        render_full(&body)?;
    }
    Ok(())
}

fn render_full(body: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(body).context("parse response body")?;
    let path = v["source_path"].as_str().unwrap_or("?");
    let slug = v["source"]["slug"].as_str().unwrap_or("?");
    let url = v["published_url"].as_str().unwrap_or("(none)");
    let chunks = v["chunks"].as_array().cloned().unwrap_or_default();

    println!("document: {slug}/{path}");
    println!("URL:      {url}");
    println!("chunks:   {} chunks", chunks.len());
    println!();
    for (i, c) in chunks.iter().enumerate() {
        let chunk_index = c["chunk_index"].as_i64().unwrap_or(0);
        let content = c["content"].as_str().unwrap_or("");
        let headings: Vec<&str> = c["heading_path"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        println!("--- chunk {}/{} (index {chunk_index}) ---", i + 1, chunks.len());
        if !headings.is_empty() {
            println!("heading: > {}", headings.join(" > "));
        }
        println!("{content}");
        println!();
    }
    Ok(())
}
```

- [ ] **Step 5: Implement `documents/chunks.rs`**

```rust
//! `mnm documents chunks <doc-id> --from K --limit N` — render a windowed slice.

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use uuid::Uuid;

#[derive(Debug, ClapArgs)]
pub struct Args {
    pub document_id: Uuid,
    #[arg(long, default_value_t = 0)]
    pub from: usize,
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let url = format!(
        "{server_url}/v1/documents/{}/chunks?from={}&limit={}",
        args.document_id, args.from, args.limit
    );
    let body = super::fetch(&url).await?;
    if json {
        println!("{body}");
    } else {
        render_window(&body)?;
    }
    Ok(())
}

fn render_window(body: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(body).context("parse response body")?;
    let from = v["from"].as_u64().unwrap_or(0);
    let limit = v["limit"].as_u64().unwrap_or(0);
    let total = v["total_chunks"].as_u64().unwrap_or(0);
    let chunks = v["chunks"].as_array().cloned().unwrap_or_default();
    let to = from + (chunks.len() as u64);

    println!("chunks {from}..{to} of {total} total");
    if chunks.is_empty() {
        println!("(none in range)");
        return Ok(());
    }
    for c in &chunks {
        let chunk_index = c["chunk_index"].as_i64().unwrap_or(0);
        let content = c["content"].as_str().unwrap_or("");
        println!("--- chunk_index {chunk_index} ---");
        println!("{content}");
        println!();
    }
    Ok(())
}
```

- [ ] **Step 6: Wire the dispatcher**

Replace `crates/mn-cli/src/commands/documents/mod.rs` with:

```rust
//! `mnm documents <subcommand>` dispatcher.

use anyhow::{Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::TelemetryClient;

pub mod show;
pub mod full;
pub mod chunks;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: DocumentsCmd,
}

#[derive(Debug, Subcommand)]
pub enum DocumentsCmd {
    Show(show::Args),
    Full(full::Args),
    Chunks(chunks::Args),
}

pub async fn run(
    args: Args,
    server: Option<&str>,
    _telemetry: &TelemetryClient,
    _cli_version: &str,
    json: bool,
) -> Result<()> {
    match args.cmd {
        DocumentsCmd::Show(a) => show::run(a, server, json).await,
        DocumentsCmd::Full(a) => full::run(a, server, json).await,
        DocumentsCmd::Chunks(a) => chunks::run(a, server, json).await,
    }
}

/// Shared GET helper used by show + chunks (full has its own because it
/// handles 412 specially).
pub(super) async fn fetch(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build HTTP client")?;
    let resp = client.get(url).send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::anyhow!("{status} from {url}: {body}"));
    }
    Ok(body)
}
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p mn-cli --test documents_cli`
Expected: 3 PASS.

- [ ] **Step 8: Run the full workspace suite**

Run: `cargo test --workspace`
Expected: every test passes.

- [ ] **Step 9: Commit**

```bash
git add crates/mn-cli/
git commit -m "feat(mn-cli): mnm documents {show, full, chunks}

Three verbs in the new always-visible documents namespace. show
renders the overview with chunk_ids; full renders metadata + every
chunk inline (translates server 412 into a friendly 'use --from/--limit'
hint); chunks renders the windowed slice with a 'chunks K..N of M
total' header. --json on every verb dumps the server response.

Spec: docs/superpowers/specs/2026-05-25-chunk-document-navigation-design.md §4"
```

---

## Phase 4 — Cleanup

### Task 11: Workspace cleanup pass

**Files:** any that fmt or clippy flags.

- [ ] **Step 1: Format check**

Run: `cargo fmt --check`
If unformatted, run `cargo fmt`.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Fix any warnings. Common categories: `missing_docs` on new public items (add one-line doc comments), `dead_code` from helpers that didn't end up used (delete), `option_if_let_else` etc. (apply suggested rewrite if it doesn't change behavior).

- [ ] **Step 3: Workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Integration tests** (if testcontainers / Postgres available)

Run: `cargo test --workspace --features integration`
Expected: PASS, or skip with note if Docker/Postgres unavailable.

- [ ] **Step 5: Commit if needed**

```bash
git add -A
git commit -m "chore: clean up lints after chunk+document navigation"
```

Skip the commit if nothing was flagged.

### Task 12: Manual smoke test against deployed server (operator action)

Not a code change. After the PR merges and Fly redeploys, run:

```bash
HOST=https://midnight-manual.midnightntwrk.expert

# 1. Find a chunk via search.
mnm search "welcome placeholder fixture"

# 2. Look at it with bundled context.
mnm chunks show <chunk-id-from-step-1>

# 3. Walk forward.
mnm chunks next <chunk-id-from-step-1> --count 2

# 4. Walk backward.
mnm chunks prev <chunk-id-from-step-1> --count 2

# 5. Inspect the document.
mnm documents show <document-id-from-step-2>

# 6. Pull the whole document.
mnm documents full <document-id-from-step-2>

# 7. Page through the document.
mnm documents chunks <document-id-from-step-2> --from 0 --limit 5
```

Pass criteria: every verb returns exit 0 with a rendered output; the chunk-detail and document responses include non-null `published_url`. Confirms the navigation surface works end-to-end against real data + that the F-bug stays fixed.

---

## Self-review notes

(Filled in during the self-review pass — see commit history if anything needed adjusting.)

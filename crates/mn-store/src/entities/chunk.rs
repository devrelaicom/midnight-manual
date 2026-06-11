//! `chunk` entity queries.
//!
//! Chunks carry the FTS vector (`tsvector`, GENERATED STORED) and the
//! embedding (`vector(1024)`); both are written at insert time. The trigger
//! `trg_chunk_embedding_model_match` ensures `chunk.embedding_model_id`
//! always matches the owning `source_version.embedding_model_id` (FR-002).

use mn_core::types::{Chunk, ChunkStatus};
use pgvector::Vector;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{Result, StoreError};

/// Document subset bundled into chunk read responses.
///
/// Intentionally smaller than the full `Document` row — only the fields
/// useful for navigation/inspection. Spec §1.1 of the chunk+document
/// navigation design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSummary {
    /// Document UUID.
    pub id: Uuid,
    /// Repo-relative source path.
    pub source_path: String,
    /// Public published URL, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_url: Option<String>,
    /// Public source URL, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// ISO language tag, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Document kind discriminator.
    pub kind: mn_core::types::DocumentKind,
    /// Materialized provenance JSON.
    pub provenance: serde_json::Value,
}

/// Source subset bundled into chunk read responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSummary {
    /// URL-safe stable handle, e.g. `compact-docs`.
    pub slug: String,
    /// Human-readable name, e.g. `Compact Docs`.
    pub display_name: String,
}

/// Chunk + bundled document + source context returned by the navigation
/// read endpoints (`GET /v1/chunks/:id`, `/next`, `/prev`).
///
/// The existing chunk fields stay at the top level via `#[serde(flatten)]`
/// so callers that deserialize into a struct containing only chunk fields
/// still work (they ignore the extra `document` and `source` keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkWithContext {
    /// The chunk itself.
    #[serde(flatten)]
    pub chunk: Chunk,
    /// Parent document summary.
    pub document: DocumentSummary,
    /// Owning source summary.
    pub source: SourceSummary,
}

/// Parameters for inserting a new chunk — grouped because the table is wide.
#[derive(Debug, Clone)]
pub struct NewChunk<'a> {
    /// Owning source_version.
    pub source_version_id: Uuid,
    /// Owning document.
    pub document_id: Uuid,
    /// Owning node (kind=chunk).
    pub node_id: Uuid,
    /// 0-indexed position within the document's chunks.
    pub chunk_index: i32,
    /// Total chunks in the parent document.
    pub total_chunks: i32,
    /// Chunk content (text/markdown/code).
    pub content: &'a str,
    /// SHA-256 of `content`.
    pub content_hash: &'a str,
    /// Embedding vector. Must have exactly 1024 dimensions (the `voyage-code-3`
    /// width) when present; `None` for not-yet-embedded chunks.
    pub embedding: Option<Vec<f32>>,
    /// Embedding model used to produce `embedding`. MUST match the owning
    /// source_version's embedding_model_id (trigger-enforced).
    pub embedding_model_id: Uuid,
    /// Markdown heading path.
    pub heading_path: &'a [String],
    /// Code-symbol path (structured, persisted as JSONB).
    pub symbol_path: &'a [mn_core::types::SymbolSegment],
    /// Start byte in source.
    pub start_byte: i32,
    /// End byte in source.
    pub end_byte: i32,
    /// Best-effort token count.
    pub token_count: i32,
    /// Lifecycle state.
    pub status: ChunkStatus,
}

/// Insert a chunk row, returning the newly-minted id.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::ForeignKeyViolation`] for unknown FKs,
/// or [`crate::error::StoreError::CheckViolation`] when the embedding-model
/// trigger rejects a mismatched chunk (EC-10).
pub async fn insert(pool: &PgPool, c: NewChunk<'_>) -> Result<Uuid> {
    let status_str = match c.status {
        ChunkStatus::Ready => "ready",
        ChunkStatus::EmbedFailed => "embed_failed",
        ChunkStatus::Deprecated => "deprecated",
    };
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO chunk ( \
            source_version_id, document_id, node_id, chunk_index, total_chunks, content, \
            content_hash, embedding, embedding_model_id, heading_path, symbol_path, \
            start_byte, end_byte, token_count, status \
         ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) RETURNING id",
    )
    .bind(c.source_version_id)
    .bind(c.document_id)
    .bind(c.node_id)
    .bind(c.chunk_index)
    .bind(c.total_chunks)
    .bind(c.content)
    .bind(c.content_hash)
    .bind(c.embedding.map(Vector::from))
    .bind(c.embedding_model_id)
    .bind(c.heading_path)
    .bind(sqlx::types::Json(c.symbol_path))
    .bind(c.start_byte)
    .bind(c.end_byte)
    .bind(c.token_count)
    .bind(status_str)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// One row returned by [`list_embed_failed_batch`] — the minimum needed for
/// the embedder worker to encode the text and write the vector back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedFailedChunk {
    /// Chunk id (PK).
    pub id: Uuid,
    /// Verbatim chunk content to embed.
    pub content: String,
    /// Owning embedding model — guards a cross-model write.
    pub embedding_model_id: Uuid,
}

/// Fetch up to `limit` `embed_failed` chunks whose `embedding_model_id`
/// matches `model_id`. Used by the background embedder worker.
///
/// `source_version_filter` is `None` in production (the worker drains every
/// pending chunk across all source_versions sharing the model). Integration
/// tests pass `Some(sv_id)` so a concurrent sibling test running against the
/// same shared CI Postgres doesn't pollute the batch.
///
/// Rows are ordered by `created_at ASC` so older work goes first
/// (fair-queue semantics — a fresh ingest doesn't starve an in-flight one).
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn list_embed_failed_batch(
    pool: &PgPool,
    model_id: Uuid,
    source_version_filter: Option<Uuid>,
    limit: i64,
) -> Result<Vec<EmbedFailedChunk>> {
    let rows: Vec<(Uuid, String, Uuid)> = sqlx::query_as(
        "SELECT id, content, embedding_model_id FROM chunk \
         WHERE status = 'embed_failed' AND embedding_model_id = $1 \
           AND ($2::uuid IS NULL OR source_version_id = $2::uuid) \
         ORDER BY created_at ASC \
         LIMIT $3",
    )
    .bind(model_id)
    .bind(source_version_filter)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, content, embedding_model_id)| EmbedFailedChunk {
            id,
            content,
            embedding_model_id,
        })
        .collect())
}

/// Set the embedding bytes and flip `status` to `ready` for one chunk.
///
/// No-ops (returns `Ok(false)`) if the row is no longer in `embed_failed` —
/// another worker may have raced us, or the chunk may have been demoted to
/// `deprecated`. The `trg_chunk_embedding_model_match` trigger is honoured
/// by only updating the embedding column (the column under cross-check is
/// never mutated by this query).
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure (e.g.
/// vector dimensionality mismatch).
pub async fn set_embedding(pool: &PgPool, id: Uuid, vector: Vec<f32>) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE chunk SET embedding = $1, status = 'ready' \
         WHERE id = $2 AND status = 'embed_failed'",
    )
    .bind(Vector::from(vector))
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Fetch a chunk by id (status filter applied — embed_failed rows are excluded).
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if id is unknown OR the
/// chunk has `status = 'embed_failed'`.
pub async fn get_by_id_ready(pool: &PgPool, id: Uuid) -> Result<Chunk> {
    let row = sqlx::query_as::<_, ChunkRow>(
        "SELECT id, source_version_id, document_id, node_id, chunk_index, total_chunks, \
                content, content_hash, embedding_model_id, heading_path, symbol_path, \
                start_byte, end_byte, token_count, status, created_at \
         FROM chunk WHERE id = $1 AND status <> 'embed_failed'",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

/// Admin-facing variant: returns the chunk regardless of `status`. Used by
/// `mnm doctor` / `mnm chunks list --include-failed` (Phase 8).
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if id is unknown.
pub async fn get_by_id_admin(pool: &PgPool, id: Uuid) -> Result<Chunk> {
    let row = sqlx::query_as::<_, ChunkRow>(
        "SELECT id, source_version_id, document_id, node_id, chunk_index, total_chunks, \
                content, content_hash, embedding_model_id, heading_path, symbol_path, \
                start_byte, end_byte, token_count, status, created_at \
         FROM chunk WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

/// Carry-forward snapshot of a chunk — all the fields the ingest handler
/// needs to clone a chunk row into a new `source_version` without re-running
/// the embedder.
#[derive(Debug, Clone, PartialEq)]
pub struct CarryForwardChunk {
    /// Verbatim chunk text.
    pub content: String,
    /// SHA-256 over `content`.
    pub content_hash: String,
    /// Existing embedding vector (may be `None` if the prior chunk was in
    /// `embed_failed`).
    pub embedding: Option<Vec<f32>>,
    /// Embedding model id from the prior version — must match the new SV's
    /// embedding model (trigger-enforced).
    pub embedding_model_id: Uuid,
    /// Markdown heading path.
    pub heading_path: Vec<String>,
    /// Code-symbol path (structured).
    pub symbol_path: Vec<mn_core::types::SymbolSegment>,
    /// 0-indexed chunk position.
    pub chunk_index: i32,
    /// Total chunks in the parent document.
    pub total_chunks: i32,
    /// Start byte in source.
    pub start_byte: i32,
    /// End byte in source.
    pub end_byte: i32,
    /// Best-effort token count.
    pub token_count: i32,
    /// Lifecycle state.
    pub status: ChunkStatus,
}

/// List every chunk under a document for carry-forward cloning. Includes
/// `embed_failed` rows so a doc with a failed chunk doesn't silently lose it
/// across versions.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn list_for_carry_forward(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Vec<CarryForwardChunk>> {
    let rows = sqlx::query_as::<_, CarryForwardRow>(
        "SELECT content, content_hash, embedding, embedding_model_id, heading_path, symbol_path, \
                chunk_index, total_chunks, start_byte, end_byte, token_count, status \
         FROM chunk WHERE document_id = $1 ORDER BY chunk_index",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

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
            s.slug AS s_slug, s.display_name AS s_display_name \
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

/// List the previous `count` chunks before `anchor` in the same document.
///
/// Returned in ascending `chunk_index` (reading) order. SQL fetches the
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
            s.slug AS s_slug, s.display_name AS s_display_name \
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
            s.slug AS s_slug, s.display_name AS s_display_name \
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

/// Fetch many chunks (each with document + source context) in one query.
///
/// Rows come back in arbitrary DB order; callers re-order by input order.
/// Ids that don't exist (or are `embed_failed`) are simply absent.
///
/// # Errors
///
/// Returns `StoreError::Database` on any SQL failure.
pub async fn get_many_with_context(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<ChunkWithContext>> {
    let rows = sqlx::query_as::<_, ChunkWithContextRow>(
        "SELECT \
            c.id, c.source_version_id, c.document_id, c.node_id, c.chunk_index, c.total_chunks, \
            c.content, c.content_hash, c.embedding_model_id, c.heading_path, c.symbol_path, \
            c.start_byte, c.end_byte, c.token_count, c.status, c.created_at, \
            d.source_path AS d_source_path, d.published_url AS d_published_url, \
            d.source_url AS d_source_url, d.language AS d_language, d.kind AS d_kind, \
            d.provenance AS d_provenance, \
            s.slug AS s_slug, s.display_name AS s_display_name \
         FROM chunk c \
         JOIN document d ON c.document_id = d.id \
         JOIN source_version sv ON c.source_version_id = sv.id \
         JOIN source s ON sv.source_id = s.id \
         WHERE c.id = ANY($1) AND c.status <> 'embed_failed'",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

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
    symbol_path: sqlx::types::Json<Vec<mn_core::types::SymbolSegment>>,
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
    // 2 source columns:
    s_slug: String,
    s_display_name: String,
}

impl TryFrom<ChunkWithContextRow> for ChunkWithContext {
    type Error = StoreError;
    fn try_from(r: ChunkWithContextRow) -> Result<Self> {
        // Mirror the existing ChunkRow → Chunk pattern: round-trip the
        // status string through serde_json so the enum's existing serde
        // impl handles the decode.
        let status: ChunkStatus = serde_json::from_value(serde_json::Value::String(r.status))
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
            symbol_path: r.symbol_path.0,
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
            source: SourceSummary {
                slug: r.s_slug,
                display_name: r.s_display_name,
            },
        })
    }
}

#[derive(sqlx::FromRow)]
struct CarryForwardRow {
    content: String,
    content_hash: String,
    embedding: Option<Vector>,
    embedding_model_id: Uuid,
    heading_path: Vec<String>,
    symbol_path: sqlx::types::Json<Vec<mn_core::types::SymbolSegment>>,
    chunk_index: i32,
    total_chunks: i32,
    start_byte: i32,
    end_byte: i32,
    token_count: i32,
    status: String,
}

impl TryFrom<CarryForwardRow> for CarryForwardChunk {
    type Error = crate::error::StoreError;

    fn try_from(r: CarryForwardRow) -> std::result::Result<Self, Self::Error> {
        let status: ChunkStatus = serde_json::from_value(serde_json::Value::String(r.status))
            .map_err(|e| crate::error::StoreError::Json(e.to_string()))?;
        Ok(Self {
            content: r.content,
            content_hash: r.content_hash,
            embedding: r.embedding.map(|v| v.to_vec()),
            embedding_model_id: r.embedding_model_id,
            heading_path: r.heading_path,
            symbol_path: r.symbol_path.0,
            chunk_index: r.chunk_index,
            total_chunks: r.total_chunks,
            start_byte: r.start_byte,
            end_byte: r.end_byte,
            token_count: r.token_count,
            status,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ChunkRow {
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
    symbol_path: sqlx::types::Json<Vec<mn_core::types::SymbolSegment>>,
    start_byte: i32,
    end_byte: i32,
    token_count: i32,
    status: String,
    created_at: OffsetDateTime,
}

impl TryFrom<ChunkRow> for Chunk {
    type Error = crate::error::StoreError;

    fn try_from(r: ChunkRow) -> std::result::Result<Self, Self::Error> {
        let status: ChunkStatus = serde_json::from_value(serde_json::Value::String(r.status))
            .map_err(|e| crate::error::StoreError::Json(e.to_string()))?;
        Ok(Self {
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
            symbol_path: r.symbol_path.0,
            start_byte: r.start_byte,
            end_byte: r.end_byte,
            token_count: r.token_count,
            status,
            created_at: r.created_at,
        })
    }
}

/// Read a chunk's structured symbol path. Test/diagnostic helper.
///
/// # Errors
/// Propagates query errors.
pub async fn symbol_path_of(pool: &PgPool, id: Uuid) -> Result<Vec<mn_core::types::SymbolSegment>> {
    let row: (sqlx::types::Json<Vec<mn_core::types::SymbolSegment>>,) =
        sqlx::query_as("SELECT symbol_path FROM chunk WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?;
    Ok(row.0 .0)
}

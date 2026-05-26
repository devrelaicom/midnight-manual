//! `document` entity queries.

use mn_core::provenance::Provenance;
use mn_core::types::{Document, DocumentKind};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::Result;

/// Parameters for inserting a new document — grouped to keep call sites
/// readable since the table has many columns.
#[derive(Debug, Clone)]
pub struct NewDocument<'a> {
    /// Owning source_version.
    pub source_version_id: Uuid,
    /// Owning node (kind = document).
    pub node_id: Uuid,
    /// Document kind discriminator.
    pub kind: DocumentKind,
    /// Public source URL (optional).
    pub source_url: Option<&'a str>,
    /// Public published URL (optional).
    pub published_url: Option<&'a str>,
    /// Repo-relative source path.
    pub source_path: &'a str,
    /// ISO language tag, if known.
    pub language: Option<&'a str>,
    /// SHA-256 of normalized content.
    pub content_hash: &'a str,
    /// Last-modified timestamp from the source.
    pub source_modified_at: Option<OffsetDateTime>,
    /// Verbatim frontmatter JSON.
    pub frontmatter: Option<serde_json::Value>,
    /// Materialized provenance.
    pub provenance: &'a Provenance,
    /// Package this document belongs to, if any.
    pub package_id: Option<Uuid>,
    /// Character count of the source content.
    pub char_count: i32,
    /// Token count by the embedding tokenizer.
    pub token_count: i32,
}

/// Insert a document row, returning the newly-minted id.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::ForeignKeyViolation`] if any FK is
/// unknown, or [`crate::error::StoreError::Json`] if provenance fails to
/// serialize.
pub async fn insert(pool: &PgPool, doc: NewDocument<'_>) -> Result<Uuid> {
    let kind_str = match doc.kind {
        DocumentKind::Markdown => "markdown",
        DocumentKind::Code => "code",
        DocumentKind::Plaintext => "plaintext",
    };
    let provenance_json = serde_json::to_value(doc.provenance)
        .map_err(|e| crate::error::StoreError::Json(e.to_string()))?;
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO document ( \
            source_version_id, node_id, kind, source_url, published_url, source_path, language, \
            content_hash, source_modified_at, frontmatter, provenance, package_id, char_count, token_count \
         ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING id",
    )
    .bind(doc.source_version_id)
    .bind(doc.node_id)
    .bind(kind_str)
    .bind(doc.source_url)
    .bind(doc.published_url)
    .bind(doc.source_path)
    .bind(doc.language)
    .bind(doc.content_hash)
    .bind(doc.source_modified_at)
    .bind(doc.frontmatter)
    .bind(provenance_json)
    .bind(doc.package_id)
    .bind(doc.char_count)
    .bind(doc.token_count)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Fetch a document by id.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if id is unknown.
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> Result<Document> {
    let row = sqlx::query_as::<_, DocumentRow>(
        "SELECT id, source_version_id, node_id, kind, source_url, published_url, source_path, \
                language, content_hash, source_modified_at, frontmatter, provenance, \
                package_id, char_count, token_count, created_at \
         FROM document WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

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

/// One row from [`list_for_source_version`] — the minimum needed to seed an
/// [`IngestPlanBuilder`'s prior state](super) (FR-014 carry-forward).
///
/// [`IngestPlanBuilder`]: ../../../mn_content/ingest/struct.PlanBuilder.html
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSummary {
    /// Document id.
    pub id: Uuid,
    /// Repo-relative source path.
    pub source_path: String,
    /// Normalized content hash.
    pub content_hash: String,
}

/// Document overview returned by `GET /v1/documents/:id` — full document
/// row + the source's slug + ordered chunk_ids. No chunk bodies.
///
/// Spec §1.3 of the chunk+document navigation design.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentOverview {
    /// Full document row.
    #[serde(flatten)]
    pub document: Document,
    /// Source summary (slug only).
    pub source: crate::entities::chunk::SourceSummary,
    /// Document's ready chunk IDs in chunk_index order (excluding embed_failed).
    pub chunk_ids: Vec<Uuid>,
}

/// One chunk body in a document-full or document-window response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkBody {
    /// Chunk UUID.
    pub chunk_id: Uuid,
    /// 0-indexed position within the document's chunks.
    pub chunk_index: i32,
    /// Chunk content (text/markdown/code).
    pub content: String,
    /// Markdown heading path.
    pub heading_path: Vec<String>,
    /// Best-effort token count.
    pub token_count: i32,
}

/// Complete document: metadata + every ready chunk inline.
///
/// Spec §1.4 of the chunk+document navigation design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFull {
    /// Full document row.
    #[serde(flatten)]
    pub document: Document,
    /// Source summary (slug only).
    pub source: crate::entities::chunk::SourceSummary,
    /// Every ready chunk in chunk_index order.
    pub chunks: Vec<ChunkBody>,
}

/// Window of chunks at offset `from` with `limit` cap.
///
/// Spec §1.5 of the chunk+document navigation design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentChunkWindow {
    /// Full document row.
    #[serde(flatten)]
    pub document: Document,
    /// Source summary (slug only).
    pub source: crate::entities::chunk::SourceSummary,
    /// Requested chunk window in chunk_index order.
    pub chunks: Vec<ChunkBody>,
    /// Start offset (chunk index) of the requested window.
    pub from: usize,
    /// Requested limit; actual chunks returned may be less if end of document.
    pub limit: usize,
    /// Total number of ready chunks in the document.
    pub total_chunks: usize,
}

/// Returned by `get_full` when the document exceeds the chunk cap, so the
/// route can map to a 412 without paying the cost of a full chunk fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullResult {
    /// Document with metadata + all ready chunks.
    Document(DocumentFull),
    /// Document exceeds cap; count and cap are reported for diagnostics.
    TooManyChunks {
        /// Actual number of ready chunks.
        count: usize,
        /// Maximum allowed chunks.
        cap: usize,
    },
}

/// List every document under a given `source_version`, returning the minimal
/// fields needed to drive carry-forward decisions.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn list_for_source_version(
    pool: &PgPool,
    source_version_id: Uuid,
) -> Result<Vec<DocumentSummary>> {
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, source_path, content_hash FROM document WHERE source_version_id = $1 \
         ORDER BY source_path",
    )
    .bind(source_version_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, source_path, content_hash)| DocumentSummary { id, source_path, content_hash })
        .collect())
}

/// Look up an existing document in a source_version by its `content_hash` —
/// powers the FR-014 incremental re-ingest optimization (carry forward embedding
/// bytes for unchanged content).
///
/// Returns `Some(id)` if a matching row exists, `None` otherwise.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn find_by_hash(
    pool: &PgPool,
    source_version_id: Uuid,
    content_hash: &str,
) -> Result<Option<Uuid>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM document WHERE source_version_id = $1 AND content_hash = $2",
    )
    .bind(source_version_id)
    .bind(content_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

#[derive(sqlx::FromRow)]
struct DocumentRow {
    id: Uuid,
    source_version_id: Uuid,
    node_id: Uuid,
    kind: String,
    source_url: Option<String>,
    published_url: Option<String>,
    source_path: String,
    language: Option<String>,
    content_hash: String,
    source_modified_at: Option<OffsetDateTime>,
    frontmatter: Option<serde_json::Value>,
    provenance: serde_json::Value,
    package_id: Option<Uuid>,
    char_count: i32,
    token_count: i32,
    created_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct ChunkBodyRow {
    chunk_id: Uuid,
    chunk_index: i32,
    content: String,
    heading_path: Vec<String>,
    token_count: i32,
}

impl TryFrom<DocumentRow> for Document {
    type Error = crate::error::StoreError;

    fn try_from(r: DocumentRow) -> std::result::Result<Self, Self::Error> {
        let kind: DocumentKind = serde_json::from_value(serde_json::Value::String(r.kind))
            .map_err(|e| crate::error::StoreError::Json(e.to_string()))?;
        let provenance: Provenance = serde_json::from_value(r.provenance)
            .map_err(|e| crate::error::StoreError::Json(e.to_string()))?;
        Ok(Self {
            id: r.id,
            source_version_id: r.source_version_id,
            node_id: r.node_id,
            kind,
            source_url: r.source_url,
            published_url: r.published_url,
            source_path: r.source_path,
            language: r.language,
            content_hash: r.content_hash,
            source_modified_at: r.source_modified_at,
            frontmatter: r.frontmatter,
            provenance,
            package_id: r.package_id,
            char_count: r.char_count,
            token_count: r.token_count,
            created_at: r.created_at,
        })
    }
}

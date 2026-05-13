//! `document` entity queries.

use mn_core::provenance::Provenance;
use mn_core::types::{Document, DocumentKind};
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

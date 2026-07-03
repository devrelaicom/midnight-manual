//! `document` entity queries.

use mnm_core::provenance::Provenance;
use mnm_core::types::{Document, DocumentKind};
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

/// Get a document overview: full document row + source slug + ordered
/// chunk skeletons (`{id, chunk_index, token_count}` plus each chunk's
/// `heading_path` and primary `symbol` — an outline, no bodies).
///
/// # Errors
///
/// Returns `StoreError::NotFound` if no document has that id.
pub async fn get_overview(pool: &PgPool, id: Uuid) -> Result<DocumentOverview> {
    let document = get_by_id(pool, id).await?;
    let (source_slug, source_display_name) = sqlx::query_as::<_, (String, String)>(
        "SELECT s.slug, s.display_name FROM source s \
         JOIN source_version sv ON sv.source_id = s.id \
         WHERE sv.id = $1",
    )
    .bind(document.source_version_id)
    .fetch_one(pool)
    .await?;
    let chunks = sqlx::query_as::<_, ChunkSkeletonRow>(
        "SELECT id, chunk_index, token_count, heading_path, symbol_path FROM chunk \
         WHERE document_id = $1 AND status <> 'embed_failed' \
         ORDER BY chunk_index ASC",
    )
    .bind(id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(ChunkSkeletonRow::into_skeleton)
    .collect();
    Ok(DocumentOverview {
        document,
        source: crate::entities::chunk::SourceSummary {
            slug: source_slug,
            display_name: source_display_name,
        },
        chunks,
    })
}

/// Get a windowed slice of a document's chunks.
///
/// Starts at chunk index `from`, returning up to `limit` chunks. Also returns
/// the document metadata and total ready-chunk count so callers can render
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
    let (source_slug, source_display_name) = sqlx::query_as::<_, (String, String)>(
        "SELECT s.slug, s.display_name FROM source s \
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
        "SELECT id AS chunk_id, chunk_index, content, heading_path, symbol_path, token_count \
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
        symbol_path: r.symbol_path.0,
        token_count: r.token_count,
    })
    .collect();
    Ok(DocumentChunkWindow {
        document,
        source: crate::entities::chunk::SourceSummary {
            slug: source_slug,
            display_name: source_display_name,
        },
        chunks,
        from,
        limit,
        total_chunks: usize::try_from(total).unwrap_or(0),
    })
}

/// One row from [`list_for_source_version`] — the minimum needed to seed an
/// [`IngestPlanBuilder`'s prior state](super) (FR-014 carry-forward).
///
/// [`IngestPlanBuilder`]: ../../../mnm_content/ingest/struct.PlanBuilder.html
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
/// row + the source's slug + ordered chunk skeletons. No chunk bodies.
///
/// Spec §1.3 of the chunk+document navigation design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentOverview {
    /// Full document row.
    #[serde(flatten)]
    pub document: Document,
    /// Source summary (slug only).
    pub source: crate::entities::chunk::SourceSummary,
    /// Ready chunk skeletons in chunk_index order (excluding embed_failed),
    /// each carrying its heading/symbol outline breadcrumbs (issue #141).
    pub chunks: Vec<ChunkSkeleton>,
}

/// Per-chunk skeleton entry in a document overview: position + cost, no body.
///
/// Plus light structural breadcrumbs — the markdown `heading_path` and, for code
/// chunks, the primary (`{kind, name}`) symbol — that turn the ordered skeleton
/// into a document outline / table of contents (issue #141). Both breadcrumb
/// fields are omitted from the wire when empty, so a plaintext (or heading-less)
/// chunk serializes exactly as it did before this enrichment
/// (`{id, chunk_index, token_count}`) — plaintext documents are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkSkeleton {
    /// Chunk id (feed to `GET /v1/chunks?ids=`).
    pub id: Uuid,
    /// Position within the document.
    pub chunk_index: i32,
    /// Token count of the chunk body.
    pub token_count: i32,
    /// Markdown heading breadcrumb for this chunk (outermost first). Empty —
    /// and omitted on the wire — for code and plaintext chunks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    /// The chunk's primary code symbol (`{kind, name}`), when it has one — the
    /// outline label for a code chunk. `None` (and omitted) for prose chunks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<PrimarySymbol>,
}

/// A chunk's primary code symbol (`{kind, name}`): the leading `symbol_path`
/// segment, used as the chunk's label in a document outline (issue #141).
///
/// A deliberately trimmed view of [`mnm_core::types::SymbolSegment`] — no
/// ancestor `path`: the overview is a table of contents, not a symbol dump. The
/// full structured `symbol_path` (with `kind`, `name`, and ancestor `path` per
/// segment) is still available per chunk via `get_document_chunks` / the
/// `get_chunks` family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimarySymbol {
    /// Syntactic kind of the symbol (`impl`, `fn`, `class`, `key`, …).
    pub kind: String,
    /// Identifier or label for the symbol.
    pub name: String,
}

/// Raw `get_overview` chunk row: the skeleton columns plus the `text[]`
/// heading_path and JSONB `symbol_path`, mapped down to [`ChunkSkeleton`]'s
/// primary-symbol view. Mirrors the `ChunkBodyRow` → `ChunkBody` pattern used
/// by [`list_chunks_window`], keeping the JSONB decode in one typed place.
#[derive(sqlx::FromRow)]
struct ChunkSkeletonRow {
    id: Uuid,
    chunk_index: i32,
    token_count: i32,
    heading_path: Vec<String>,
    symbol_path: sqlx::types::Json<Vec<mnm_core::types::SymbolSegment>>,
}

impl ChunkSkeletonRow {
    /// Collapse the full structured `symbol_path` to just its leading segment as
    /// the outline's primary `{kind, name}` symbol (dropping the ancestor
    /// `path`), leaving `heading_path` untouched.
    fn into_skeleton(self) -> ChunkSkeleton {
        ChunkSkeleton {
            id: self.id,
            chunk_index: self.chunk_index,
            token_count: self.token_count,
            heading_path: self.heading_path,
            symbol: self
                .symbol_path
                .0
                .into_iter()
                .next()
                .map(|s| PrimarySymbol { kind: s.kind, name: s.name }),
        }
    }
}

/// One chunk body in a document-window response.
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
    /// Code-symbol path leading to this chunk — structured `{kind, name, path}`
    /// segments (empty for prose chunks). Carried here so the windowed body-read
    /// path keeps the same code-symbol context as the `get_chunks` family
    /// (issue #132); mirrors [`mnm_core::types::Chunk::symbol_path`].
    #[serde(default)]
    pub symbol_path: Vec<mnm_core::types::SymbolSegment>,
    /// Best-effort token count.
    pub token_count: i32,
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

/// One document in a version's inventory, with whether every chunk is embedded.
#[derive(Debug, Clone)]
pub struct InventoryDoc {
    /// Repo-relative source path.
    pub source_path: String,
    /// SHA-256 of normalized content.
    pub content_hash: String,
    /// Document UUID.
    pub document_id: Uuid,
    /// True iff the document has ≥1 chunk and no chunk is `embed_failed`.
    pub embed_complete: bool,
}

/// List the documents of a source_version with a per-document embed-complete
/// rollup, for the prior-state inventory endpoint.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn list_active_inventory(
    pool: &PgPool,
    source_version_id: Uuid,
) -> Result<Vec<InventoryDoc>> {
    let rows: Vec<(String, String, Uuid, bool)> = sqlx::query_as(
        "SELECT d.source_path, d.content_hash, d.id, \
                (COUNT(c.id) > 0 AND COUNT(c.id) FILTER (WHERE c.status = 'embed_failed') = 0) \
            FROM document d \
            LEFT JOIN chunk c ON c.document_id = d.id \
            WHERE d.source_version_id = $1 \
            GROUP BY d.id, d.source_path, d.content_hash \
            ORDER BY d.source_path",
    )
    .bind(source_version_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(source_path, content_hash, document_id, embed_complete)| InventoryDoc {
            source_path,
            content_hash,
            document_id,
            embed_complete,
        })
        .collect())
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
    symbol_path: sqlx::types::Json<Vec<mnm_core::types::SymbolSegment>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #141 (S4): the outline breadcrumbs are `skip_serializing_if`-empty,
    /// so a heading-less, symbol-less skeleton entry serializes to exactly the
    /// pre-#141 shape (`{id, chunk_index, token_count}`) — the load-bearing
    /// "plaintext documents are unchanged" claim. A populated entry carries them.
    #[test]
    fn chunk_skeleton_omits_empty_breadcrumbs_keeps_populated() {
        let bare = ChunkSkeleton {
            id: Uuid::nil(),
            chunk_index: 0,
            token_count: 7,
            heading_path: vec![],
            symbol: None,
        };
        let v = serde_json::to_value(&bare).expect("serialize bare skeleton");
        let obj = v.as_object().expect("skeleton serializes to an object");
        assert!(obj.get("heading_path").is_none(), "empty heading_path must be omitted");
        assert!(obj.get("symbol").is_none(), "absent symbol must be omitted");
        // Exactly the three pre-#141 keys — byte-for-byte the old shape.
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["chunk_index", "id", "token_count"].into_iter().collect(),
            "bare skeleton must carry ONLY the pre-#141 keys"
        );

        let populated = ChunkSkeleton {
            id: Uuid::nil(),
            chunk_index: 1,
            token_count: 12,
            heading_path: vec!["Guide".to_owned(), "Setup".to_owned()],
            symbol: Some(PrimarySymbol {
                kind: "impl".to_owned(),
                name: "Counter".to_owned(),
            }),
        };
        let v = serde_json::to_value(&populated).expect("serialize populated skeleton");
        assert_eq!(v["heading_path"][1], "Setup", "populated heading_path is present");
        assert_eq!(v["symbol"]["kind"], "impl", "populated symbol is present");
        assert_eq!(v["symbol"]["name"], "Counter");
    }
}

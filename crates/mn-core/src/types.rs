//! Canonical wire types — the shapes returned by `/v1/search`, `/v1/chunks/...`, and the MCP `search` tool.
//!
//! Mirror the SQL schema in
//! [`specs/001-rag-platform/data-model.md`](../../../../specs/001-rag-platform/data-model.md)
//! but with the application-layer Rust ergonomics (UUIDs as typed handles, enums
//! for status fields, `Provenance` materialized from the JSONB column, etc.).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::model_id::EmbeddingModelId;
use crate::provenance::Provenance;

/// `source.kind` — the four kinds of logical content source the corpus supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A documentation website (e.g. midnight-docs).
    DocsSite,
    /// A source-code repository.
    CodeRepo,
    /// One-off standalone files.
    Standalone,
    /// A source mixing documentation and code.
    Mixed,
}

/// Stable handle for a logical content source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Database UUID.
    pub id: Uuid,
    /// Human-readable, URL-safe slug.
    pub slug: String,
    /// Display label.
    pub display_name: String,
    /// Source kind.
    pub kind: SourceKind,
    /// Canonical origin URL (git URL, docs site URL, etc.).
    pub origin_url: Option<String>,
    /// How many historical versions to retain (default 5, range 1..=50, D15).
    pub retention_count: i32,
    /// When the source was first registered.
    pub created_at: OffsetDateTime,
    /// If set, the source has been retired and is eligible for sweep.
    pub retired_at: Option<OffsetDateTime>,
}

/// Lifecycle states of a `source_version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceVersionStatus {
    /// An ingest is uploading documents into this version. Not yet promoted.
    Building,
    /// The current authoritative version for its source.
    Active,
    /// Was previously active; not deleted yet (grace window).
    Inactive,
    /// Ingest was aborted before finalize.
    Aborted,
    /// Marked for sweep.
    Retired,
}

/// One immutable snapshot of a source.
///
/// The partial unique index `uniq_source_version_active` guarantees that at
/// most one row per `source_id` has `is_active = true` (FR-003, FR-061, EC-04).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceVersion {
    /// Database UUID.
    pub id: Uuid,
    /// Owning `source.id`.
    pub source_id: Uuid,
    /// Monotonic per-source revision number (1-indexed).
    pub revision: i32,
    /// Lifecycle state.
    pub status: SourceVersionStatus,
    /// True only for the single active row per source.
    pub is_active: bool,
    /// When ingest finalized.
    pub ingested_at: OffsetDateTime,
    /// CLI version that produced the ingest (FR-019 reproducibility).
    pub ingest_cli_version: String,
    /// The embedding model used for every chunk in this version.
    pub embedding_model_id: Uuid,
    /// Aggregate content hash for tamper-detection.
    pub content_hash: String,
    /// Free-form notes captured at ingest time.
    pub notes: Option<String>,
    /// If set, the version has been marked retired and is sweep-eligible.
    pub retired_at: Option<OffsetDateTime>,
}

/// A per-CIDR temporary rate-limit ceiling (`rate_limit_override`, D11/FR-031).
///
/// `cidr` is carried as a `String` because the workspace's sqlx build excludes
/// the `ipnetwork` feature, so the column is cast to/from text at the query
/// boundary. The value is the network address Postgres stored (host bits
/// masked off), e.g. `169.155.237.0/25`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitOverride {
    /// Database UUID.
    pub id: Uuid,
    /// Network block the override applies to, in `addr/prefix` form.
    pub cidr: String,
    /// Requests-per-second ceiling for the block (always positive).
    pub limit_rps: i32,
    /// When the override stops being effective.
    pub expires_at: OffsetDateTime,
    /// Free-form operator note (e.g. an event name).
    pub note: Option<String>,
    /// `user_id` of the admin who created the override (JWT `sub` claim).
    pub created_by: String,
    /// When the override row was inserted.
    pub created_at: OffsetDateTime,
}

/// `embedding_model` registry row — the typed view of the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingModel {
    /// Database UUID.
    pub id: Uuid,
    /// Canonical model name (e.g. `"bge-base-en-v1.5"`).
    pub name: String,
    /// Monotonic per-name revision; combined with `name` forms the
    /// [`EmbeddingModelId`] wire identifier.
    pub revision: i32,
    /// Output dimensionality of the embedder.
    pub dim: i32,
    /// Source / provider tag (e.g. `"baai"`).
    pub provider: String,
    /// When this model was first registered.
    pub created_at: OffsetDateTime,
}

impl EmbeddingModel {
    /// The wire-format `{name}@{revision}` identifier.
    ///
    /// # Errors
    ///
    /// Returns an error only if `revision` is non-positive — the DB constraint
    /// guarantees `revision >= 1`, but typed conversion can still fail if a row
    /// is hand-rolled.
    pub fn wire_id(&self) -> Result<EmbeddingModelId, crate::model_id::ParseEmbeddingModelIdError> {
        EmbeddingModelId::new(
            &self.name,
            u32::try_from(self.revision).map_err(|_| {
                crate::model_id::ParseEmbeddingModelIdError::InvalidRevision(
                    self.revision.to_string(),
                )
            })?,
        )
    }
}

/// `node.kind` — every hierarchy element belongs to exactly one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// The implicit root of a `source_version`'s tree.
    Root,
    /// An intermediate grouping (folder, manifest section).
    Group,
    /// A document node — sits directly above the chunks of one document.
    Document,
    /// A chunk node — leaf in the hierarchy, references the chunk row.
    Chunk,
}

/// Node in the source-version hierarchy tree (`root → groups → documents → chunks`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Database UUID.
    pub id: Uuid,
    /// Owning `source_version.id`.
    pub source_version_id: Uuid,
    /// Parent node, or `None` for the root.
    pub parent_node_id: Option<Uuid>,
    /// Kind discriminator.
    pub kind: NodeKind,
    /// Display name (folder, document title, chunk-#N).
    pub name: String,
    /// Ordering hint among siblings.
    pub order_index: i32,
    /// When created.
    pub created_at: OffsetDateTime,
}

/// `package.kind` — the four ecosystems v1 detects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageKind {
    /// Cargo crate (`Cargo.toml [package]`).
    Rust,
    /// npm package (`package.json "name"`).
    Npm,
    /// Compact module (`module Foo {` declaration).
    Compact,
    /// Anything else, untagged.
    Other,
}

/// Detected package membership for a document/chunk (FR-049, FR-050).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// Database UUID.
    pub id: Uuid,
    /// Owning `source_version.id`.
    pub source_version_id: Uuid,
    /// Ecosystem kind.
    pub kind: PackageKind,
    /// Canonical package name.
    pub name: String,
    /// Optional version string from the manifest.
    pub version: Option<String>,
    /// Repo-relative path to the manifest, if any (Cargo.toml / package.json).
    pub manifest_path: Option<String>,
    /// Free-form metadata bag.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// `document.kind` — the document types v1 supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    /// Markdown / MDX page.
    Markdown,
    /// Source-code file.
    Code,
    /// Plaintext (e.g. README, .txt).
    Plaintext,
}

/// A single ingested document (a Markdown page or source-code file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// Database UUID.
    pub id: Uuid,
    /// Owning `source_version.id`.
    pub source_version_id: Uuid,
    /// Owning hierarchy node (kind=`document`).
    pub node_id: Uuid,
    /// Document kind discriminator.
    pub kind: DocumentKind,
    /// Public source URL (e.g. github.com/.../docs/foo.md), if known.
    pub source_url: Option<String>,
    /// Public published URL (e.g. docs.midnight.network/foo), if known.
    pub published_url: Option<String>,
    /// Repo-relative source path.
    pub source_path: String,
    /// ISO language tag or extension fallback.
    pub language: Option<String>,
    /// SHA-256 of normalized content; powers incremental re-ingest (FR-014).
    pub content_hash: String,
    /// Last-modified timestamp from the source, if known.
    pub source_modified_at: Option<OffsetDateTime>,
    /// Verbatim parsed frontmatter (Markdown YAML / code-file metadata).
    pub frontmatter: Option<serde_json::Value>,
    /// Materialized [`Provenance`] from the `document.provenance` JSONB column.
    #[serde(default)]
    pub provenance: Provenance,
    /// Package this document belongs to, if any.
    pub package_id: Option<Uuid>,
    /// Character count of the original content.
    pub char_count: i32,
    /// Token count (best-effort, by the embedding tokenizer).
    pub token_count: i32,
    /// When ingested.
    pub created_at: OffsetDateTime,
}

/// `chunk.status` — the lifecycle states of a chunk row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkStatus {
    /// Embedded and queryable.
    Ready,
    /// Embedding attempt failed (EC-03); excluded from read queries.
    EmbedFailed,
    /// Soft-deprecated by an explicit admin action.
    Deprecated,
}

/// One indexable chunk — the smallest searchable unit.
///
/// The DB-side `tsvector` and `embedding` columns are generated / written by
/// the ingest pipeline and not surfaced verbatim through the JSON API
/// (chunks return their `content` text and floats are too noisy for human
/// inspection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// Database UUID.
    pub id: Uuid,
    /// Owning `source_version.id`.
    pub source_version_id: Uuid,
    /// Owning `document.id`.
    pub document_id: Uuid,
    /// Owning hierarchy node (kind=`chunk`).
    pub node_id: Uuid,
    /// 0-indexed position within the document's chunks.
    pub chunk_index: i32,
    /// Total chunks in the parent document — enables prev/next navigation.
    pub total_chunks: i32,
    /// The chunk's content text (Markdown / code / plaintext).
    pub content: String,
    /// SHA-256 of `content`.
    pub content_hash: String,
    /// The embedding model used to produce this chunk's vector.
    pub embedding_model_id: Uuid,
    /// Markdown heading path leading to this chunk, e.g. `["Setup", "Install"]`.
    #[serde(default)]
    pub heading_path: Vec<String>,
    /// Code-symbol path leading to this chunk (mod/impl/fn for Rust, etc.).
    #[serde(default)]
    pub symbol_path: Vec<String>,
    /// Start byte offset in the source document.
    pub start_byte: i32,
    /// End byte offset in the source document.
    pub end_byte: i32,
    /// Best-effort token count by the embedding tokenizer.
    pub token_count: i32,
    /// Lifecycle state.
    pub status: ChunkStatus,
    /// When the chunk was first ingested.
    pub created_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_value(SourceKind::DocsSite).unwrap(),
            serde_json::Value::String("docs_site".into())
        );
        assert_eq!(
            serde_json::to_value(SourceVersionStatus::Building).unwrap(),
            serde_json::Value::String("building".into())
        );
        assert_eq!(
            serde_json::to_value(NodeKind::Group).unwrap(),
            serde_json::Value::String("group".into())
        );
        assert_eq!(
            serde_json::to_value(PackageKind::Rust).unwrap(),
            serde_json::Value::String("rust".into())
        );
        assert_eq!(
            serde_json::to_value(DocumentKind::Markdown).unwrap(),
            serde_json::Value::String("markdown".into())
        );
        assert_eq!(
            serde_json::to_value(ChunkStatus::EmbedFailed).unwrap(),
            serde_json::Value::String("embed_failed".into())
        );
    }

    #[test]
    fn embedding_model_to_wire_id() {
        let now = OffsetDateTime::now_utc();
        let m = EmbeddingModel {
            id: Uuid::new_v4(),
            name: "bge-base-en-v1.5".into(),
            revision: 1,
            dim: 768,
            provider: "baai".into(),
            created_at: now,
        };
        assert_eq!(m.wire_id().unwrap().to_string(), "bge-base-en-v1.5@1");
    }
}

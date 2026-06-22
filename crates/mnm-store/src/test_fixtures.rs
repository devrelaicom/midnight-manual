//! Shared seeding helpers for DB integration tests.
//!
//! These helpers drive the entity layer directly, independently of
//! `mnm-content`, so the store tests have no transitive dependency on the
//! chunker or embedder.
//!
//! Only compiled when `cfg(any(test, feature = "integration"))`.

#![allow(
    clippy::missing_panics_doc, // test helpers are allowed to panic on setup failure
    clippy::too_many_lines,     // fixture setup is verbose by design
    clippy::similar_names,      // doc_a_node / doc_b_node etc. are intentionally parallel names
)]

use mnm_core::provenance::Provenance;
use mnm_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use sqlx::PgPool;
use uuid::Uuid;

use crate::entities::{chunk, document, embedding_model, node, source, source_version};

/// IDs returned by [`seed_two_doc_version`].
pub struct TwoDocFixture {
    /// The source UUID.
    pub source_id: Uuid,
    /// The `source_version` UUID.
    pub source_version_id: Uuid,
    /// UUID of doc A (`a.md`) — all chunks `ready`.
    pub doc_a_id: Uuid,
    /// UUID of doc B (`b.md`) — has one `embed_failed` chunk.
    pub doc_b_id: Uuid,
}

/// Insert a fresh source with one `source_version` containing two documents:
///
/// * `a.md` — two chunks, both `ready` → `embed_complete = true`.
/// * `b.md` — two chunks, one `ready` and one `embed_failed` →
///   `embed_complete = false`.
///
/// Insert order: `source` → `source_version` → `node` → `document` → `chunk`.
pub async fn seed_two_doc_version(pool: &PgPool) -> TwoDocFixture {
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .expect("upsert embedding model");

    let slug = format!("inventory-test-{}", Uuid::new_v4());
    let source_id =
        source::insert(pool, &slug, &format!("{slug} (fixture)"), SourceKind::DocsSite, None, 5)
            .await
            .expect("insert source");

    let (sv_id, _) =
        source_version::create_building(pool, source_id, model_id, None, "0.1.0", "inv-hash")
            .await
            .expect("create source_version");

    // Root node
    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .expect("insert root node");

    // ── Doc A ──────────────────────────────────────────────────────────────
    let doc_a_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, "a.md", 0)
        .await
        .expect("insert doc A node");

    let provenance = Provenance::default();

    let doc_a_id = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_a_node,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: None,
            source_path: "a.md",
            language: Some("en"),
            content_hash: "hash-a",
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 40,
            token_count: 10,
        },
    )
    .await
    .expect("insert document A");

    // Doc A — chunk 0: ready
    let ca0_node = node::insert(pool, sv_id, Some(doc_a_node), NodeKind::Chunk, "ca0", 0)
        .await
        .expect("insert doc A chunk 0 node");

    chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_a_id,
            node_id: ca0_node,
            chunk_index: 0,
            total_chunks: 2,
            content: "Doc A chunk 0.",
            content_hash: "hash-a-c0",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 14,
            token_count: 4,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .expect("insert doc A chunk 0");

    // Doc A — chunk 1: ready
    let ca1_node = node::insert(pool, sv_id, Some(doc_a_node), NodeKind::Chunk, "ca1", 1)
        .await
        .expect("insert doc A chunk 1 node");

    chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_a_id,
            node_id: ca1_node,
            chunk_index: 1,
            total_chunks: 2,
            content: "Doc A chunk 1.",
            content_hash: "hash-a-c1",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 15,
            end_byte: 29,
            token_count: 4,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .expect("insert doc A chunk 1");

    // ── Doc B ──────────────────────────────────────────────────────────────
    let doc_b_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, "b.md", 1)
        .await
        .expect("insert doc B node");

    let doc_b_id = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_b_node,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: None,
            source_path: "b.md",
            language: Some("en"),
            content_hash: "hash-b",
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 40,
            token_count: 10,
        },
    )
    .await
    .expect("insert document B");

    // Doc B — chunk 0: ready
    let cb0_node = node::insert(pool, sv_id, Some(doc_b_node), NodeKind::Chunk, "cb0", 0)
        .await
        .expect("insert doc B chunk 0 node");

    chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_b_id,
            node_id: cb0_node,
            chunk_index: 0,
            total_chunks: 2,
            content: "Doc B chunk 0.",
            content_hash: "hash-b-c0",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 14,
            token_count: 4,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .expect("insert doc B chunk 0");

    // Doc B — chunk 1: embed_failed  ← this is what makes embed_complete false
    let cb1_node = node::insert(pool, sv_id, Some(doc_b_node), NodeKind::Chunk, "cb1", 1)
        .await
        .expect("insert doc B chunk 1 node");

    chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_b_id,
            node_id: cb1_node,
            chunk_index: 1,
            total_chunks: 2,
            content: "Doc B chunk 1.",
            content_hash: "hash-b-c1",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 15,
            end_byte: 29,
            token_count: 4,
            status: ChunkStatus::EmbedFailed,
        },
    )
    .await
    .expect("insert doc B chunk 1");

    TwoDocFixture {
        source_id,
        source_version_id: sv_id,
        doc_a_id,
        doc_b_id,
    }
}

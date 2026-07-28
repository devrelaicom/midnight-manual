//! Shared test fixtures for midnight-manual-server integration tests.
//!
//! These helpers mirror those in `crates/mnm-store/tests/fixtures.rs` so that
//! midnight-manual-server integration tests can set up a standard corpus without depending
//! on mnm-store's test infrastructure directly. Duplication is intentional —
//! test code isolation is more important than DRY here.

#![allow(dead_code, missing_docs, clippy::too_many_lines)]

use mnm_core::provenance::Provenance;
use mnm_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mnm_store::entities::{chunk, document, embedding_model, node, source, source_version};
use sqlx::PgPool;
use uuid::Uuid;

/// IDs returned by [`ingest_n_chunk_doc`].
pub struct MinimalDocFixture {
    /// The source UUID.
    pub source_id: Uuid,
    /// The `source_version` UUID.
    pub source_version_id: Uuid,
    /// The document UUID.
    pub document_id: Uuid,
    /// Chunk UUIDs in `chunk_index` order (0..n).
    pub chunk_ids: Vec<Uuid>,
}

/// Insert a fresh source with one `source_version` + one document + `n` chunks
/// (indices `0..n`).
///
/// The document's `published_url` is `https://example.com/<slug>/first/`.
/// All chunks have `status = 'ready'`.
pub async fn ingest_n_chunk_doc(pool: &PgPool, slug: &str, n: usize) -> MinimalDocFixture {
    assert!(n >= 1, "ingest_n_chunk_doc requires at least 1 chunk");
    let n = i32::try_from(n).expect("chunk count fits in i32");

    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .expect("upsert embedding model");

    let source_id =
        source::insert(pool, slug, &format!("{slug} (fixture)"), SourceKind::DocsSite, None, 5)
            .await
            .expect("insert source");

    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, None, "0.1.0", "h")
        .await
        .expect("create source_version");

    let root_node = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .expect("insert root node");

    let doc_node = node::insert(pool, sv_id, Some(root_node), NodeKind::Document, "first.md", 0)
        .await
        .expect("insert document node");

    let provenance = Provenance::default();
    let published_url = format!("https://example.com/{slug}/first/");

    let document_id = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: Some(&published_url),
            source_path: "first.md",
            language: Some("en"),
            content_hash: "fixture-hash-first",
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 40,
            token_count: 10,
            license: None,
        },
    )
    .await
    .expect("insert document");

    let mut chunk_ids = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
    for i in 0..n {
        let chunk_node =
            node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, &format!("c{i}"), i)
                .await
                .expect("insert chunk node");

        let chunk_id = chunk::insert(
            pool,
            chunk::NewChunk {
                source_version_id: sv_id,
                document_id,
                node_id: chunk_node,
                chunk_index: i,
                total_chunks: n,
                content: &format!("Chunk {i} of the fixture document."),
                content_hash: &format!("fixture-chunk-hash-{i}"),
                embedding: None,
                embedding_model_id: model_id,
                code_embedding: None,
                heading_path: &[],
                symbol_path: &[],
                start_byte: i * 40,
                end_byte: (i + 1) * 40,
                token_count: 7,
                status: ChunkStatus::Ready,
            },
        )
        .await
        .expect("insert chunk");

        chunk_ids.push(chunk_id);
    }

    MinimalDocFixture {
        source_id,
        source_version_id: sv_id,
        document_id,
        chunk_ids,
    }
}

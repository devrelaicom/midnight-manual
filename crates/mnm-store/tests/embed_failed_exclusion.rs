//! US1 acceptance #9 / EC-03: chunks with `status = 'embed_failed'` are excluded
//! from the read API (`get_by_id_ready`) but remain listable via the
//! admin-facing `get_by_id_admin`.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use mnm_core::provenance::Provenance;
use mnm_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mnm_store::entities::{chunk, document, embedding_model, node, source, source_version};
use uuid::Uuid;

#[tokio::test]
async fn embed_failed_excluded_from_read_path_but_admin_visible() {
    let h = common::boot().await;

    let model_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("embed-failed-test-{}", Uuid::new_v4());
    let source_id =
        source::insert(&h.pool, &slug, "Embed Failed Test", SourceKind::DocsSite, None, 5)
            .await
            .unwrap();
    let (sv_id, _) =
        source_version::create_building(&h.pool, source_id, model_id, None, "0.1.0", "h")
            .await
            .unwrap();
    let root = node::insert(&h.pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let doc_node = node::insert(&h.pool, sv_id, Some(root), NodeKind::Document, "doc.md", 0)
        .await
        .unwrap();
    let provenance = Provenance::default();
    let doc_id = document::insert(
        &h.pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: None,
            source_path: "doc.md",
            language: Some("en"),
            content_hash: "h",
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 0,
            token_count: 0,
        },
    )
    .await
    .unwrap();

    let chunk_node_1 = node::insert(&h.pool, sv_id, Some(doc_node), NodeKind::Chunk, "c1", 0)
        .await
        .unwrap();
    let chunk_node_2 = node::insert(&h.pool, sv_id, Some(doc_node), NodeKind::Chunk, "c2", 1)
        .await
        .unwrap();

    // Two chunks: c1 ready, c2 embed_failed.
    let ready_id = chunk::insert(
        &h.pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node_1,
            chunk_index: 0,
            total_chunks: 2,
            content: "ready chunk",
            content_hash: "ch1",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 11,
            token_count: 2,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .unwrap();

    let failed_id = chunk::insert(
        &h.pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node_2,
            chunk_index: 1,
            total_chunks: 2,
            content: "failed chunk",
            content_hash: "ch2",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 12,
            end_byte: 24,
            token_count: 2,
            status: ChunkStatus::EmbedFailed,
        },
    )
    .await
    .unwrap();

    // Read path: ready chunk visible.
    chunk::get_by_id_ready(&h.pool, ready_id)
        .await
        .expect("ready visible");

    // Read path: failed chunk hidden.
    let err = chunk::get_by_id_ready(&h.pool, failed_id)
        .await
        .unwrap_err();
    assert!(matches!(err, mnm_store::StoreError::NotFound));

    // Admin path: both visible.
    chunk::get_by_id_admin(&h.pool, ready_id)
        .await
        .expect("admin sees ready");
    chunk::get_by_id_admin(&h.pool, failed_id)
        .await
        .expect("admin sees failed");
}

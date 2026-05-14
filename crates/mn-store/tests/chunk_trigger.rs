//! EC-10 / FR-002 cross-table check: `chunk.embedding_model_id` MUST match
//! its owning `source_version.embedding_model_id`. Enforced by the
//! `trg_chunk_embedding_model_match` trigger.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use mn_core::provenance::Provenance;
use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use uuid::Uuid;

#[tokio::test]
async fn chunk_with_mismatched_model_id_is_rejected() {
    let h = common::boot().await;

    let model_a = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let model_b = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 2, 768, "baai")
        .await
        .unwrap();
    assert_ne!(model_a, model_b);

    let slug = format!("trigger-test-{}", Uuid::new_v4());
    let source_id = source::insert(&h.pool, &slug, "Trigger Test", SourceKind::Standalone, None, 5)
        .await
        .unwrap();

    // source_version uses model_a
    let (sv_id, _) = source_version::create_building(&h.pool, source_id, model_a, "0.1.0", "h")
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

    let chunk_node = node::insert(&h.pool, sv_id, Some(doc_node), NodeKind::Chunk, "c1", 0)
        .await
        .unwrap();

    // Inserting a chunk whose embedding_model_id is model_b (NOT model_a) must
    // be rejected by the BEFORE INSERT trigger.
    let err = chunk::insert(
        &h.pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node,
            chunk_index: 0,
            total_chunks: 1,
            content: "hello",
            content_hash: "ch",
            embedding: None,
            embedding_model_id: model_b, // wrong!
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 5,
            token_count: 1,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .unwrap_err();

    match err {
        mn_store::StoreError::CheckViolation(msg) => {
            assert!(msg.contains("does not match"), "expected trigger message, got: {msg}");
        }
        other => panic!("expected CheckViolation, got {other:?}"),
    }

    // Matching the source_version's model_a succeeds.
    chunk::insert(
        &h.pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node,
            chunk_index: 0,
            total_chunks: 1,
            content: "hello",
            content_hash: "ch",
            embedding: None,
            embedding_model_id: model_a, // correct
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 5,
            token_count: 1,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .expect("chunk with matching model_id inserts");
}

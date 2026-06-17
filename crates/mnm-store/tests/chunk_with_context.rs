//! Task 1 integration test: `chunk::get_with_context` returns a chunk bundled
//! with its parent document and source metadata.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;
mod fixtures;

#[tokio::test]
async fn get_with_context_returns_chunk_plus_document_and_source() {
    let h = common::boot().await;
    let fx = fixtures::ingest_minimal_two_chunk_doc(&h.pool, "with-context").await;

    let row = mnm_store::entities::chunk::get_with_context(&h.pool, fx.chunk_ids[0])
        .await
        .unwrap();

    assert_eq!(row.chunk.id, fx.chunk_ids[0]);
    assert_eq!(row.document.id, fx.document_id);
    assert_eq!(row.document.source_path, "first.md");
    assert_eq!(
        row.document.published_url.as_deref(),
        Some("https://example.com/with-context/first/")
    );
    assert_eq!(row.source.slug, "with-context");
}

#[tokio::test]
async fn get_with_context_embed_failed_returns_not_found() {
    use mnm_core::provenance::Provenance;
    use mnm_core::types::ChunkStatus;
    use mnm_core::types::{DocumentKind, NodeKind, SourceKind};
    use mnm_store::entities::{chunk, embedding_model, node, source, source_version};
    use uuid::Uuid;

    let h = common::boot().await;

    let model_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("ctx-failed-{}", Uuid::new_v4());
    let source_id =
        source::insert(&h.pool, &slug, "Ctx Failed Test", SourceKind::DocsSite, None, 5)
            .await
            .unwrap();
    let (sv_id, _) =
        source_version::create_building(&h.pool, source_id, model_id, None, "0.1.0", "h")
            .await
            .unwrap();
    let root = node::insert(&h.pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let doc_node = node::insert(&h.pool, sv_id, Some(root), NodeKind::Document, "fail.md", 0)
        .await
        .unwrap();
    let provenance = Provenance::default();
    let doc_id = mnm_store::entities::document::insert(
        &h.pool,
        mnm_store::entities::document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: None,
            source_path: "fail.md",
            language: None,
            content_hash: "hfail",
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
    let failed_id = chunk::insert(
        &h.pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node,
            chunk_index: 0,
            total_chunks: 1,
            content: "failed chunk",
            content_hash: "chfail",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 12,
            token_count: 2,
            status: ChunkStatus::EmbedFailed,
        },
    )
    .await
    .unwrap();

    let err = chunk::get_with_context(&h.pool, failed_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, mnm_store::StoreError::NotFound),
        "embed_failed chunk must be invisible to get_with_context"
    );
}

//! Store-level navigation tests for document::* helpers.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;
mod fixtures;

use mn_store::entities::document;

#[tokio::test]
async fn get_overview_returns_doc_plus_ordered_chunk_ids() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "overview", 4).await;

    let ov = document::get_overview(&h.pool, fx.document_id).await.unwrap();

    assert_eq!(ov.document.id, fx.document_id);
    assert_eq!(ov.source.slug, "overview");
    assert_eq!(ov.chunk_ids, fx.chunk_ids); // already in chunk_index order from the fixture
}

#[tokio::test]
async fn get_overview_omits_embed_failed_chunks() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "overview-skip", 4).await;
    fixtures::mark_chunk_failed(&h.pool, fx.chunk_ids[2]).await;

    let ov = document::get_overview(&h.pool, fx.document_id).await.unwrap();
    let expected: Vec<_> = vec![fx.chunk_ids[0], fx.chunk_ids[1], fx.chunk_ids[3]];
    assert_eq!(ov.chunk_ids, expected);
}

#[tokio::test]
async fn get_full_returns_document_with_all_chunks_in_order() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "full", 4).await;

    let res = document::get_full(&h.pool, fx.document_id, 500).await.unwrap();
    let full = match res {
        document::FullResult::Document(f) => f,
        document::FullResult::TooManyChunks { .. } => panic!("unexpected cap result"),
    };
    let idxs: Vec<i32> = full.chunks.iter().map(|c| c.chunk_index).collect();
    assert_eq!(idxs, vec![0, 1, 2, 3]);
    assert_eq!(full.source.slug, "full");
}

#[tokio::test]
async fn get_full_signals_too_many_chunks_above_cap() {
    let h = common::boot().await;
    // 6-chunk doc; cap at 5 to trigger the overflow without inserting hundreds.
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "full-cap", 6).await;

    let res = document::get_full(&h.pool, fx.document_id, 5).await.unwrap();
    match res {
        document::FullResult::TooManyChunks { count, cap } => {
            assert_eq!(count, 6);
            assert_eq!(cap, 5);
        }
        document::FullResult::Document(_) => panic!("expected TooManyChunks"),
    }
}

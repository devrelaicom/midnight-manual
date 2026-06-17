//! Store-level navigation tests for `document::*` helpers.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;
mod fixtures;

use mnm_store::entities::document;

#[tokio::test]
async fn get_overview_returns_doc_plus_ordered_chunk_skeletons() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "overview", 4).await;

    let ov = document::get_overview(&h.pool, fx.document_id)
        .await
        .unwrap();

    assert_eq!(ov.document.id, fx.document_id);
    assert_eq!(ov.source.slug, "overview");
    // Already in chunk_index order from the fixture.
    let skeleton_ids: Vec<_> = ov.chunks.iter().map(|c| c.id).collect();
    assert_eq!(skeleton_ids, fx.chunk_ids);
    let indexes: Vec<i32> = ov.chunks.iter().map(|c| c.chunk_index).collect();
    assert_eq!(indexes, vec![0, 1, 2, 3]);
    assert!(
        ov.chunks.iter().all(|c| c.token_count > 0),
        "every skeleton entry should carry a positive token_count"
    );
}

#[tokio::test]
async fn get_overview_omits_embed_failed_chunks() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "overview-skip", 4).await;
    fixtures::mark_chunk_failed(&h.pool, fx.chunk_ids[2]).await;

    let ov = document::get_overview(&h.pool, fx.document_id)
        .await
        .unwrap();
    let expected: Vec<_> = vec![fx.chunk_ids[0], fx.chunk_ids[1], fx.chunk_ids[3]];
    let skeleton_ids: Vec<_> = ov.chunks.iter().map(|c| c.id).collect();
    assert_eq!(skeleton_ids, expected);
}

#[tokio::test]
async fn list_chunks_window_returns_requested_range() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "window", 10).await;

    let w = document::list_chunks_window(&h.pool, fx.document_id, 3, 4)
        .await
        .unwrap();
    let idxs: Vec<i32> = w.chunks.iter().map(|c| c.chunk_index).collect();
    assert_eq!(idxs, vec![3, 4, 5, 6]);
    assert_eq!(w.from, 3);
    assert_eq!(w.limit, 4);
    assert_eq!(w.total_chunks, 10);
}

#[tokio::test]
async fn list_chunks_window_past_end_returns_empty_with_total() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "window-end", 5).await;

    let w = document::list_chunks_window(&h.pool, fx.document_id, 10, 5)
        .await
        .unwrap();
    assert!(w.chunks.is_empty());
    assert_eq!(w.from, 10);
    assert_eq!(w.total_chunks, 5);
}

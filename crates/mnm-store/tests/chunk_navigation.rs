//! Store-level navigation tests for `chunk::list_next` / `list_prev`.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;
mod fixtures;

use mnm_store::entities::chunk;

#[tokio::test]
async fn list_next_returns_chunks_after_anchor_in_order() {
    let h = common::boot().await;
    // 5-chunk doc, indices 0..=4
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "next-test", 5).await;

    let rows = chunk::list_next(&h.pool, fx.chunk_ids[1], 2).await.unwrap();
    let idxs: Vec<i32> = rows.iter().map(|r| r.chunk.chunk_index).collect();
    assert_eq!(idxs, vec![2, 3]);
}

#[tokio::test]
async fn list_next_returns_empty_on_last_chunk() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "next-end", 3).await;

    let rows = chunk::list_next(&h.pool, fx.chunk_ids[2], 5).await.unwrap();
    assert!(rows.is_empty());
}

#[tokio::test]
async fn list_prev_returns_chunks_before_anchor_in_ascending_order() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "prev-test", 5).await;

    // Anchor at index 4; ask for 2 prev → expect indices [2, 3].
    let rows = chunk::list_prev(&h.pool, fx.chunk_ids[4], 2).await.unwrap();
    let idxs: Vec<i32> = rows.iter().map(|r| r.chunk.chunk_index).collect();
    assert_eq!(idxs, vec![2, 3]);
}

#[tokio::test]
async fn list_prev_returns_empty_on_first_chunk() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "prev-start", 3).await;

    let rows = chunk::list_prev(&h.pool, fx.chunk_ids[0], 5).await.unwrap();
    assert!(rows.is_empty());
}

#[tokio::test]
async fn list_next_skips_embed_failed_chunks() {
    let h = common::boot().await;
    // 5-chunk doc; mark chunk at index 2 as embed_failed.
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, "next-skip", 5).await;
    fixtures::mark_chunk_failed(&h.pool, fx.chunk_ids[2]).await;

    // Anchor at index 1; ask for 3 next → expect indices [3, 4] (2 is skipped).
    let rows = chunk::list_next(&h.pool, fx.chunk_ids[1], 3).await.unwrap();
    let idxs: Vec<i32> = rows.iter().map(|r| r.chunk.chunk_index).collect();
    assert_eq!(idxs, vec![3, 4]);
}

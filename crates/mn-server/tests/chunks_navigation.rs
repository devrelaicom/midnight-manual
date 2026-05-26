//! Integration tests for /v1/chunks/:id, /next, /prev.

#![cfg(feature = "integration")]
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::missing_const_for_fn
)]

mod common;
mod fixtures;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use mn_server::{app, config::ServerConfig};
use tower::ServiceExt;
use uuid::Uuid;

fn cfg() -> ServerConfig {
    ServerConfig {
        corpus_model: None,
        ..Default::default()
    }
}

#[tokio::test]
async fn get_chunk_returns_document_and_source_context() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, &format!("ctx-{}", Uuid::new_v4().simple()), 3).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/chunks/{}", fx.chunk_ids[1]))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["id"].as_str().unwrap(), fx.chunk_ids[1].to_string());
    // document context is bundled
    assert!(v["document"]["published_url"].is_string(), "document.published_url should be present");
    assert!(v["source"]["slug"].is_string(), "source.slug should be present");
}

#[tokio::test]
async fn get_next_returns_chunks_after_anchor() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, &format!("next-{}", Uuid::new_v4().simple()), 5).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/chunks/{}/next?count=2", fx.chunk_ids[1]))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let chunks = v["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0]["chunk_index"], 2);
    assert_eq!(chunks[1]["chunk_index"], 3);
}

#[tokio::test]
async fn get_prev_returns_chunks_before_anchor() {
    let h = common::boot().await;
    let fx = fixtures::ingest_n_chunk_doc(&h.pool, &format!("prev-{}", Uuid::new_v4().simple()), 5).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/chunks/{}/prev?count=2", fx.chunk_ids[4]))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let chunks = v["chunks"].as_array().unwrap();
    let idxs: Vec<i64> = chunks.iter().map(|c| c["chunk_index"].as_i64().unwrap()).collect();
    assert_eq!(idxs, vec![2, 3]);
}

#[tokio::test]
async fn get_chunk_404_for_missing_id() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/chunks/{}", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

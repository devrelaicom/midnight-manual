//! Integration tests for /v1/documents/:id and /chunks.

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
use midnight_manual_server::{app, config::ServerConfig};
use tower::ServiceExt;
use uuid::Uuid;

fn cfg() -> ServerConfig {
    ServerConfig::default()
}

#[tokio::test]
async fn get_document_returns_overview_with_chunk_skeletons() {
    let h = common::boot().await;
    let fx =
        fixtures::ingest_n_chunk_doc(&h.pool, &format!("doc-ov-{}", Uuid::new_v4().simple()), 3)
            .await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/documents/{}", fx.document_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        v["id"].as_str().unwrap(),
        fx.document_id.to_string(),
        "response id should match document_id"
    );
    assert!(v["source"]["slug"].is_string(), "source.slug should be present");
    let chunks = v["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 3, "should return 3 chunk skeletons");
    // Skeleton entries carry {id, chunk_index, token_count} — no bodies.
    assert!(chunks[0]["id"].is_string(), "chunks[0].id should be a UUID string");
    assert_eq!(chunks[0]["chunk_index"], 0, "chunks[0] should be the first chunk");
    assert!(
        chunks[0]["token_count"].as_i64().unwrap_or(0) > 0,
        "chunks[0].token_count should be positive"
    );
    assert!(chunks[0].get("content").is_none(), "skeleton must not carry chunk bodies");
}

#[tokio::test]
async fn get_document_chunks_returns_windowed_slice() {
    let h = common::boot().await;
    let fx =
        fixtures::ingest_n_chunk_doc(&h.pool, &format!("doc-win-{}", Uuid::new_v4().simple()), 10)
            .await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/documents/{}/chunks?from=3&limit=4", fx.document_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let chunks = v["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 4, "should return 4 chunks");
    assert_eq!(v["from"], 3, "from should echo back 3");
    assert_eq!(v["limit"], 4, "limit should echo back 4");
    assert_eq!(v["total_chunks"], 10, "total_chunks should be 10");
}

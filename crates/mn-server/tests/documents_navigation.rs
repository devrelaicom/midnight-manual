//! Integration tests for /v1/documents/:id, /full, and /chunks.

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
    ServerConfig::default()
}

#[tokio::test]
async fn get_document_returns_overview_with_chunk_ids() {
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
    let ids = v["chunk_ids"].as_array().unwrap();
    assert_eq!(ids.len(), 3, "should return 3 chunk_ids");
}

#[tokio::test]
async fn get_document_full_returns_chunks_inline() {
    let h = common::boot().await;
    let fx =
        fixtures::ingest_n_chunk_doc(&h.pool, &format!("doc-full-{}", Uuid::new_v4().simple()), 2)
            .await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/documents/{}/full", fx.document_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let chunks = v["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 2, "should return 2 chunks inline");
    assert!(chunks[0]["content"].is_string(), "chunk[0].content should be a string");
}

#[tokio::test]
async fn get_document_full_returns_412_above_cap() {
    // SAFETY: env vars are process-global, so this test may race if run in
    // parallel with other tests that also set this variable. `serial_test` is
    // not in dev-dependencies for this crate; callers who need strict isolation
    // should run this test file with `-- --test-threads=1`. In practice the
    // unique slug + separate DB schemas prevent data-level interference; only
    // the cap env var is shared. The remove_var at the end restores the env.
    //
    // The cap is set to 5 so only 6 inserted chunks are needed to trigger
    // TooManyChunks — vastly cheaper than inserting 501 chunks.
    std::env::set_var("MIDNIGHT_MANUAL_DOCUMENT_FULL_CHUNK_CAP", "5");

    let h = common::boot().await;
    let fx =
        fixtures::ingest_n_chunk_doc(&h.pool, &format!("doc-cap-{}", Uuid::new_v4().simple()), 6)
            .await;
    // Build app AFTER setting the env var so effective_cap() picks it up.
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/documents/{}/full", fx.document_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Restore the env var before any assertions (so a panic doesn't leave it set).
    std::env::remove_var("MIDNIGHT_MANUAL_DOCUMENT_FULL_CHUNK_CAP");

    assert_eq!(
        resp.status(),
        StatusCode::PRECONDITION_FAILED,
        "should return 412 when chunk count exceeds cap"
    );
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "too_many_chunks");
    assert_eq!(v["chunk_count"], 6);
    assert_eq!(v["cap"], 5);
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

//! Integration tests for `GET /v1/facets`.
#![cfg(feature = "integration")]
#![allow(clippy::doc_markdown)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use mn_server::{app, config::ServerConfig};
use tower::ServiceExt as _;

fn cfg() -> ServerConfig {
    ServerConfig::default()
}

#[tokio::test]
async fn facets_lists_modes_and_closed_enums() {
    let h = common::boot().await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/facets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["modes"], serde_json::json!(["hybrid", "vector", "fts"]));
    let kind = body["filters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "kind")
        .expect("kind facet");
    assert_eq!(kind["type"], "enum");
    assert_eq!(kind["values"], serde_json::json!(["markdown", "code", "plaintext"]));
}

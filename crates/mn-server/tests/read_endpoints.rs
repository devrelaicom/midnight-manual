//! End-to-end smoke test for the Phase 4b read endpoints.
//!
//! Boots the axum app in-process against a real Postgres (testcontainers or
//! CI's `DATABASE_URL`) and exercises `/healthz`, `/readyz`, `/v1/models/active`,
//! and `/v1/sources` list+show.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use mn_core::types::SourceKind;
use mn_server::{app, config::ServerConfig};
use mn_store::entities::{embedding_model, source};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn healthz_returns_200() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn readyz_returns_200_when_pgvector_present() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn active_model_returns_seeded_row() {
    let h = common::boot().await;
    // The active model after migrations is voyage-code-3@1: migration 0008
    // registers it most-recently, so `get_active` returns it over the older
    // bge-base-en-v1.5@1 (migration 0006). Upsert is idempotent.
    embedding_model::upsert(&h.pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/models/active")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["name"], "voyage-code-3");
    assert_eq!(v["revision"], 1);
    assert_eq!(v["dim"], 1024);
}

#[tokio::test]
async fn sources_list_includes_inserted_row_and_show_round_trips() {
    let h = common::boot().await;
    let slug = format!("phase4b-test-{}", Uuid::new_v4());
    source::insert(
        &h.pool,
        &slug,
        "Phase 4b Test",
        SourceKind::DocsSite,
        Some("https://example.com"),
        5,
    )
    .await
    .unwrap();

    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    // List
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/sources")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 16 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let sources = v["sources"]
        .as_array()
        .expect("list response must be `{sources, total, next_cursor}`");
    let has_slug = sources
        .iter()
        .filter_map(|row| row["slug"].as_str())
        .any(|s| s == slug);
    assert!(has_slug, "list response must include the inserted slug");
    assert!(v["total"].as_i64().unwrap() >= 1, "total must count the inserted row: {v}");
    assert!(v["next_cursor"].is_null(), "single page must have null next_cursor: {v}");

    // Show
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/sources/{slug}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["slug"], slug);
    assert_eq!(v["display_name"], "Phase 4b Test");
    assert_eq!(v["kind"], "docs_site");
}

#[tokio::test]
async fn unknown_source_returns_404() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/sources/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "not_found");
    assert!(v["error"]["remediation"].is_string());
}

#[tokio::test]
async fn x_request_id_is_echoed() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("x-request-id", "test-id-abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.headers()
            .get("x-request-id")
            .map(|v| v.to_str().unwrap()),
        Some("test-id-abc")
    );

    // No incoming header: server mints one.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let id = resp
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(!id.is_empty(), "server-minted request id must be non-empty");
    assert!(uuid::Uuid::parse_str(id).is_ok(), "minted id must be a valid UUID");
}

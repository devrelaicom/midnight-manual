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

#[tokio::test]
async fn healthz_returns_200() {
    let h = common::boot().await;
    let app = app::build(
        h.pool.clone(),
        ServerConfig {
            database_url: String::new(),
            port: 0,
            auto_migrate: false,
            corpus_model: None,
        },
    );

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
    let app = app::build(
        h.pool.clone(),
        ServerConfig {
            database_url: String::new(),
            port: 0,
            auto_migrate: false,
            corpus_model: None,
        },
    );

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
    // Make sure the seed row exists (migration 0006 already does it).
    embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let app = app::build(
        h.pool.clone(),
        ServerConfig {
            database_url: String::new(),
            port: 0,
            auto_migrate: false,
            corpus_model: None,
        },
    );

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
    assert_eq!(v["name"], "bge-base-en-v1.5");
    assert_eq!(v["revision"], 1);
    assert_eq!(v["dim"], 768);
}

#[tokio::test]
async fn sources_list_includes_inserted_row_and_show_round_trips() {
    let h = common::boot().await;
    source::insert(
        &h.pool,
        "phase4b-test",
        "Phase 4b Test",
        SourceKind::DocsSite,
        Some("https://example.com"),
        5,
    )
    .await
    .unwrap();

    let app = app::build(
        h.pool.clone(),
        ServerConfig {
            database_url: String::new(),
            port: 0,
            auto_migrate: false,
            corpus_model: None,
        },
    );

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
    assert!(v.is_array(), "list response must be a JSON array");
    let has_slug = v
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["slug"].as_str())
        .any(|s| s == "phase4b-test");
    assert!(has_slug, "list response must include the inserted slug");

    // Show
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/sources/phase4b-test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["slug"], "phase4b-test");
    assert_eq!(v["display_name"], "Phase 4b Test");
    assert_eq!(v["kind"], "docs_site");
}

#[tokio::test]
async fn unknown_source_returns_404() {
    let h = common::boot().await;
    let app = app::build(
        h.pool.clone(),
        ServerConfig {
            database_url: String::new(),
            port: 0,
            auto_migrate: false,
            corpus_model: None,
        },
    );

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
    let app = app::build(
        h.pool.clone(),
        ServerConfig {
            database_url: String::new(),
            port: 0,
            auto_migrate: false,
            corpus_model: None,
        },
    );

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

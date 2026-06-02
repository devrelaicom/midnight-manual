//! Integration test for `POST /v1/embeddings` (Task 4.6).
//!
//! Voyage is mocked with wiremock so the test exercises the full handler path
//! (token resolve → check → call → charge → snapshot) without a network egress.
//! `AppState` needs a real `PgPool`, so this is gated behind the `integration`
//! feature and boots Postgres via the shared `common` harness.

#![cfg(feature = "integration")]
#![allow(missing_docs, clippy::too_many_lines)]

mod common;

use std::sync::{Arc, RwLock};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use mn_embedding::voyage::VoyageEmbedder;
use mn_server::app;
use mn_server::config::ServerConfig;
use mn_server::corpus_model::CorpusModel;
use mn_server::tokenlimit::{Limits, TokenUsageLimiter};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn embeds_via_voyage_and_charges_tokens() {
    let h = common::boot().await;

    // Mock Voyage: one 1024-dim vector, 5 tokens consumed.
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "embedding": vec![0.0_f32; 1024], "index": 0 }],
            "model": "voyage-code-3",
            "usage": { "total_tokens": 5 }
        })))
        .mount(&mock)
        .await;

    // Pin the corpus model to voyage-code-3@1 / 1024 dims so the response echoes
    // it and any `model` assertion would match.
    let corpus_model = Arc::new(RwLock::new(Some(CorpusModel {
        wire: "voyage-code-3@1".to_owned(),
        id: Uuid::new_v4(),
        dim: 1024,
    })));

    // Generous limits so the request is allowed; assert the post-charge balance.
    let token_limiter = Arc::new(TokenUsageLimiter::new(
        Limits { hourly: 2000, daily: 20000 },
        Limits { hourly: 4000, daily: 40000 },
        Limits {
            hourly: 500_000,
            daily: 100_000_000,
        },
    ));

    let voyage = Some(Arc::new(
        VoyageEmbedder::new("k", "voyage-code-3", 1024, "float").with_base_url(&mock.uri()),
    ));

    let cfg = ServerConfig::default();
    // Rate limiting is irrelevant to this test; token accounting is what matters.
    let limiter = None;
    let app =
        app::build_with_limiter(h.pool.clone(), cfg, limiter, corpus_model, token_limiter, voyage)
            .expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "input": ["hi"], "input_type": "query" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["usage"]["total_tokens"], 5, "{body}");
    assert_eq!(body["model"], "voyage-code-3@1", "{body}");
    assert_eq!(
        body["rate"]["hour"]["remaining"],
        2000 - 5,
        "anon hourly remaining after charging 5 tokens: {body}"
    );
    assert_eq!(
        body["embeddings"].as_array().map(Vec::len),
        Some(1),
        "one vector returned: {body}"
    );
}

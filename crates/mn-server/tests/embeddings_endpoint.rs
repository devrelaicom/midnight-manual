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
use mn_server::tokenlimit::{Limits, TokenSubject, TokenUsageLimiter};
use serde_json::{json, Value};
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A `VoyageEmbedder` pointed at an unreachable URL. The error-path tests below
/// reject (429/413) or 503 *before* Voyage is ever called, so this is never hit.
fn unreachable_voyage() -> Arc<VoyageEmbedder> {
    Arc::new(
        VoyageEmbedder::new("k", "voyage-code-3", 1024, "float")
            .with_base_url("http://127.0.0.1:1"),
    )
}

fn pinned_corpus_model() -> Arc<RwLock<Option<CorpusModel>>> {
    Arc::new(RwLock::new(Some(CorpusModel {
        wire: "voyage-code-3@1".to_owned(),
        id: Uuid::new_v4(),
        dim: 1024,
    })))
}

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
    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        limiter,
        corpus_model,
        token_limiter,
        voyage,
        None,
        Arc::new(RwLock::new(None)),
    )
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

#[tokio::test]
async fn over_hourly_cap_returns_429_with_retry_after() {
    let h = common::boot().await;

    // Tiny anon hourly ceiling; pre-charge the caller's IP subject over it so the
    // pre-check rejects before Voyage is ever called.
    let token_limiter = Arc::new(TokenUsageLimiter::new(
        Limits { hourly: 100, daily: 1000 },
        Limits { hourly: 4000, daily: 40000 },
        Limits {
            hourly: 500_000,
            daily: 100_000_000,
        },
    ));
    let now = OffsetDateTime::now_utc().unix_timestamp();
    token_limiter.charge(&TokenSubject::Ip("203.0.113.7".to_owned()), 200, now);

    let app = app::build_with_limiter(
        h.pool.clone(),
        ServerConfig::default(),
        None,
        pinned_corpus_model(),
        token_limiter,
        Some(unreachable_voyage()),
        None,
        Arc::new(RwLock::new(None)),
    )
    .expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                // Default client-ip header is `fly-client-ip`; resolve subject to
                // the pre-charged, over-budget IP.
                .header("fly-client-ip", "203.0.113.7")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "input": ["PRIVACY_SENTINEL_4_7_INPUT"] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        resp.headers().contains_key("retry-after"),
        "429 must carry a Retry-After header"
    );
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["context"]["error"], "token_limit_exceeded", "{body}");
    assert_eq!(body["error"]["context"]["window"], "hour", "{body}");
    // Privacy: the rejection body must not echo the submitted input text.
    assert!(
        !body.to_string().contains("PRIVACY_SENTINEL_4_7_INPUT"),
        "429 body must not contain the input text"
    );
}

#[tokio::test]
async fn missing_voyage_key_returns_503() {
    let h = common::boot().await;
    let token_limiter = Arc::new(TokenUsageLimiter::new(
        Limits { hourly: 2000, daily: 20000 },
        Limits { hourly: 4000, daily: 40000 },
        Limits {
            hourly: 500_000,
            daily: 100_000_000,
        },
    ));
    // voyage = None -> server embedding not configured.
    let app = app::build_with_limiter(
        h.pool.clone(),
        ServerConfig::default(),
        None,
        pinned_corpus_model(),
        token_limiter,
        None,
        None,
        Arc::new(RwLock::new(None)),
    )
    .expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "input": ["hi"] }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn over_1000_inputs_returns_413() {
    let h = common::boot().await;
    let token_limiter = Arc::new(TokenUsageLimiter::new(
        Limits { hourly: 2000, daily: 20000 },
        Limits { hourly: 4000, daily: 40000 },
        Limits {
            hourly: 500_000,
            daily: 100_000_000,
        },
    ));
    let app = app::build_with_limiter(
        h.pool.clone(),
        ServerConfig::default(),
        None,
        pinned_corpus_model(),
        token_limiter,
        Some(unreachable_voyage()),
        None,
        Arc::new(RwLock::new(None)),
    )
    .expect("build app");

    let oversized: Vec<String> = (0..1001).map(|i| format!("t{i}")).collect();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "input": oversized }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

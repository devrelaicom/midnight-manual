//! Integration test for `POST /v1/embeddings` (Task 4.6; dual-type routing
//! per the 2026-06-10 contextualized-dual-embeddings design §9).
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
use mn_embedding::contextualized::ContextualizedVoyageEmbedder;
use mn_embedding::voyage::VoyageEmbedder;
use mn_server::app;
use mn_server::code_model::CodeModel;
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
/// reject (429/413/400/409) or 503 *before* Voyage is ever called, so this is
/// never hit.
fn unreachable_voyage() -> Arc<VoyageEmbedder> {
    Arc::new(
        VoyageEmbedder::new("k", "voyage-code-3", 1024, "float")
            .with_base_url("http://127.0.0.1:1"),
    )
}

fn pinned_corpus_model() -> Arc<RwLock<Option<CorpusModel>>> {
    Arc::new(RwLock::new(Some(CorpusModel {
        wire: "voyage-context-3@1".to_owned(),
        id: Uuid::new_v4(),
        dim: 1024,
    })))
}

fn pinned_code_model() -> Arc<RwLock<Option<CodeModel>>> {
    Arc::new(RwLock::new(Some(CodeModel {
        wire: "voyage-code-3@1".to_owned(),
        id: Uuid::new_v4(),
        dim: 1024,
    })))
}

/// Generous tiered limits so requests pass the token gate; tests that need
/// throttling build their own limiter.
fn generous_limiter() -> Arc<TokenUsageLimiter> {
    Arc::new(TokenUsageLimiter::new(
        Limits { hourly: 2000, daily: 20000 },
        Limits { hourly: 4000, daily: 40000 },
        Limits {
            hourly: 500_000,
            daily: 100_000_000,
        },
    ))
}

/// Mount a dynamic `POST /v1/contextualizedembeddings` mock that mirrors the
/// request's group shape: one 1024-dim zero vector per chunk, one token per
/// chunk.
async fn mount_contextualized_mock(mock: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/contextualizedembeddings"))
        .respond_with(|req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let groups: Vec<usize> = body["inputs"]
                .as_array()
                .map(|gs| {
                    gs.iter()
                        .map(|g| g.as_array().map_or(0, Vec::len))
                        .collect()
                })
                .unwrap_or_default();
            let total: usize = groups.iter().sum();
            let data: Vec<Value> = groups
                .iter()
                .enumerate()
                .map(|(gi, &n)| {
                    json!({
                        "index": gi,
                        "data": (0..n)
                            .map(|k| json!({ "embedding": vec![0.0_f32; 1024], "index": k }))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({
                "data": data,
                "model": "voyage-context-3",
                "usage": { "total_tokens": total }
            }))
        })
        .mount(mock)
        .await;
}

#[tokio::test]
async fn embeds_via_voyage_and_charges_tokens() {
    let h = common::boot().await;

    // Mock Voyage's contextualized endpoint: the default `type=general` flat
    // input embeds each text as its own single-chunk document.
    let mock = MockServer::start().await;
    mount_contextualized_mock(&mock).await;

    // Pin the corpus model to voyage-context-3@1 / 1024 dims so the response
    // echoes it and any `model` assertion would match.
    let corpus_model = pinned_corpus_model();

    // Generous limits so the request is allowed; assert the post-charge balance.
    let token_limiter = generous_limiter();

    // The general path never touches the flat embedder: `voyage = None` proves
    // the routing (a misroute would 503 "code embedder not configured").
    let voyage_ctx = Some(Arc::new(
        ContextualizedVoyageEmbedder::new("k", "voyage-context-3", 1024, "float")
            .with_base_url(&mock.uri()),
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
        None,
        voyage_ctx,
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

    // The mock charges one token per chunk: one input text = 1 token.
    assert_eq!(body["usage"]["total_tokens"], 1, "{body}");
    assert_eq!(body["model"], "voyage-context-3@1", "{body}");
    assert_eq!(
        body["rate"]["hour"]["remaining"],
        2000 - 1,
        "anon hourly remaining after charging 1 token: {body}"
    );
    assert_eq!(
        body["embeddings"].as_array().map(Vec::len),
        Some(1),
        "one vector returned: {body}"
    );
}

/// `type=code` routes to the flat Voyage endpoint and reports the CODE model
/// wire id (not the corpus model).
#[tokio::test]
async fn code_type_embeds_via_flat_voyage_and_reports_code_model() {
    let h = common::boot().await;

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

    let voyage = Some(Arc::new(
        VoyageEmbedder::new("k", "voyage-code-3", 1024, "float").with_base_url(&mock.uri()),
    ));

    let app = app::build_with_limiter(
        h.pool.clone(),
        ServerConfig::default(),
        None,
        pinned_corpus_model(),
        generous_limiter(),
        voyage,
        None,
        pinned_code_model(),
    )
    .expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "input": ["hi"], "type": "code" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["usage"]["total_tokens"], 5, "{body}");
    assert_eq!(body["model"], "voyage-code-3@1", "code wire id expected: {body}");
    assert_eq!(body["embeddings"].as_array().map(Vec::len), Some(1), "{body}");
}

/// Nested context groups (general type) pass through to the contextualized
/// endpoint and come back flattened row-per-chunk in input order.
#[tokio::test]
async fn general_nested_groups_return_flattened_vectors() {
    let h = common::boot().await;

    let mock = MockServer::start().await;
    mount_contextualized_mock(&mock).await;

    let voyage_ctx = Some(Arc::new(
        ContextualizedVoyageEmbedder::new("k", "voyage-context-3", 1024, "float")
            .with_base_url(&mock.uri()),
    ));

    let app = app::build_with_limiter(
        h.pool.clone(),
        ServerConfig::default(),
        None,
        pinned_corpus_model(),
        generous_limiter(),
        None,
        voyage_ctx,
        Arc::new(RwLock::new(None)),
    )
    .expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "input": [["a", "b"], ["c"]],
                        "input_type": "document",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(
        body["embeddings"].as_array().map(Vec::len),
        Some(3),
        "three chunks across two groups flatten to three vectors: {body}"
    );
    assert_eq!(body["model"], "voyage-context-3@1", "{body}");
}

/// Contract (§9): nested input is only valid with `type=general` —
/// `type=code` + nested → 400 before any model/token work.
#[tokio::test]
async fn nested_input_with_code_type_returns_400() {
    let h = common::boot().await;

    let app = app::build_with_limiter(
        h.pool.clone(),
        ServerConfig::default(),
        None,
        pinned_corpus_model(),
        generous_limiter(),
        Some(unreachable_voyage()),
        None,
        pinned_code_model(),
    )
    .expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "input": [["a", "b"]], "type": "code" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("type=general")),
        "400 should explain nested input is general-only: {body}"
    );
}

/// Contract (§9): each nested group must fit the per-document context budget
/// (~28 800 tokens via the 4-bytes/token estimate) — an oversized group → 413.
#[tokio::test]
async fn oversized_nested_group_returns_413() {
    let h = common::boot().await;

    let app = app::build_with_limiter(
        h.pool.clone(),
        ServerConfig::default(),
        None,
        pinned_corpus_model(),
        generous_limiter(),
        Some(unreachable_voyage()),
        None,
        Arc::new(RwLock::new(None)),
    )
    .expect("build app");

    // 120 000 chars ≈ 30 000 estimated tokens > the 28 800-token group budget.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "input": [["x".repeat(120_000)]] }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

/// Contract (§9): the optional `model` pin is checked against the model the
/// requested `type` resolves to — pinning the OTHER type's wire id → 409.
#[tokio::test]
async fn model_pin_mismatching_type_resolved_model_returns_409() {
    let h = common::boot().await;

    let app = app::build_with_limiter(
        h.pool.clone(),
        ServerConfig::default(),
        None,
        pinned_corpus_model(), // voyage-context-3@1
        generous_limiter(),
        Some(unreachable_voyage()),
        None,
        pinned_code_model(), // voyage-code-3@1
    )
    .expect("build app");

    // General request pinning the CODE wire id mismatches the corpus model.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "input": ["hi"], "model": "voyage-code-3@1" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT, "general + code pin must 409");

    // Code request pinning the GENERAL wire id mismatches the code model.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "input": ["hi"],
                        "type": "code",
                        "model": "voyage-context-3@1",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT, "code + general pin must 409");
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

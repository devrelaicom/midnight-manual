//! Integration tests for `GET /v1/me` — auth + limit introspection.
//!
//! Covers the anonymous shape, the admin-JWT shape, the rate-limit object when
//! the limiter is enabled (and that the handler's peek does not spend a
//! token), and that an actual `POST /v1/embeddings` charge is visible in the
//! reported token budget.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use std::sync::{Arc, RwLock};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use midnight_manual_server::config::ServerConfig;
use midnight_manual_server::corpus_model::CorpusModel;
use midnight_manual_server::tokenlimit::{Limits, TokenUsageLimiter};
use midnight_manual_server::{app, ratelimit::RateLimiter};
use mnm_auth::Keypair;
use mnm_embedding::voyage::VoyageEmbedder;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg_with_auth(user_store_body: String, jwt_secret_bytes: Vec<u8>) -> ServerConfig {
    ServerConfig {
        user_store_body: Some(user_store_body),
        jwt_secret: Some(jwt_secret_bytes),
        ..Default::default()
    }
}

fn admin_user_store(user_id: &str, kp: &Keypair) -> String {
    format!(
        r#"
schema_version = 1

[[users]]
user_id = "{user_id}"
role = "admin"
public_key = "{wire}"
created_at = "2026-05-14"
"#,
        wire = kp.public_wire(),
    )
}

async fn call(
    app: axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    let req = if let Some(b) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn mint_token(app: axum::Router, user_id: &str, kp: &Keypair) -> String {
    let (_, body) = call(
        app.clone(),
        "POST",
        "/v1/auth/challenge",
        None,
        Some(json!({"user_id": user_id})),
    )
    .await;
    let challenge_id = body["challenge_id"].as_str().unwrap().to_owned();
    let nonce = STANDARD_NO_PAD
        .decode(body["nonce_b64"].as_str().unwrap())
        .unwrap();
    let signature_b64 = STANDARD_NO_PAD.encode(kp.sign(&nonce));
    let (_, body) = call(
        app,
        "POST",
        "/v1/auth/verify",
        None,
        Some(json!({"challenge_id": challenge_id, "signature_b64": signature_b64})),
    )
    .await;
    body["token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn anonymous_me_reports_unauthenticated_with_token_budget() {
    let h = common::boot().await;
    // Default config: rate limiting disabled, anon token ceilings 2000/20000.
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let (status, v) = call(app.clone(), "GET", "/v1/me", None, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["authenticated"], false, "{v}");
    assert_eq!(v["auth_type"], "anonymous", "{v}");
    assert_eq!(v["permission_level"], "read", "{v}");
    assert!(v["identity"].is_null(), "{v}");
    assert!(
        !v["server_version"].as_str().unwrap_or("").is_empty(),
        "server_version must be non-empty: {v}"
    );
    // Rate limiting is disabled in the default test config — no bucket exists.
    assert!(v["rate_limit"].is_null(), "{v}");
    let tl = &v["token_limits"];
    assert_eq!(tl["tier"], "anonymous", "{v}");
    assert!(tl["hourly"]["limit"].as_u64().unwrap() > 0, "{v}");
    assert!(tl["daily"]["limit"].as_u64().unwrap() > 0, "{v}");
    // Nothing charged yet: full headroom.
    assert_eq!(tl["hourly"]["remaining"], tl["hourly"]["limit"], "{v}");

    // A second /v1/me must not charge the token budget (snapshot_for is
    // read-only).
    let (status2, v2) = call(app, "GET", "/v1/me", None, None).await;
    assert_eq!(status2, StatusCode::OK, "{v2}");
    assert_eq!(
        v2["token_limits"]["hourly"]["remaining"], v["token_limits"]["hourly"]["remaining"],
        "introspection must not consume token budget: {v2}"
    );
}

#[tokio::test]
async fn admin_me_reports_identity_and_admin_tier() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    let (status, v) = call(app, "GET", "/v1/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["authenticated"], true, "{v}");
    assert_eq!(v["auth_type"], "admin", "{v}");
    assert_eq!(v["identity"], "aaron", "{v}");
    assert_eq!(v["permission_level"], "admin", "{v}");
    assert_eq!(v["token_limits"]["tier"], "admin", "{v}");
    // Rate limiting stays disabled in this config.
    assert!(v["rate_limit"].is_null(), "{v}");
}

#[tokio::test]
async fn rate_limit_enabled_reports_bucket_and_peek_does_not_spend() {
    let h = common::boot().await;
    // Pin anonymous to 2 rps: the bucket refills continuously at `rps`
    // tokens/sec, so a low rate keeps the within-request refill (middleware
    // charge -> handler peek) far below one whole token even on a slow runner.
    let cfg = ServerConfig {
        rate_limit_enabled: true,
        rate_limit_anonymous_rps: 2,
        ..Default::default()
    };
    let anon_limit = u64::from(cfg.rate_limit_anonymous_rps);
    let limiter = RateLimiter::from_config(&cfg);
    assert!(limiter.is_some(), "limiter must be enabled");
    let corpus_model = Arc::new(RwLock::new(None));
    let token_limiter = TokenUsageLimiter::from_config(&cfg);
    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        limiter,
        corpus_model,
        token_limiter,
        None,
        None,
        Arc::new(RwLock::new(None)),
    )
    .expect("build app");

    let (status, v) = call(app.clone(), "GET", "/v1/me", None, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let rl = &v["rate_limit"];
    assert_eq!(rl["tier"], "anonymous", "{v}");
    assert_eq!(rl["limit"].as_u64(), Some(anon_limit), "{v}");
    // The middleware charged exactly 1 token for this request; the handler's
    // peek charges 0. If the peek spent a token this would read limit - 2.
    assert_eq!(
        rl["remaining"].as_u64(),
        Some(anon_limit - 1),
        "handler peek must not spend a token: {v}"
    );

    // The bucket persists across requests: a second call reports the same
    // limit and a remaining strictly below the cap.
    let (status2, v2) = call(app, "GET", "/v1/me", None, None).await;
    assert_eq!(status2, StatusCode::OK, "{v2}");
    assert_eq!(v2["rate_limit"]["limit"].as_u64(), Some(anon_limit), "{v2}");
    assert!(
        v2["rate_limit"]["remaining"].as_u64().unwrap() < anon_limit,
        "second request must also be charged: {v2}"
    );
}

#[tokio::test]
async fn embeddings_charge_is_visible_in_me_token_budget() {
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

    let corpus_model = Arc::new(RwLock::new(Some(CorpusModel {
        wire: "voyage-code-3@1".to_owned(),
        name: "voyage-code-3".to_owned(),
        id: Uuid::new_v4(),
        dim: 1024,
    })));
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
    // `type=code` snapshots the resolved code model for the wire id; a
    // synthetic entry suffices (the flat embedder above does the work).
    let code_model = Arc::new(RwLock::new(Some(midnight_manual_server::code_model::CodeModel {
        wire: "voyage-code-3@1".to_owned(),
        name: "voyage-code-3".to_owned(),
        id: Uuid::new_v4(),
        dim: 1024,
    })));
    let app = app::build_with_limiter(
        h.pool.clone(),
        ServerConfig::default(),
        None,
        corpus_model,
        token_limiter,
        voyage,
        None,
        code_model,
    )
    .expect("build app");

    // Both requests carry no client-IP headers, so they resolve to the same
    // anonymous subject and /v1/me sees the embeddings charge.
    let (status, v) = call(
        app.clone(),
        "POST",
        "/v1/embeddings",
        None,
        Some(json!({ "input": ["hi"], "input_type": "query", "type": "code" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["usage"]["total_tokens"], 5, "{v}");

    let (status, v) = call(app, "GET", "/v1/me", None, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let tl = &v["token_limits"];
    assert_eq!(tl["tier"], "anonymous", "{v}");
    assert_eq!(tl["hourly"]["limit"], 2000, "{v}");
    assert_eq!(
        tl["hourly"]["remaining"], 1995,
        "the 5-token embeddings charge must show in /v1/me: {v}"
    );
    assert_eq!(tl["daily"]["remaining"], 19995, "{v}");
}

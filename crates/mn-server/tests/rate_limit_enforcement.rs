//! End-to-end exercises for rate-limit enforcement (Phase 17).
//!
//! Each test sets a distinct `Fly-Client-IP` so concurrent tests (and the
//! shared CI Postgres) never share a bucket. The read-uplift tier is covered
//! by the engine unit tests (`ratelimit::tests`); here the authenticated
//! higher-tier path is exercised end-to-end with an admin token.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{HeaderMap, Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use mn_auth::Keypair;
use mn_server::app;
use mn_server::config::ServerConfig;
use mn_server::ratelimit::RateLimiter;
use mn_server::tokenlimit::TokenUsageLimiter;
use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;

fn enabled_cfg(anonymous_rps: u32) -> ServerConfig {
    ServerConfig {
        rate_limit_enabled: true,
        rate_limit_anonymous_rps: anonymous_rps,
        rate_limit_uplift_rps: 1000,
        rate_limit_admin_rps: 1000,
        ..Default::default()
    }
}

fn enabled_auth_cfg(anonymous_rps: u32, user_store_body: String) -> ServerConfig {
    ServerConfig {
        user_store_body: Some(user_store_body),
        jwt_secret: Some(vec![7u8; 32]),
        ..enabled_cfg(anonymous_rps)
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

async fn send(
    app: axum::Router,
    method: &str,
    uri: &str,
    ip: &str,
    bearer: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("fly-client-ip", ip);
    if let Some(t) = bearer {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    let resp = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body)
}

/// Mint an admin token via challenge/verify. The two calls go through the
/// rate limiter, so they carry their own `mint_ip` (which must have enough
/// anonymous headroom for two requests).
async fn mint_token(app: axum::Router, user_id: &str, kp: &Keypair, mint_ip: &str) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/challenge")
                .header("content-type", "application/json")
                .header("fly-client-ip", mint_ip)
                .body(Body::from(json!({ "user_id": user_id }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let challenge_id = body["challenge_id"].as_str().unwrap().to_owned();
    let nonce = STANDARD_NO_PAD
        .decode(body["nonce_b64"].as_str().unwrap())
        .unwrap();
    let signature_b64 = STANDARD_NO_PAD.encode(kp.sign(&nonce));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/verify")
                .header("content-type", "application/json")
                .header("fly-client-ip", mint_ip)
                .body(Body::from(
                    json!({ "challenge_id": challenge_id, "signature_b64": signature_b64 })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    body["token"].as_str().unwrap().to_owned()
}

fn unique_ip() -> String {
    let b = Uuid::new_v4().into_bytes();
    format!("198.51.{}.{}", b[0], b[1])
}

#[tokio::test]
async fn success_carries_ratelimit_headers() {
    let h = common::boot().await;
    let cfg = enabled_cfg(100);
    let limiter = RateLimiter::from_config(&cfg);
    let token_limiter = TokenUsageLimiter::from_config(&cfg);
    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        limiter,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
        token_limiter,
        None,
        None,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
    )
    .expect("build");
    let (status, headers, _) = send(app, "GET", "/v1/sources", &unique_ip(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.contains_key("x-ratelimit-limit"), "limit header present");
    assert!(headers.contains_key("x-ratelimit-remaining"), "remaining header present");
    assert!(headers.contains_key("x-ratelimit-reset"), "reset header present");
    assert_eq!(headers.get("x-ratelimit-limit").unwrap(), "100");
}

#[tokio::test]
async fn anonymous_over_budget_returns_429_with_retry_after() {
    let h = common::boot().await;
    let cfg = enabled_cfg(2);
    let limiter = RateLimiter::from_config(&cfg);
    let token_limiter = TokenUsageLimiter::from_config(&cfg);
    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        limiter,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
        token_limiter,
        None,
        None,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
    )
    .expect("build");
    let ip = unique_ip();
    let (s1, _, _) = send(app.clone(), "GET", "/v1/sources", &ip, None).await;
    let (s2, _, _) = send(app.clone(), "GET", "/v1/sources", &ip, None).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    let (s3, headers, body) = send(app, "GET", "/v1/sources", &ip, None).await;
    assert_eq!(s3, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(headers.contains_key("retry-after"), "retry-after present");
    assert_eq!(headers.get("x-ratelimit-remaining").unwrap(), "0");
    assert_eq!(body["error"]["code"], "rate_limited");
    assert!(
        body["error"]["message"].as_str().unwrap().contains("req/s"),
        "message names the limit: {body}"
    );
}

#[tokio::test]
async fn health_is_exempt_from_limiting() {
    let h = common::boot().await;
    let cfg = enabled_cfg(1);
    let limiter = RateLimiter::from_config(&cfg);
    let token_limiter = TokenUsageLimiter::from_config(&cfg);
    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        limiter,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
        token_limiter,
        None,
        None,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
    )
    .expect("build");
    let ip = unique_ip();
    for _ in 0..5 {
        let (s, _, _) = send(app.clone(), "GET", "/healthz", &ip, None).await;
        assert_ne!(s, StatusCode::TOO_MANY_REQUESTS, "health must never throttle");
    }
}

#[tokio::test]
async fn cidr_override_raises_the_limit() {
    let h = common::boot().await;
    let cfg = enabled_cfg(1); // anon floor = 1 rps
    let limiter = RateLimiter::from_config(&cfg).expect("enabled");
    let token_limiter = TokenUsageLimiter::from_config(&cfg);
    let b = Uuid::new_v4().into_bytes();
    let net = format!("203.0.{}.0/24", b[0]);
    let ip = format!("203.0.{}.7", b[0]);
    mn_store::entities::rate_limit_override::insert(
        &h.pool,
        &net,
        50,
        OffsetDateTime::now_utc() + Duration::hours(1),
        Some(&format!("test-{}", Uuid::new_v4())),
        "rl-test",
    )
    .await
    .expect("seed override");
    limiter
        .refresh_overrides_now(&h.pool)
        .await
        .expect("refresh");

    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        Some(Arc::clone(&limiter)),
        std::sync::Arc::new(std::sync::RwLock::new(None)),
        token_limiter,
        None,
        None,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
    )
    .expect("build");
    // Anon floor of 1 would 429 the third request; the /24 override (50 rps)
    // keeps all three at 200 with the override's limit in the header.
    for _ in 0..3 {
        let (s, headers, body) = send(app.clone(), "GET", "/v1/sources", &ip, None).await;
        assert_eq!(s, StatusCode::OK, "override should permit the request: {body}");
        assert_eq!(headers.get("x-ratelimit-limit").unwrap(), "50");
    }
}

#[tokio::test]
async fn admin_token_gets_the_top_tier() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let user = format!("admin-{}", Uuid::new_v4().simple());
    // Anon floor of 2 leaves headroom for the two-call mint handshake on the
    // mint IP; the asserted requests carry the admin token (a separate bucket).
    let cfg = enabled_auth_cfg(2, admin_user_store(&user, &kp));
    let limiter = RateLimiter::from_config(&cfg);
    let token_limiter = TokenUsageLimiter::from_config(&cfg);
    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        limiter,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
        token_limiter,
        None,
        None,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
    )
    .expect("build");
    let token = mint_token(app.clone(), &user, &kp, &unique_ip()).await;
    let ip = unique_ip();
    // With an admin token the tier is admin (1000 rps), independent of the IP
    // anon floor, so the header reports the top tier and requests don't 429.
    for _ in 0..3 {
        let (s, headers, body) = send(app.clone(), "GET", "/v1/sources", &ip, Some(&token)).await;
        assert_eq!(s, StatusCode::OK, "admin tier should permit the request: {body}");
        assert_eq!(headers.get("x-ratelimit-limit").unwrap(), "1000");
    }
}

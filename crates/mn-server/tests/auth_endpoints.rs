//! End-to-end HTTP exercises for the `POST /v1/auth/{challenge,verify}`
//! flow plus the bearer-extraction middleware.
//!
//! Driven via `tower::ServiceExt::oneshot` so no real listener is needed.
//! Uses `common::boot()` for the pool because `app::build` requires a
//! `PgPool` — the auth handlers themselves don't touch the DB.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use mn_auth::Keypair;
use mn_server::{app, config::ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

fn cfg_with_auth(user_store_body: String, jwt_secret_bytes: Vec<u8>) -> ServerConfig {
    ServerConfig {
        database_url: String::new(),
        port: 0,
        auto_migrate: false,
        corpus_model: Some("bge-base-en-v1.5@1".to_owned()),
        user_store_body: Some(user_store_body),
        jwt_secret: Some(jwt_secret_bytes),
    }
}

fn user_store_for(user_id: &str, kp: &Keypair) -> String {
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

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn happy_path_challenge_then_verify_mints_jwt() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    // 1. Mint a challenge.
    let (status, body) = post(app.clone(), "/v1/auth/challenge", json!({"user_id": "aaron"})).await;
    assert_eq!(status, StatusCode::OK, "challenge: {body}");
    let challenge_id = body["challenge_id"].as_str().unwrap().to_owned();
    let nonce_b64 = body["nonce_b64"].as_str().unwrap();
    let nonce = STANDARD_NO_PAD.decode(nonce_b64).unwrap();

    // 2. Sign the nonce client-side.
    let signature = kp.sign(&nonce);
    let signature_b64 = STANDARD_NO_PAD.encode(signature);

    // 3. Verify — mints a JWT.
    let (status, body) = post(
        app,
        "/v1/auth/verify",
        json!({"challenge_id": challenge_id, "signature_b64": signature_b64}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "verify: {body}");
    assert_eq!(body["user_id"], "aaron");
    let token = body["token"].as_str().expect("token present");
    assert!(token.contains('.'), "token must look like a JWT");
}

#[tokio::test]
async fn challenge_for_unknown_user_404s() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, body) = post(app, "/v1/auth/challenge", json!({"user_id": "imposter"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn verify_with_wrong_signature_403s() {
    let h = common::boot().await;
    let aaron_kp = Keypair::generate();
    let imposter_kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &aaron_kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (_, body) = post(app.clone(), "/v1/auth/challenge", json!({"user_id": "aaron"})).await;
    let challenge_id = body["challenge_id"].as_str().unwrap().to_owned();
    let nonce = STANDARD_NO_PAD
        .decode(body["nonce_b64"].as_str().unwrap())
        .unwrap();

    // Sign with the wrong key.
    let bad_signature = imposter_kp.sign(&nonce);
    let bad_b64 = STANDARD_NO_PAD.encode(bad_signature);

    let (status, body) = post(
        app,
        "/v1/auth/verify",
        json!({"challenge_id": challenge_id, "signature_b64": bad_b64}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "forbidden");
}

#[tokio::test]
async fn verify_with_consumed_challenge_404s_on_replay() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (_, body) = post(app.clone(), "/v1/auth/challenge", json!({"user_id": "aaron"})).await;
    let challenge_id = body["challenge_id"].as_str().unwrap().to_owned();
    let nonce = STANDARD_NO_PAD
        .decode(body["nonce_b64"].as_str().unwrap())
        .unwrap();
    let signature_b64 = STANDARD_NO_PAD.encode(kp.sign(&nonce));

    // First verify wins.
    let (status, _) = post(
        app.clone(),
        "/v1/auth/verify",
        json!({"challenge_id": &challenge_id, "signature_b64": &signature_b64}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Replay must fail.
    let (status, body) = post(
        app,
        "/v1/auth/verify",
        json!({"challenge_id": challenge_id, "signature_b64": signature_b64}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn auth_endpoints_503_when_auth_unconfigured() {
    let h = common::boot().await;
    let cfg = ServerConfig {
        database_url: String::new(),
        port: 0,
        auto_migrate: false,
        corpus_model: Some("bge-base-en-v1.5@1".to_owned()),
        user_store_body: None,
        jwt_secret: None,
    };
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, body) = post(app, "/v1/auth/challenge", json!({"user_id": "aaron"})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
}

#[tokio::test]
async fn bearer_middleware_rejects_bad_jwt() {
    // Existing public endpoint (`/v1/sources`) accepts anonymous traffic AND
    // valid bearers. A malformed bearer must 401.
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/sources")
                .header("Authorization", "Bearer not.a.real.jwt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn bearer_middleware_accepts_valid_jwt() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    // First mint a real JWT through the public flow.
    let (_, body) = post(app.clone(), "/v1/auth/challenge", json!({"user_id": "aaron"})).await;
    let challenge_id = body["challenge_id"].as_str().unwrap().to_owned();
    let nonce = STANDARD_NO_PAD
        .decode(body["nonce_b64"].as_str().unwrap())
        .unwrap();
    let signature_b64 = STANDARD_NO_PAD.encode(kp.sign(&nonce));
    let (_, body) = post(
        app.clone(),
        "/v1/auth/verify",
        json!({"challenge_id": challenge_id, "signature_b64": signature_b64}),
    )
    .await;
    let token = body["token"].as_str().unwrap();

    // Then use it on a public read endpoint — must succeed with 200.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/sources")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn bearer_middleware_skips_when_auth_unconfigured() {
    // No user store / jwt secret in config: middleware is a passthrough,
    // and an Authorization header (even a bogus one) is ignored.
    let h = common::boot().await;
    let cfg = ServerConfig {
        database_url: String::new(),
        port: 0,
        auto_migrate: false,
        corpus_model: Some("bge-base-en-v1.5@1".to_owned()),
        user_store_body: None,
        jwt_secret: None,
    };
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/sources")
                .header("Authorization", "Bearer literally-anything")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

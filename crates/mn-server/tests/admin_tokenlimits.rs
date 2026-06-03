//! End-to-end exercises for `/v1/admin/tokenlimits` CRUD (Task 4.8).
//!
//! Mirrors the `/v1/admin/ratelimits` CRUD test. CI shares one Postgres across
//! test binaries, so every test mints its admin token under a unique
//! `user_id`, which the server records as the row's `created_by`; list
//! assertions filter to that sentinel rather than trusting the global active
//! set.

#![cfg(feature = "integration")]
#![allow(missing_docs, clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use mn_auth::Keypair;
use mn_server::{app, config::ServerConfig};
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;

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

fn writer_user_store(
    admin_id: &str,
    admin_kp: &Keypair,
    writer_id: &str,
    writer_kp: &Keypair,
) -> String {
    format!(
        r#"
schema_version = 1

[[users]]
user_id = "{admin_id}"
role = "admin"
public_key = "{admin_wire}"
created_at = "2026-05-14"

[[users]]
user_id = "{writer_id}"
role = "writer"
public_key = "{writer_wire}"
created_at = "2026-05-14"
"#,
        admin_wire = admin_kp.public_wire(),
        writer_wire = writer_kp.public_wire(),
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

/// Unique admin user_id → becomes the row's `created_by` sentinel.
fn unique_admin() -> String {
    format!("admin-{}", Uuid::new_v4().simple())
}

fn rfc3339(offset: Duration) -> String {
    (OffsetDateTime::now_utc() + offset)
        .format(&Rfc3339)
        .unwrap()
}

/// Boot an app with a single admin user; returns `(app, token, user_id)`.
async fn admin_app(pool: sqlx::PgPool) -> (axum::Router, String, String) {
    let kp = Keypair::generate();
    let who = unique_admin();
    let cfg = cfg_with_auth(admin_user_store(&who, &kp), vec![7u8; 32]);
    let app = app::build(pool, cfg).expect("build app");
    let token = mint_token(app.clone(), &who, &kp).await;
    (app, token, who)
}

#[tokio::test]
async fn create_user_override_happy_path() {
    let h = common::boot().await;
    let (app, token, who) = admin_app(h.pool.clone()).await;

    let (status, body) = call(
        app,
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "user",
            "subject": "alice",
            "hourly": 50_000,
            "daily": 500_000,
            "expires_at": rfc3339(Duration::hours(48)),
            "note": "uplift-for-bulk-ingest"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["subject_kind"], "user");
    assert_eq!(body["subject"], "alice");
    assert_eq!(body["hourly"], 50_000);
    assert_eq!(body["daily"], 500_000);
    assert_eq!(body["note"], "uplift-for-bulk-ingest");
    assert_eq!(body["created_by"], who, "created_by reflects the token sub");
}

#[tokio::test]
async fn create_cidr_override_masks_host_bits() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;

    let (status, body) = call(
        app,
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "cidr",
            "subject": "169.155.237.15/25",
            "hourly": 1_000,
            "daily": 10_000,
            "expires_at": rfc3339(Duration::hours(2))
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["subject"], "169.155.237.0/25", "host bits masked: {body}");
}

#[tokio::test]
async fn create_allows_zero_ceiling() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;

    // Zero is a valid ceiling (denies all spend); only negatives are rejected.
    let (status, body) = call(
        app,
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "user",
            "subject": "blocked-user",
            "hourly": 0,
            "daily": 0,
            "expires_at": rfc3339(Duration::hours(1))
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["hourly"], 0);
    assert_eq!(body["daily"], 0);
}

#[tokio::test]
async fn create_rejects_negative_ceiling() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;

    let (status, body) = call(
        app,
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "user",
            "subject": "alice",
            "hourly": -1,
            "daily": 10,
            "expires_at": rfc3339(Duration::hours(1))
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn create_rejects_bad_subject_kind() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;

    let (status, body) = call(
        app,
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "banana",
            "subject": "alice",
            "hourly": 10,
            "daily": 100,
            "expires_at": rfc3339(Duration::hours(1))
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn create_rejects_bad_cidr_subject() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;

    let (status, body) = call(
        app,
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "cidr",
            "subject": "not-an-ip",
            "hourly": 10,
            "daily": 100,
            "expires_at": rfc3339(Duration::hours(1))
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn create_rejects_past_expiry() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;

    let (status, body) = call(
        app,
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "user",
            "subject": "alice",
            "hourly": 10,
            "daily": 100,
            "expires_at": rfc3339(-Duration::hours(1))
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn list_returns_active_only_scoped_to_creator() {
    let h = common::boot().await;
    let (app, token, who) = admin_app(h.pool.clone()).await;

    let (s1, active) = call(
        app.clone(),
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "user",
            "subject": "alice",
            "hourly": 10,
            "daily": 100,
            "expires_at": rfc3339(Duration::hours(2))
        })),
    )
    .await;
    assert_eq!(s1, StatusCode::CREATED, "{active}");

    // Seed an expired row directly via the store (the API refuses past expiry).
    mn_store::entities::token_limit_override::insert(
        &h.pool,
        "user",
        "expired-bob",
        5,
        50,
        OffsetDateTime::now_utc() - Duration::hours(1),
        None,
        &who,
    )
    .await
    .expect("seed expired");

    let (status, body) = call(app, "GET", "/v1/admin/tokenlimits", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ours: Vec<&Value> = body
        .as_array()
        .expect("array")
        .iter()
        .filter(|r| r["created_by"] == who)
        .collect();
    assert_eq!(ours.len(), 1, "only the active row appears: {body}");
    assert_eq!(ours[0]["id"], active["id"]);
}

#[tokio::test]
async fn patch_override_extends_and_adjusts() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;
    let (_, created) = call(
        app.clone(),
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "user",
            "subject": "alice",
            "hourly": 10,
            "daily": 100,
            "expires_at": rfc3339(Duration::hours(1))
        })),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let (status, body) = call(
        app,
        "PATCH",
        &format!("/v1/admin/tokenlimits/{id}"),
        Some(&token),
        Some(json!({"expires_at": rfc3339(Duration::hours(72)), "hourly": 999, "daily": 9_999})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["hourly"], 999);
    assert_eq!(body["daily"], 9_999);
    assert_eq!(body["id"], created["id"]);
}

#[tokio::test]
async fn patch_rejects_negative_ceiling() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;
    let (_, created) = call(
        app.clone(),
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "user",
            "subject": "alice",
            "hourly": 10,
            "daily": 100,
            "expires_at": rfc3339(Duration::hours(1))
        })),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let (status, body) = call(
        app,
        "PATCH",
        &format!("/v1/admin/tokenlimits/{id}"),
        Some(&token),
        Some(json!({"daily": -1})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn patch_404_on_unknown_id() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;
    let (status, body) = call(
        app,
        "PATCH",
        &format!("/v1/admin/tokenlimits/{}", Uuid::new_v4()),
        Some(&token),
        Some(json!({"hourly": 5})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn delete_override_then_404() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;
    let (_, created) = call(
        app.clone(),
        "POST",
        "/v1/admin/tokenlimits",
        Some(&token),
        Some(json!({
            "subject_kind": "user",
            "subject": "alice",
            "hourly": 10,
            "daily": 100,
            "expires_at": rfc3339(Duration::hours(1))
        })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();

    let (status, body) = call(
        app.clone(),
        "DELETE",
        &format!("/v1/admin/tokenlimits/{id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], created["id"]);

    // Second delete is a 404.
    let (s2, _) =
        call(app, "DELETE", &format!("/v1/admin/tokenlimits/{id}"), Some(&token), None).await;
    assert_eq!(s2, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_404_on_unknown_id() {
    let h = common::boot().await;
    let (app, token, _) = admin_app(h.pool.clone()).await;
    let (status, _) = call(
        app,
        "DELETE",
        &format!("/v1/admin/tokenlimits/{}", Uuid::new_v4()),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unauthenticated_requests_return_401() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    for (method, uri, body) in [
        (
            "POST",
            "/v1/admin/tokenlimits",
            Some(json!({
                "subject_kind": "user",
                "subject": "alice",
                "hourly": 10,
                "daily": 100,
                "expires_at": "2030-01-01T00:00:00Z"
            })),
        ),
        ("GET", "/v1/admin/tokenlimits", None),
        (
            "PATCH",
            "/v1/admin/tokenlimits/00000000-0000-0000-0000-000000000000",
            Some(json!({"hourly": 5})),
        ),
        ("DELETE", "/v1/admin/tokenlimits/00000000-0000-0000-0000-000000000000", None),
    ] {
        let (status, _) = call(app.clone(), method, uri, None, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn writer_role_is_forbidden() {
    let h = common::boot().await;
    let admin_kp = Keypair::generate();
    let writer_kp = Keypair::generate();
    let cfg = cfg_with_auth(
        writer_user_store("aaron", &admin_kp, "writer-user", &writer_kp),
        vec![7u8; 32],
    );
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "writer-user", &writer_kp).await;

    for (method, uri, body) in [
        (
            "POST",
            "/v1/admin/tokenlimits",
            Some(json!({
                "subject_kind": "user",
                "subject": "alice",
                "hourly": 10,
                "daily": 100,
                "expires_at": "2030-01-01T00:00:00Z"
            })),
        ),
        ("GET", "/v1/admin/tokenlimits", None),
        (
            "PATCH",
            "/v1/admin/tokenlimits/00000000-0000-0000-0000-000000000000",
            Some(json!({"hourly": 5})),
        ),
        ("DELETE", "/v1/admin/tokenlimits/00000000-0000-0000-0000-000000000000", None),
    ] {
        let (status, body) = call(app.clone(), method, uri, Some(&token), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
        assert_eq!(body["error"]["code"], "forbidden");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("writer"),
            "403 names the caller role: {body}"
        );
    }
}

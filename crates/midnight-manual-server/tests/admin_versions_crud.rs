//! End-to-end exercises for `/v1/(admin/)sources/:slug/versions` (Phase 14).

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use midnight_manual_server::{app, config::ServerConfig};
use mnm_auth::Keypair;
use mnm_core::types::SourceKind;
use mnm_store::entities::{embedding_model, source, source_version};
use serde_json::{json, Value};
use sqlx::PgPool;
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

async fn mint_admin(app: axum::Router, user_id: &str, kp: &Keypair) -> String {
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

/// Seed a source with `n` finalized source_versions. Returns the slug and
/// the source_version ids in ascending revision order.
async fn seed_source_with_versions(
    pool: &PgPool,
    prefix: &str,
    n: usize,
) -> (String, Uuid, Vec<Uuid>) {
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("{prefix}-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Versions Fixture", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let (id, _rev) = source_version::create_building(
            pool,
            source_id,
            model_id,
            None,
            "0.1.0",
            &format!("h{i}"),
        )
        .await
        .unwrap();
        source_version::finalize(pool, id).await.unwrap();
        ids.push(id);
    }
    (slug, source_id, ids)
}

// ===== Public reads =====

#[tokio::test]
async fn list_versions_returns_array_newest_first() {
    let h = common::boot().await;
    let cfg = ServerConfig::default();
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let (slug, _, _) = seed_source_with_versions(&h.pool, "list-public", 3).await;

    let (status, body) =
        call(app, "GET", &format!("/v1/sources/{slug}/versions"), None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["revision"], 3);
    assert_eq!(arr[0]["is_active"], true);
    assert_eq!(arr[1]["revision"], 2);
    assert_eq!(arr[2]["revision"], 1);
}

#[tokio::test]
async fn list_versions_404_on_unknown_slug() {
    let h = common::boot().await;
    let cfg = ServerConfig::default();
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, body) = call(app, "GET", "/v1/sources/no-such-source/versions", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn get_version_happy_path() {
    let h = common::boot().await;
    let cfg = ServerConfig::default();
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let (slug, _, _) = seed_source_with_versions(&h.pool, "get-public", 2).await;

    let (status, body) =
        call(app, "GET", &format!("/v1/sources/{slug}/versions/1"), None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], 1);
    assert_eq!(body["is_active"], false);
}

#[tokio::test]
async fn get_version_404_on_unknown_revision() {
    let h = common::boot().await;
    let cfg = ServerConfig::default();
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let (slug, _, _) = seed_source_with_versions(&h.pool, "get-missing", 1).await;

    let (status, body) =
        call(app, "GET", &format!("/v1/sources/{slug}/versions/999"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

// ===== Admin promote =====

#[tokio::test]
async fn promote_happy_path_swaps_active() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin(app.clone(), "aaron", &kp).await;
    let (slug, _, _) = seed_source_with_versions(&h.pool, "promote-ok", 3).await;
    // State: rev 3 active, 2/1 inactive.

    let (status, body) = call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/versions/1/promote"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["promoted_revision"], 1);
    assert_eq!(body["demoted_revision"], 3);

    // Re-fetch and confirm.
    let (_, list) = call(app, "GET", &format!("/v1/sources/{slug}/versions"), None, None).await;
    let active = list
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["is_active"] == true)
        .unwrap();
    assert_eq!(active["revision"], 1);
}

#[tokio::test]
async fn promote_rejects_already_active_revision() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin(app.clone(), "aaron", &kp).await;
    let (slug, _, _) = seed_source_with_versions(&h.pool, "promote-noop", 2).await;

    let (status, body) = call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/versions/2/promote"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn promote_404_on_unknown_revision() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin(app.clone(), "aaron", &kp).await;
    let (slug, _, _) = seed_source_with_versions(&h.pool, "promote-missing", 1).await;

    let (status, body) = call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/versions/999/promote"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn promote_404_on_unknown_slug() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin(app.clone(), "aaron", &kp).await;

    let (status, body) = call(
        app,
        "POST",
        "/v1/admin/sources/no-such-source/versions/1/promote",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn promote_requires_admin_auth() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, body) =
        call(app, "POST", "/v1/admin/sources/whatever/versions/1/promote", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

// ===== Admin retire-version =====

#[tokio::test]
async fn retire_version_happy_path() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin(app.clone(), "aaron", &kp).await;
    let (slug, _, _) = seed_source_with_versions(&h.pool, "retire-ok", 3).await;

    let (status, body) = call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/versions/1/retire"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], 1);
    assert_eq!(body["status"], "retired");
    assert!(!body["retired_at"].is_null(), "retired_at must be set: {body}");
}

#[tokio::test]
async fn retire_version_refuses_active_revision() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin(app.clone(), "aaron", &kp).await;
    let (slug, _, _) = seed_source_with_versions(&h.pool, "retire-active", 1).await;

    let (status, body) = call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/versions/1/retire"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("active version"));
}

#[tokio::test]
async fn retire_version_404_on_unknown_revision() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin(app.clone(), "aaron", &kp).await;
    let (slug, _, _) = seed_source_with_versions(&h.pool, "retire-missing", 1).await;

    let (status, body) = call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/versions/999/retire"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn retire_version_requires_admin_auth() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, _) =
        call(app, "POST", "/v1/admin/sources/whatever/versions/1/retire", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

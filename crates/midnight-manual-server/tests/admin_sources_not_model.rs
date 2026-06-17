//! Integration: `GET /v1/admin/sources?not_model=<wire>` returns sources whose
//! active version is NOT on the given model.
//!
//! This test is compile-checked here; it exercises the full app stack
//! (auth + route + DB) and requires the `integration` feature + a live
//! Postgres+pgvector instance (supplied by CI or testcontainers).

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
use serde_json::Value;
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
        Some(serde_json::json!({"user_id": user_id})),
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
        Some(serde_json::json!({"challenge_id": challenge_id, "signature_b64": signature_b64})),
    )
    .await;
    body["token"].as_str().unwrap().to_owned()
}

fn unique_slug(prefix: &str) -> String {
    let id = Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &id[..16])
}

/// Seed a source with an active `source_version` on `model_id`. Returns the
/// source slug.
async fn seed_source_on_model(pool: &sqlx::PgPool, slug: &str, model_id: Uuid) {
    let source_id = source::insert(pool, slug, slug, SourceKind::DocsSite, None, 5)
        .await
        .expect("insert source");
    let (sv_id, _) =
        source_version::create_building(pool, source_id, model_id, None, "0.1.0", "hash")
            .await
            .expect("create source_version");
    source_version::finalize(pool, sv_id)
        .await
        .expect("finalize source_version");
}

#[tokio::test]
async fn not_model_returns_sources_not_on_target() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    // Register two models: the target (voyage-code-3@1) and an old one.
    let target_id = embedding_model::upsert(&h.pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .expect("upsert target model");
    let old_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .expect("upsert old model");

    // Two sources on the old model, one on the target model.
    let slug_old1 = unique_slug("nm-old1");
    let slug_old2 = unique_slug("nm-old2");
    let slug_new = unique_slug("nm-new");
    seed_source_on_model(&h.pool, &slug_old1, old_id).await;
    seed_source_on_model(&h.pool, &slug_old2, old_id).await;
    seed_source_on_model(&h.pool, &slug_new, target_id).await;

    // Query for sources NOT on voyage-code-3@1.
    let (status, body) = call(
        app.clone(),
        "GET",
        "/v1/admin/sources?not_model=voyage-code-3@1",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let sources = body["sources"].as_array().expect("sources array");
    let slugs: Vec<&str> = sources.iter().filter_map(|s| s["slug"].as_str()).collect();

    // Both old-model sources must appear; the target-model source must not.
    assert!(slugs.contains(&slug_old1.as_str()), "old1 must appear: {body}");
    assert!(slugs.contains(&slug_old2.as_str()), "old2 must appear: {body}");
    assert!(!slugs.contains(&slug_new.as_str()), "on-target source must NOT appear: {body}");
}

#[tokio::test]
async fn not_model_unauthenticated_returns_401() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, _) =
        call(app, "GET", "/v1/admin/sources?not_model=voyage-code-3@1", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn not_model_writer_role_returns_403() {
    let h = common::boot().await;
    let admin_kp = Keypair::generate();
    let writer_kp = Keypair::generate();
    let cfg = cfg_with_auth(
        writer_user_store("admin-nm", &admin_kp, "writer-nm", &writer_kp),
        vec![7u8; 32],
    );
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "writer-nm", &writer_kp).await;

    let (status, body) =
        call(app, "GET", "/v1/admin/sources?not_model=voyage-code-3@1", Some(&token), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "forbidden");
}

#[tokio::test]
async fn not_model_invalid_wire_id_returns_400() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    let (status, body) =
        call(app, "GET", "/v1/admin/sources?not_model=notavalidwireid", Some(&token), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn not_model_unknown_model_returns_404() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    // This model is valid wire-format but not registered in the DB.
    let (status, body) =
        call(app, "GET", "/v1/admin/sources?not_model=ghost-model@99", Some(&token), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");
}

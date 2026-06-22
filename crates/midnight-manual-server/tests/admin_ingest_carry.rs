//! DB route tests for the carry-forward gate introduced in Task 3.
//!
//! Verifies that the server correctly rejects a `carried: true` document when
//! there is no prior active version to carry from (no-prior-match path inside
//! `classify_upload`).

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
use mnm_store::entities::{embedding_model, source};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

// ── Shared helpers (mirrors admin_ingest_endpoints.rs) ──────────────────────

fn cfg_with_auth(user_store_body: String, jwt_secret_bytes: Vec<u8>) -> ServerConfig {
    ServerConfig {
        user_store_body: Some(user_store_body),
        jwt_secret: Some(jwt_secret_bytes),
        ..Default::default()
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
    let bytes = to_bytes(resp.into_body(), 8 * 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn mint_admin_token(app: axum::Router, user_id: &str, kp: &Keypair) -> String {
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

/// Seed a source with the voyage-context-3@1 model registered but NO active
/// source_version. Returns `(slug, source_id)`.
async fn seed_source_no_version(pool: &sqlx::PgPool) -> (String, Uuid) {
    embedding_model::upsert(pool, "voyage-context-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let slug = format!("carry-gate-test-{}", Uuid::new_v4());
    let id = source::insert(pool, &slug, "Carry Gate Test", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    (slug, id)
}

/// Start a new ingest run against `slug` using voyage-context-3@1. Returns the
/// `ingest_run_id` string.
async fn start_run(app: &axum::Router, slug: &str, token: &str) -> String {
    let (status, body) = call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(token),
        Some(json!({
            "ingest_cli_version": "0.1.0-test",
            "embedding_model": "voyage-context-3@1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start_run: {body}");
    body["ingest_run_id"].as_str().unwrap().to_owned()
}

/// Upload `body` to the given `run_id` and return `(status, parsed JSON)`.
async fn upload(
    app: &axum::Router,
    slug: &str,
    run_id: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(token),
        Some(body),
    )
    .await
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// A document sent with `carried: true` but no prior active version for the
/// source must appear in `conflicts`, not in `accepted`.
#[tokio::test]
async fn carried_doc_without_prior_match_conflicts_not_inserts() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source_no_version(&h.pool).await;

    let run_id = start_run(&app, &slug, &token).await;

    // Upload a doc flagged `carried: true` but there is no prior active version
    // to carry from → must be a conflict, not accepted.
    let (status, body) = upload(
        &app,
        &slug,
        &run_id,
        &token,
        json!({
            "documents": [{
                "path": "a.md",
                "kind": "markdown",
                "content_hash": "h",
                "carried": true,
                "provenance": {},
                "chunks": []
            }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload response: {body}");
    assert_eq!(body["accepted"], 0, "no doc should be accepted: {body}");
    assert_eq!(
        body["conflicts"].as_array().unwrap().len(),
        1,
        "exactly one conflict expected: {body}"
    );
}

/// A new document (no `carried` flag) with chunks is accepted normally even
/// when there is no prior active version.
#[tokio::test]
async fn new_doc_with_chunks_inserts_when_no_prior_version() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source_no_version(&h.pool).await;

    let run_id = start_run(&app, &slug, &token).await;

    let (status, body) = upload(
        &app,
        &slug,
        &run_id,
        &token,
        json!({
            "documents": [{
                "path": "b.md",
                "kind": "markdown",
                "content_hash": "hb",
                "provenance": {},
                "chunks": [{
                    "chunk_index": 0,
                    "total_chunks": 1,
                    "content": "hello world",
                    "content_hash": "cb"
                }]
            }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload response: {body}");
    assert_eq!(body["accepted"], 1, "doc should be accepted: {body}");
    assert_eq!(body["conflicts"].as_array().unwrap().len(), 0, "no conflicts expected: {body}");
}

/// A new document without chunks is rejected even when there is no prior
/// active version (zero-chunk guard).
#[tokio::test]
async fn new_doc_without_chunks_conflicts() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source_no_version(&h.pool).await;

    let run_id = start_run(&app, &slug, &token).await;

    let (status, body) = upload(
        &app,
        &slug,
        &run_id,
        &token,
        json!({
            "documents": [{
                "path": "empty.md",
                "kind": "markdown",
                "content_hash": "he",
                "provenance": {},
                "chunks": []
            }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload response: {body}");
    assert_eq!(body["accepted"], 0, "doc without chunks must not be accepted: {body}");
    assert_eq!(
        body["conflicts"].as_array().unwrap().len(),
        1,
        "exactly one conflict expected: {body}"
    );
}

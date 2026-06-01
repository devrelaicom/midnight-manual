//! End-to-end exercises for the Phase-9b admin ingest write protocol.
//!
//! Walks a fresh source through the four-endpoint lifecycle:
//! `POST /ingest-runs` → `PUT .../documents` → `POST .../finalize`,
//! plus the abort path and auth gating. Carry-forward is exercised by
//! running two ingests against the same source slug and asserting the
//! second run re-uses the first run's chunks for an unchanged document.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use mn_auth::Keypair;
use mn_core::types::SourceKind;
use mn_server::{app, config::ServerConfig};
use mn_store::entities::{embedding_model, source};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

fn cfg_with_auth(user_store_body: String, jwt_secret_bytes: Vec<u8>) -> ServerConfig {
    ServerConfig {
        corpus_model: Some("bge-base-en-v1.5@1".to_owned()),
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

async fn json_call(
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
    let (_, body) = json_call(
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
    let (_, body) = json_call(
        app,
        "POST",
        "/v1/auth/verify",
        None,
        Some(json!({"challenge_id": challenge_id, "signature_b64": signature_b64})),
    )
    .await;
    body["token"].as_str().unwrap().to_owned()
}

async fn seed_source(pool: &sqlx::PgPool) -> (String, Uuid) {
    embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("ingest-test-{}", Uuid::new_v4());
    let id = source::insert(pool, &slug, "Ingest Test", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    (slug, id)
}

fn sample_document_payload(path: &str, content: &str) -> Value {
    // The handler trusts the client-supplied content_hash; tests use the
    // body itself as the hash key so distinct content → distinct hash.
    let content_hash = format!("h:{path}:{}", hash_of(content));
    let chunk_hash = format!("c:{path}:{}", hash_of(content));
    json!({
        "path": path,
        "kind": "markdown",
        "content_hash": content_hash,
        "char_count": content.len(),
        "token_count": 0,
        "provenance": {},
        "chunks": [{
            "chunk_index": 0,
            "total_chunks": 1,
            "content": content,
            "content_hash": chunk_hash,
            "heading_path": [],
            "symbol_path": [],
            "start_byte": 0,
            "end_byte": content.len(),
            "token_count": 0,
        }],
    })
}

fn hash_of(s: &str) -> String {
    // Trivial fold — only the deterministic property matters, not the
    // collision resistance, because the server stores whatever we send.
    let mut acc: u64 = 1_469_598_103_934_665_603;
    for b in s.bytes() {
        acc = acc
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(u64::from(b));
    }
    format!("{acc:016x}")
}

#[tokio::test]
async fn happy_path_full_lifecycle() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;

    // 1. Start.
    let (status, body) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(&token),
        Some(json!({
            "ingest_cli_version": "0.1.0-test",
            "embedding_model": "bge-base-en-v1.5@1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start: {body}");
    let run_id = body["ingest_run_id"].as_str().unwrap().to_owned();
    assert_eq!(body["source_version_revision"], 1);

    // 2. Upload.
    let (status, body) = json_call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(&token),
        Some(json!({
            "documents": [
                sample_document_payload("intro.md", "# Welcome\n\nIntro body."),
                sample_document_payload("guide.md", "# Guide\n\nGuide body."),
            ],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload: {body}");
    assert_eq!(body["accepted"], 2);
    assert_eq!(body["carried"], 0);

    // 3. Finalize.
    let (status, body) = json_call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/finalize"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "finalize: {body}");
    assert_eq!(body["is_active"], true);
    assert_eq!(body["revision"], 1);
    assert_eq!(body["demoted_revision"], Value::Null);
}

#[tokio::test]
async fn carry_forward_reuses_unchanged_documents() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, source_id) = seed_source(&h.pool).await;

    // First ingest: 2 docs.
    let intro_body = "# Stable\n\nNever changes.";
    let guide_body = "# Guide\n\nFirst version.";

    let run_id_1 = start_and_finalize(
        &app,
        &slug,
        &token,
        &[
            sample_document_payload("intro.md", intro_body),
            sample_document_payload("guide.md", guide_body),
        ],
    )
    .await;
    assert_eq!(run_id_1.revision, 1);

    // Second ingest: same intro, changed guide.
    let (run_id_2_body, run_id_2_id) = start_run(&app, &slug, &token).await;
    let (status, upload) = json_call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id_2_id}/documents"),
        Some(&token),
        Some(json!({
            "documents": [
                sample_document_payload("intro.md", intro_body),
                sample_document_payload("guide.md", "# Guide\n\nSecond version with changes."),
            ],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload2: {upload}");
    assert_eq!(upload["accepted"], 2);
    assert_eq!(upload["carried"], 1, "intro.md should have been carried");

    let (status, fin) = json_call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id_2_id}/finalize"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "finalize2: {fin}");
    assert_eq!(fin["revision"], 2);
    assert_eq!(fin["demoted_revision"], 1);

    // The new SV must have 2 documents — same as the first.
    let new_sv_docs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM document \
         WHERE source_version_id = (SELECT id FROM source_version WHERE source_id = $1 AND revision = 2)",
    )
    .bind(source_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(new_sv_docs, 2);

    // The carried intro.md in the new SV must have the same content_hash AND
    // its chunks must have inherited the embedding from rev 1 (NULL in our
    // case because we never embedded — but the row must exist).
    let new_intro_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM document \
         WHERE source_path = 'intro.md' AND source_version_id = \
           (SELECT id FROM source_version WHERE source_id = $1 AND revision = 2)",
    )
    .bind(source_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    let chunk_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chunk WHERE document_id = $1")
        .bind(new_intro_id)
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(chunk_count, 1, "carried doc must have its chunks copied");

    drop(run_id_2_body);
}

struct FinalizeResultRow {
    revision: i64,
}

async fn start_and_finalize(
    app: &axum::Router,
    slug: &str,
    token: &str,
    docs: &[Value],
) -> FinalizeResultRow {
    let (_body, run_id) = start_run(app, slug, token).await;
    let (status, upload) = json_call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(token),
        Some(json!({"documents": docs})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload: {upload}");
    let (status, fin) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/finalize"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "finalize: {fin}");
    FinalizeResultRow {
        revision: fin["revision"].as_i64().unwrap(),
    }
}

async fn start_run(app: &axum::Router, slug: &str, token: &str) -> (Value, String) {
    let (status, body) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(token),
        Some(json!({
            "ingest_cli_version": "0.1.0-test",
            "embedding_model": "bge-base-en-v1.5@1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start: {body}");
    let run_id = body["ingest_run_id"].as_str().unwrap().to_owned();
    (body, run_id)
}

#[tokio::test]
async fn abort_blocks_subsequent_uploads_with_run_aborted() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;
    let (_, run_id) = start_run(&app, &slug, &token).await;

    // Abort.
    let (status, _) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/abort"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Subsequent upload must 409 with run_aborted.
    let (status, body) = json_call(
        app,
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(&token),
        Some(json!({"documents": [sample_document_payload("a.md", "# A")]})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "run_aborted");
}

#[tokio::test]
async fn unauthenticated_request_401s() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let (slug, _) = seed_source(&h.pool).await;

    let (status, body) = json_call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        None,
        Some(json!({
            "ingest_cli_version": "0.1.0",
            "embedding_model": "bge-base-en-v1.5@1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn unknown_source_404s() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;

    let (status, body) = json_call(
        app,
        "POST",
        "/v1/admin/sources/does-not-exist/ingest-runs",
        Some(&token),
        Some(json!({
            "ingest_cli_version": "0.1.0",
            "embedding_model": "bge-base-en-v1.5@1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn invalid_embedding_model_409s() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;

    let (status, body) = json_call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(&token),
        Some(json!({
            "ingest_cli_version": "0.1.0",
            "embedding_model": "made-up@9",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "embedding_model_mismatch");
}

#[tokio::test]
async fn duplicate_path_in_batch_records_conflict() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;
    let (_, run_id) = start_run(&app, &slug, &token).await;

    let (status, body) = json_call(
        app,
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(&token),
        Some(json!({
            "documents": [
                sample_document_payload("x.md", "# A"),
                sample_document_payload("x.md", "# B"),
            ],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], 1);
    assert_eq!(body["conflicts"][0]["path"], "x.md");
}

#[tokio::test]
async fn chunk_upload_persists_structured_symbol_path() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, source_id) = seed_source(&h.pool).await;

    // Build a code document payload with a non-empty structured symbol_path.
    let content = "pub struct Widget { pub x: i32 }";
    let content_hash = format!("h:widget.rs:{}", hash_of(content));
    let chunk_hash = format!("c:widget.rs:{}", hash_of(content));
    let doc_payload = json!({
        "path": "widget.rs",
        "kind": "code",
        "language": "rust",
        "content_hash": content_hash,
        "char_count": content.len(),
        "token_count": 0,
        "provenance": {},
        "chunks": [{
            "chunk_index": 0,
            "total_chunks": 1,
            "content": content,
            "content_hash": chunk_hash,
            "heading_path": [],
            "symbol_path": [{"kind": "class", "name": "Widget"}],
            "start_byte": 0,
            "end_byte": content.len(),
            "token_count": 0,
        }],
    });

    // 1. Start run.
    let (status, body) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(&token),
        Some(json!({
            "ingest_cli_version": "0.1.0-test",
            "embedding_model": "bge-base-en-v1.5@1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start: {body}");
    let run_id = body["ingest_run_id"].as_str().unwrap().to_owned();

    // 2. Upload the code document.
    let (status, body) = json_call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(&token),
        Some(json!({"documents": [doc_payload]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload: {body}");
    assert_eq!(body["accepted"], 1);

    // 3. Finalize.
    let (status, body) = json_call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/finalize"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "finalize: {body}");

    // 4. Read the persisted chunk back and assert the structured symbol_path survived.
    let chunk_id: Uuid = sqlx::query_scalar(
        "SELECT c.id FROM chunk c JOIN document d ON d.id = c.document_id \
         WHERE d.source_path = $1 AND d.source_version_id = \
           (SELECT id FROM source_version WHERE source_id = $2 AND revision = 1)",
    )
    .bind("widget.rs")
    .bind(source_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();

    let segs = mn_store::entities::chunk::symbol_path_of(&h.pool, chunk_id)
        .await
        .unwrap();
    assert_eq!(segs.len(), 1, "expected exactly one symbol segment");
    assert_eq!(segs[0].kind, "class");
    assert_eq!(segs[0].name, "Widget");
}

#[tokio::test]
async fn document_package_membership_persists() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, source_id) = seed_source(&h.pool).await;

    // Build a code document payload that carries package membership.
    let content = "pub fn init() {}";
    let content_hash = format!("h:lib.rs:{}", hash_of(content));
    let chunk_hash = format!("c:lib.rs:{}", hash_of(content));
    let doc_payload = json!({
        "path": "lib.rs",
        "kind": "code",
        "language": "rust",
        "content_hash": content_hash,
        "char_count": content.len(),
        "token_count": 0,
        "provenance": {},
        "package": {
            "kind": "rust",
            "name": "midnight-foo",
            "manifest_path": "Cargo.toml"
        },
        "chunks": [{
            "chunk_index": 0,
            "total_chunks": 1,
            "content": content,
            "content_hash": chunk_hash,
            "heading_path": [],
            "symbol_path": [],
            "start_byte": 0,
            "end_byte": content.len(),
            "token_count": 0,
        }],
    });

    // 1. Start run.
    let (status, body) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(&token),
        Some(json!({
            "ingest_cli_version": "0.1.0-test",
            "embedding_model": "bge-base-en-v1.5@1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start: {body}");
    let run_id = body["ingest_run_id"].as_str().unwrap().to_owned();
    let sv_id: Uuid = run_id.parse().unwrap();

    // 2. Upload.
    let (status, body) = json_call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(&token),
        Some(json!({"documents": [doc_payload]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload: {body}");
    assert_eq!(body["accepted"], 1);

    // 3. Finalize.
    let (status, body) = json_call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/finalize"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "finalize: {body}");

    // 4. Assert a package row exists for this source_version with name "midnight-foo".
    let pkg_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM package WHERE source_version_id = $1 AND name = 'midnight-foo'",
    )
    .bind(sv_id)
    .fetch_optional(&h.pool)
    .await
    .unwrap();
    assert!(pkg_id.is_some(), "expected a package row for midnight-foo");

    // 5. Assert the document's package_id points to that package row.
    let doc_package_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT d.package_id FROM document d \
         WHERE d.source_path = 'lib.rs' AND d.source_version_id = \
           (SELECT id FROM source_version WHERE source_id = $1 AND revision = 1)",
    )
    .bind(source_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(
        doc_package_id, pkg_id,
        "document.package_id must point to the midnight-foo package row"
    );
}

#[tokio::test]
async fn cross_source_run_id_404s() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug_a, _) = seed_source(&h.pool).await;
    let (slug_b, _) = seed_source(&h.pool).await;
    let (_, run_id) = start_run(&app, &slug_a, &token).await;

    // Use slug_b with slug_a's run_id.
    let (status, body) = json_call(
        app,
        "PUT",
        &format!("/v1/admin/sources/{slug_b}/ingest-runs/{run_id}/documents"),
        Some(&token),
        Some(json!({"documents": [sample_document_payload("a.md", "# A")]})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

fn embedded_document_payload(path: &str, content: &str, dim: usize) -> Value {
    let content_hash = format!("h:{path}:{}", hash_of(content));
    let chunk_hash = format!("c:{path}:{}", hash_of(content));
    // Values are irrelevant to the assertions (the server stores them verbatim
    // and only checks length); a constant vector keeps this cast-free.
    let embedding: Vec<f32> = vec![0.001_f32; dim];
    json!({
        "path": path, "kind": "markdown", "content_hash": content_hash,
        "char_count": content.len(), "token_count": 0, "provenance": {},
        "chunks": [{
            "chunk_index": 0, "total_chunks": 1, "content": content,
            "content_hash": chunk_hash, "heading_path": [], "symbol_path": [],
            "start_byte": 0, "end_byte": content.len(), "token_count": 0,
            "embedding": embedding,
        }],
    })
}

async fn start_run_with_model(app: axum::Router, slug: &str, token: &str) -> String {
    let (status, body) = json_call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(token),
        Some(json!({"ingest_cli_version": "test", "embedding_model": "bge-base-en-v1.5@1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start run: {body}");
    body["ingest_run_id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn embedded_upload_stores_ready_chunk() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;
    let run = start_run_with_model(app.clone(), &slug, &token).await;

    let (status, body) = json_call(
        app,
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run}/documents"),
        Some(&token),
        Some(json!({
            "embedding_model": "bge-base-en-v1.5@1",
            "documents": [embedded_document_payload("a.md", "hello world", 768)],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload: {body}");

    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM chunk WHERE source_version_id = $1")
            .bind(Uuid::parse_str(&run).unwrap())
            .fetch_all(&h.pool)
            .await
            .unwrap();
    assert_eq!(statuses, vec!["ready".to_string()], "chunk should be ready, got {statuses:?}");
}

#[tokio::test]
async fn embedded_upload_without_model_is_409() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;
    let run = start_run_with_model(app.clone(), &slug, &token).await;

    let (status, _body) = json_call(
        app,
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run}/documents"),
        Some(&token),
        Some(json!({ "documents": [embedded_document_payload("a.md", "x", 768)] })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn embedded_upload_wrong_model_is_409() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;
    let run = start_run_with_model(app.clone(), &slug, &token).await;

    let (status, _body) = json_call(
        app,
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run}/documents"),
        Some(&token),
        Some(json!({
            "embedding_model": "bge-base-en-v1.5@2",
            "documents": [embedded_document_payload("a.md", "x", 768)],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn embedded_upload_wrong_dim_is_400() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;
    let run = start_run_with_model(app.clone(), &slug, &token).await;

    let (status, _body) = json_call(
        app,
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run}/documents"),
        Some(&token),
        Some(json!({
            "embedding_model": "bge-base-en-v1.5@1",
            "documents": [embedded_document_payload("a.md", "x", 3)],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

//! Integration test: `finalize_run` re-resolves the corpus model in-process.
//!
//! Drives the full three-step ingest lifecycle (start → upload → finalize)
//! against a live ephemeral Postgres, then reads the `Shared` Arc that was
//! injected into the app and asserts the corpus model wire id has been updated
//! to match the finalized run's embedding model. Proves the Task 3.4 one-liner
//! in `finalize_run` exercises the real handler path.
#![cfg(feature = "integration")]
#![allow(missing_docs, clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use std::sync::{Arc, RwLock};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use mn_auth::Keypair;
use mn_core::types::SourceKind;
use mn_server::{app, config::ServerConfig, corpus_model::CorpusModel, corpus_model::Shared};
use mn_store::entities::{embedding_model, source};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers mirrored from admin_ingest_endpoints.rs
// ---------------------------------------------------------------------------

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

fn hash_of(s: &str) -> String {
    let mut acc: u64 = 1_469_598_103_934_665_603;
    for b in s.bytes() {
        acc = acc
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(u64::from(b));
    }
    format!("{acc:016x}")
}

fn sample_document_payload(path: &str, content: &str) -> Value {
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

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// Prove that hitting the finalize endpoint re-resolves the corpus model
/// Shared so the in-process wire id reflects the finalized run's model.
///
/// Setup:
///  1. Register the target model (voyage-code-3@1, 1024-dim) — already seeded
///     by migration 0008, but `seed_source` also seeds bge-base-en-v1.5@1 so
///     we need to ensure we use the voyage model for the run.
///  2. Construct a Shared pre-loaded with a FAKE "old@1" sentinel so the flip
///     is unambiguous even if refresh sets the same id it would get at boot.
///  3. Build the app via `build_with_limiter`, keeping a clone of the Arc.
///  4. Drive start → upload → finalize with voyage-code-3@1.
///  5. Assert 200 / is_active true, then read the Arc and assert the wire id
///     has been refreshed away from "old@1".
#[tokio::test]
async fn finalize_reresolves_corpus_model_shared() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);

    // Ensure both models are registered. Migration 0008 seeds voyage-code-3@1;
    // the run below uses it. bge-base-en-v1.5@1 is already seeded by migration
    // 0006, but upsert is idempotent so we call both defensively.
    embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    embedding_model::upsert(&h.pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();

    let slug = format!("reresolve-test-{}", Uuid::new_v4());
    let _src_id = source::insert(&h.pool, &slug, "Reresolve Test", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();

    // Inject a stale sentinel so any successful refresh is detectable.
    let old_model = CorpusModel {
        wire: "old@1".to_owned(),
        id: Uuid::nil(),
        dim: 1,
    };
    let corpus_model: Shared = Arc::new(RwLock::new(Some(old_model)));
    // Clone the Arc so we can observe changes after the request.
    let observed = Arc::clone(&corpus_model);

    let limiter = None; // rate limiting not needed for this test
    let app =
        app::build_with_limiter(h.pool.clone(), cfg, limiter, corpus_model).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;

    // 1. Start an ingest run on voyage-code-3@1.
    let (status, body) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(&token),
        Some(json!({
            "ingest_cli_version": "0.1.0-test",
            "embedding_model": "voyage-code-3@1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start: {body}");
    let run_id = body["ingest_run_id"].as_str().unwrap().to_owned();

    // 2. Upload one document (no pre-embedded vectors; server-side embedding).
    let (status, body) = json_call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(&token),
        Some(json!({
            "documents": [sample_document_payload("hello.md", "# Hello\n\nWorld.")],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload: {body}");

    // 3. Finalize — this is the call that must trigger corpus_model::refresh.
    let (status, body) = json_call(
        app,
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/finalize"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "finalize: {body}");
    assert_eq!(body["is_active"], true, "finalize must report is_active true");

    // 4. Observe the Shared: the handler must have swapped "old@1" for the
    //    real voyage-code-3@1 wire id that corpus_model::resolve returns.
    let wire_after = observed
        .read()
        .expect("lock not poisoned")
        .as_ref()
        .expect("corpus_model must be Some after finalize")
        .wire
        .clone();

    assert_ne!(
        wire_after, "old@1",
        "corpus_model.wire must have been refreshed from 'old@1' but was still '{wire_after}'"
    );
    assert_eq!(
        wire_after, "voyage-code-3@1",
        "corpus_model.wire must equal 'voyage-code-3@1' after finalize"
    );
}

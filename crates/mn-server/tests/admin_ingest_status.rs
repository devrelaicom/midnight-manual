//! End-to-end exercises for `GET /v1/admin/ingest/status` (Phase 11c).

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use mn_auth::Keypair;
use mn_core::provenance::Provenance;
use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mn_server::{app, config::ServerConfig};
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

fn cfg_with_auth(user_store_body: String, jwt_secret_bytes: Vec<u8>) -> ServerConfig {
    ServerConfig {
        user_store_body: Some(user_store_body),
        jwt_secret: Some(jwt_secret_bytes),
        embedder_enabled: true,
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
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
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

/// Seed one source with one active source_version containing 2 ready + 1
/// embed_failed chunk. Returns the source slug.
async fn seed_active_source_with_chunks(pool: &sqlx::PgPool) -> String {
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("status-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Status Test", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, "0.1.0", "h")
        .await
        .unwrap();
    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();

    for (i, status) in [
        ChunkStatus::Ready,
        ChunkStatus::Ready,
        ChunkStatus::EmbedFailed,
    ]
    .iter()
    .enumerate()
    {
        let doc_node = node::insert(
            pool,
            sv_id,
            Some(root),
            NodeKind::Document,
            &format!("doc-{i}.md"),
            i32::try_from(i).unwrap(),
        )
        .await
        .unwrap();
        let doc_id = document::insert(
            pool,
            document::NewDocument {
                source_version_id: sv_id,
                node_id: doc_node,
                kind: DocumentKind::Markdown,
                source_url: None,
                published_url: None,
                source_path: &format!("doc-{i}.md"),
                language: Some("en"),
                content_hash: &format!("h{i}"),
                source_modified_at: None,
                frontmatter: None,
                provenance: &Provenance::default(),
                package_id: None,
                char_count: 1,
                token_count: 1,
            },
        )
        .await
        .unwrap();
        let chunk_node =
            node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, &format!("c{i}"), 0)
                .await
                .unwrap();
        // Ready chunks need a valid 768-dim embedding.
        let embedding = if matches!(status, ChunkStatus::Ready) {
            Some(vec![0.1_f32; 768])
        } else {
            None
        };
        chunk::insert(
            pool,
            chunk::NewChunk {
                source_version_id: sv_id,
                document_id: doc_id,
                node_id: chunk_node,
                chunk_index: 0,
                total_chunks: 1,
                content: &format!("chunk {i}"),
                content_hash: &format!("c{i}"),
                embedding,
                embedding_model_id: model_id,
                heading_path: &[],
                symbol_path: &[],
                start_byte: 0,
                end_byte: 1,
                token_count: 1,
                status: *status,
            },
        )
        .await
        .unwrap();
    }

    // Finalize so the SV becomes active.
    source_version::finalize(pool, sv_id).await.unwrap();
    slug
}

#[tokio::test]
async fn unauthenticated_returns_401() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, body) = call(app, "GET", "/v1/admin/ingest/status", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn admin_token_returns_status_with_source_summary() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let slug = seed_active_source_with_chunks(&h.pool).await;

    let (status, body) = call(app, "GET", "/v1/admin/ingest/status", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["active_embedding_model"], "bge-base-en-v1.5@1");
    assert_eq!(body["embedder_worker_enabled"], true);

    let our_row = body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["slug"] == slug)
        .expect("source must appear in response");
    assert_eq!(our_row["active_revision"], 1);
    assert_eq!(our_row["total_chunks"], 3);
    assert_eq!(our_row["ready_chunks"], 2);
    assert_eq!(our_row["embed_failed_chunks"], 1);
}

#[tokio::test]
async fn source_with_no_active_version_returns_zeros() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;

    // Register a source but no source_version.
    let _ = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("status-empty-{}", Uuid::new_v4());
    source::insert(&h.pool, &slug, "Empty", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();

    let (status, body) = call(app, "GET", "/v1/admin/ingest/status", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let our_row = body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["slug"] == slug)
        .expect("source must appear in response");
    assert_eq!(our_row["active_revision"], Value::Null);
    assert_eq!(our_row["total_chunks"], 0);
    assert_eq!(our_row["ready_chunks"], 0);
    assert_eq!(our_row["embed_failed_chunks"], 0);
}

#[tokio::test]
async fn embedder_worker_enabled_reflects_config() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let mut cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    cfg.embedder_enabled = false;
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;

    let (status, body) = call(app, "GET", "/v1/admin/ingest/status", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["embedder_worker_enabled"], false);
}

#[tokio::test]
async fn sources_are_sorted_by_slug() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;

    let (status, body) = call(app, "GET", "/v1/admin/ingest/status", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let slugs: Vec<String> = body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].as_str().unwrap().to_owned())
        .collect();
    let mut sorted = slugs.clone();
    sorted.sort();
    assert_eq!(slugs, sorted, "sources MUST be slug-sorted for stable diff");
}

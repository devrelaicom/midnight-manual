//! End-to-end exercises for `GET /v1/admin/sources/:slug/active-version/documents`
//! (Task 2: prior-state inventory endpoint).

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use midnight_manual_server::{app, config::ServerConfig};
use mnm_auth::Keypair;
use mnm_core::provenance::Provenance;
use mnm_core::types::{DocumentKind, NodeKind, SourceKind};
use mnm_store::entities::{document, embedding_model, node, source, source_version};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

// ===== Helpers =====

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
    use serde_json::json;
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

/// Seed a source with an active version carrying the given document paths.
/// Returns `(slug, source_version_id)`.
async fn seed_active_version(pool: &PgPool, prefix: &str, paths: &[&str]) -> (String, Uuid) {
    let model_id = embedding_model::upsert(pool, "voyage-context-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let slug = format!("{prefix}-{}", Uuid::new_v4().simple());
    let source_id = source::insert(pool, &slug, "Inventory Fixture", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) =
        source_version::create_building(pool, source_id, model_id, None, "0.1.0", "inv-hash")
            .await
            .unwrap();

    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let provenance = Provenance::default();

    for (i, path) in paths.iter().enumerate() {
        let doc_node = node::insert(
            pool,
            sv_id,
            Some(root),
            NodeKind::Document,
            path,
            i32::try_from(i).unwrap(),
        )
        .await
        .unwrap();
        document::insert(
            pool,
            document::NewDocument {
                source_version_id: sv_id,
                node_id: doc_node,
                kind: DocumentKind::Markdown,
                source_url: None,
                published_url: None,
                source_path: path,
                language: None,
                content_hash: &format!("hash-{i}"),
                source_modified_at: None,
                frontmatter: None,
                provenance: &provenance,
                package_id: None,
                char_count: 0,
                token_count: 0,
            },
        )
        .await
        .unwrap();
    }

    source_version::finalize(pool, sv_id).await.unwrap();
    (slug, sv_id)
}

/// Seed a source with no source_version at all.
async fn seed_source_no_version(pool: &PgPool, prefix: &str) -> String {
    let slug = format!("{prefix}-{}", Uuid::new_v4().simple());
    source::insert(pool, &slug, "Empty Source", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    slug
}

// ===== Tests =====

#[tokio::test]
async fn inventory_endpoint_returns_active_docs_and_model() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin(app.clone(), "aaron", &kp).await;

    let (slug, _) = seed_active_version(&h.pool, "inv-ok", &["a.md", "b.md"]).await;

    let (status, body) = call(
        app,
        "GET",
        &format!("/v1/admin/sources/{slug}/active-version/documents"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["documents"].as_array().unwrap().len(), 2, "{body}");
    assert!(
        body["embedding_model"].as_str().unwrap().contains('@'),
        "embedding_model must be in wire format `name@revision`: {body}"
    );
}

#[tokio::test]
async fn inventory_endpoint_404_without_active_version() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin(app.clone(), "aaron", &kp).await;

    let slug = seed_source_no_version(&h.pool, "inv-empty").await;

    let (status, body) = call(
        app,
        "GET",
        &format!("/v1/admin/sources/{slug}/active-version/documents"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn inventory_endpoint_requires_admin() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    // No bearer token supplied.
    let (status, body) =
        call(app, "GET", "/v1/admin/sources/whatever/active-version/documents", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

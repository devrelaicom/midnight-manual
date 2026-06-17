//! End-to-end exercises for `/v1/admin/sources` CRUD (Phase 12).

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
use mnm_store::entities::source;
use serde_json::{json, Value};
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

fn unique_slug(prefix: &str) -> String {
    let id = Uuid::new_v4().simple().to_string();
    // Truncate UUID portion so the result fits the 63-char slug ceiling
    // (`prefix-` + 32 hex chars is already 40+).
    format!("{prefix}-{}", &id[..16])
}

#[tokio::test]
async fn create_source_happy_path() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    let slug = unique_slug("crud-create");
    let (status, body) = call(
        app.clone(),
        "POST",
        "/v1/admin/sources",
        Some(&token),
        Some(json!({
            "slug": slug,
            "display_name": "Create Happy",
            "kind": "docs_site",
            "origin_url": "https://example.com/docs",
            "retention_count": 7
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["slug"], slug);
    assert_eq!(body["display_name"], "Create Happy");
    assert_eq!(body["kind"], "docs_site");
    assert_eq!(body["origin_url"], "https://example.com/docs");
    assert_eq!(body["retention_count"], 7);

    let (s2, b2) = call(app, "GET", &format!("/v1/sources/{slug}"), None, None).await;
    assert_eq!(s2, StatusCode::OK, "{b2}");
    assert_eq!(b2["slug"], slug);
}

#[tokio::test]
async fn create_source_defaults_retention_to_five_and_display_to_slug() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    let slug = unique_slug("crud-defaults");
    let (status, body) = call(
        app,
        "POST",
        "/v1/admin/sources",
        Some(&token),
        Some(json!({ "slug": slug, "kind": "code_repo" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["retention_count"], 5);
    assert_eq!(body["display_name"], slug);
}

#[tokio::test]
async fn create_source_rejects_invalid_slug() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    for bad in [
        "UPPER",
        "-leading",
        "trailing-",
        "",
        "with space",
        "under_score",
    ] {
        let (status, body) = call(
            app.clone(),
            "POST",
            "/v1/admin/sources",
            Some(&token),
            Some(json!({ "slug": bad, "kind": "docs_site" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "slug=`{bad}` body={body}");
        assert_eq!(body["error"]["code"], "invalid_request");
    }
}

#[tokio::test]
async fn create_source_rejects_oversize_slug() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    let slug = "a".repeat(64);
    let (status, _) = call(
        app,
        "POST",
        "/v1/admin/sources",
        Some(&token),
        Some(json!({ "slug": slug, "kind": "docs_site" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_source_duplicate_returns_400() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;
    let slug = unique_slug("crud-dup");

    let (s1, _) = call(
        app.clone(),
        "POST",
        "/v1/admin/sources",
        Some(&token),
        Some(json!({ "slug": slug, "kind": "docs_site" })),
    )
    .await;
    assert_eq!(s1, StatusCode::CREATED);

    let (s2, body) = call(
        app,
        "POST",
        "/v1/admin/sources",
        Some(&token),
        Some(json!({ "slug": slug, "kind": "docs_site" })),
    )
    .await;
    assert_eq!(s2, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("already registered"));
}

#[tokio::test]
async fn create_source_rejects_out_of_range_retention() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    for bad in [0, 51, -1] {
        let slug = unique_slug("crud-retr");
        let (status, body) = call(
            app.clone(),
            "POST",
            "/v1/admin/sources",
            Some(&token),
            Some(json!({ "slug": slug, "kind": "docs_site", "retention_count": bad })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "retention={bad} body={body}");
        assert_eq!(body["error"]["code"], "invalid_request");
    }
}

#[tokio::test]
async fn create_source_rejects_unknown_kind() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    let (status, _) = call(
        app,
        "POST",
        "/v1/admin/sources",
        Some(&token),
        Some(json!({ "slug": unique_slug("crud-kind"), "kind": "garbage" })),
    )
    .await;
    // axum's JSON extractor maps this to 400 (rejection); accept either 400 or 422.
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "got {status}"
    );
}

#[tokio::test]
async fn update_source_happy_path() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;
    let slug = unique_slug("crud-upd");
    source::insert(&h.pool, &slug, "Original", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();

    let (status, body) = call(
        app,
        "PATCH",
        &format!("/v1/admin/sources/{slug}"),
        Some(&token),
        Some(json!({ "display_name": "Renamed", "retention_count": 12 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["display_name"], "Renamed");
    assert_eq!(body["retention_count"], 12);
    assert_eq!(body["slug"], slug);
}

#[tokio::test]
async fn update_source_no_op_when_body_empty() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;
    let slug = unique_slug("crud-noop");
    source::insert(&h.pool, &slug, "Unchanged", SourceKind::Mixed, None, 9)
        .await
        .unwrap();

    let (status, body) = call(
        app,
        "PATCH",
        &format!("/v1/admin/sources/{slug}"),
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["display_name"], "Unchanged");
    assert_eq!(body["retention_count"], 9);
}

#[tokio::test]
async fn update_source_404_on_unknown_slug() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    let (status, body) = call(
        app,
        "PATCH",
        &format!("/v1/admin/sources/{}", unique_slug("crud-missing")),
        Some(&token),
        Some(json!({ "display_name": "Nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn update_source_400_on_invalid_retention_count() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;
    let slug = unique_slug("crud-bad-rc");
    source::insert(&h.pool, &slug, "Retain", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();

    let (status, body) = call(
        app,
        "PATCH",
        &format!("/v1/admin/sources/{slug}"),
        Some(&token),
        Some(json!({ "retention_count": 999 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn retire_source_happy_path() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;
    let slug = unique_slug("crud-retire");
    source::insert(&h.pool, &slug, "Will Retire", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();

    let (status, body) =
        call(app.clone(), "DELETE", &format!("/v1/admin/sources/{slug}"), Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body["retired_at"].is_null(), "retired_at must be set after DELETE: {body}");

    // The public list endpoint must filter it out…
    let (s_pub, pub_body) = call(app.clone(), "GET", "/v1/sources", None, None).await;
    assert_eq!(s_pub, StatusCode::OK);
    let present = pub_body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["slug"] == slug);
    assert!(!present, "retired slug must NOT appear in public list: {pub_body}");

    // …but the admin list MUST include it.
    let (s_admin, admin_body) = call(app, "GET", "/v1/admin/sources", Some(&token), None).await;
    assert_eq!(s_admin, StatusCode::OK);
    let our = admin_body
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["slug"] == slug)
        .expect("retired slug must appear in admin list");
    assert!(!our["retired_at"].is_null(), "retired_at must be set: {our}");
}

#[tokio::test]
async fn retire_source_404_on_unknown_slug() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;

    let (status, body) = call(
        app,
        "DELETE",
        &format!("/v1/admin/sources/{}", unique_slug("crud-gone")),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn retire_source_400_on_already_retired() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;
    let slug = unique_slug("crud-rere");
    source::insert(&h.pool, &slug, "Twice", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    source::retire(&h.pool, &slug).await.unwrap();

    let (status, body) =
        call(app, "DELETE", &format!("/v1/admin/sources/{slug}"), Some(&token), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("already retired"));
}

#[tokio::test]
async fn list_admin_includes_retired() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_token(app.clone(), "aaron", &kp).await;
    let active_slug = unique_slug("crud-act");
    let retired_slug = unique_slug("crud-ret");
    source::insert(&h.pool, &active_slug, "Active One", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    source::insert(&h.pool, &retired_slug, "Retired One", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    source::retire(&h.pool, &retired_slug).await.unwrap();

    let (status, body) = call(app, "GET", "/v1/admin/sources", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().expect("array body");
    let active = rows
        .iter()
        .find(|s| s["slug"] == active_slug)
        .expect("active appears");
    assert!(active["retired_at"].is_null());
    let retired = rows
        .iter()
        .find(|s| s["slug"] == retired_slug)
        .expect("retired appears");
    assert!(!retired["retired_at"].is_null(), "retired_at must be set: {retired}");
}

#[tokio::test]
async fn unauthenticated_writes_return_401() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(admin_user_store("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    for (method, uri, body) in [
        ("POST", "/v1/admin/sources", Some(json!({"slug": "x", "kind": "docs_site"}))),
        ("PATCH", "/v1/admin/sources/x", Some(json!({}))),
        ("DELETE", "/v1/admin/sources/x", None),
        ("GET", "/v1/admin/sources", None),
    ] {
        let (status, _) = call(app.clone(), method, uri, None, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn writer_role_cannot_write_or_list_admin() {
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
        ("POST", "/v1/admin/sources", Some(json!({"slug": "x", "kind": "docs_site"}))),
        ("PATCH", "/v1/admin/sources/x", Some(json!({}))),
        ("DELETE", "/v1/admin/sources/x", None),
        ("GET", "/v1/admin/sources", None),
    ] {
        let (status, body) = call(app.clone(), method, uri, Some(&token), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
        assert_eq!(body["error"]["code"], "forbidden");
    }
}

//! Wiremock-driven integration tests for `mnm versions` (Phase 14).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mn_cli::commands::versions::{
    list_request, promote_request, require_admin_token_from, resolve_rollback_target,
    retire_request, show_request,
};
use mn_core::config::ConfigEnv;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[derive(Default)]
struct FakeEnv(HashMap<String, String>);

impl ConfigEnv for FakeEnv {
    fn var(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

fn sv_row(revision: i32, is_active: bool, status: &str) -> serde_json::Value {
    json!({
        "id": "00000000-0000-0000-0000-000000000000",
        "source_id": "00000000-0000-0000-0000-000000000000",
        "revision": revision,
        "status": status,
        "is_active": is_active,
        "ingested_at": "2026-05-14T00:00:00Z",
        "ingest_cli_version": "0.1.0",
        "embedding_model_id": "00000000-0000-0000-0000-000000000000",
        "content_hash": "h",
        "notes": null,
        "retired_at": null,
    })
}

#[tokio::test]
async fn list_request_decodes_versions_array() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources/docs/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            sv_row(3, true, "active"),
            sv_row(2, false, "inactive"),
            sv_row(1, false, "inactive"),
        ])))
        .mount(&server)
        .await;

    let v = list_request(&http_client(), &server.uri(), "docs")
        .await
        .expect("ok");
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["revision"], 3);
    assert_eq!(arr[0]["is_active"], true);
}

#[tokio::test]
async fn show_request_decodes_single_version() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources/docs/versions/2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sv_row(2, false, "inactive")))
        .mount(&server)
        .await;
    let v = show_request(&http_client(), &server.uri(), "docs", 2)
        .await
        .expect("ok");
    assert_eq!(v["revision"], 2);
    assert_eq!(v["status"], "inactive");
}

#[tokio::test]
async fn promote_sends_bearer_and_returns_promote_result() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(Option::<String>::None));
    let cap = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/v1/admin/sources/docs/versions/1/promote"))
        .respond_with(move |req: &Request| {
            if let Some(h) = req.headers.get("authorization") {
                *cap.lock().unwrap() = Some(h.to_str().unwrap().to_owned());
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "promoted_revision": 1,
                "demoted_revision": 3,
            }))
        })
        .mount(&server)
        .await;

    let v = promote_request(&http_client(), &server.uri(), "docs", 1, "admin-tok")
        .await
        .expect("ok");
    assert_eq!(v["promoted_revision"], 1);
    assert_eq!(v["demoted_revision"], 3);
    assert_eq!(captured.lock().unwrap().clone().unwrap(), "Bearer admin-tok");
}

#[tokio::test]
async fn promote_surfaces_400_clearly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/admin/sources/docs/versions/3/promote"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"code": "invalid_request", "message": "already active"}
        })))
        .mount(&server)
        .await;

    let err = promote_request(&http_client(), &server.uri(), "docs", 3, "tok")
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("400"), "{msg}");
}

#[tokio::test]
async fn retire_sends_bearer_and_returns_retired_row() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/admin/sources/docs/versions/1/retire"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "source_id": "00000000-0000-0000-0000-000000000000",
            "revision": 1,
            "status": "retired",
            "is_active": false,
            "ingested_at": "2026-05-14T00:00:00Z",
            "ingest_cli_version": "0.1.0",
            "embedding_model_id": "00000000-0000-0000-0000-000000000000",
            "content_hash": "h",
            "notes": null,
            "retired_at": "2026-05-14T01:00:00Z",
        })))
        .mount(&server)
        .await;

    let v = retire_request(&http_client(), &server.uri(), "docs", 1, "tok")
        .await
        .expect("ok");
    assert_eq!(v["status"], "retired");
    assert!(!v["retired_at"].is_null());
}

#[tokio::test]
async fn resolve_rollback_target_picks_highest_inactive_revision() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources/docs/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            sv_row(5, true, "active"),
            sv_row(4, false, "inactive"),
            sv_row(3, false, "inactive"),
            sv_row(2, false, "retired"),
            sv_row(1, false, "inactive"),
        ])))
        .mount(&server)
        .await;

    let target = resolve_rollback_target(&http_client(), &server.uri(), "docs")
        .await
        .expect("ok");
    assert_eq!(target, 4, "rollback picks the newest inactive revision");
}

#[tokio::test]
async fn resolve_rollback_target_errors_when_only_active_exists() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources/docs/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([sv_row(1, true, "active")])))
        .mount(&server)
        .await;

    let err = resolve_rollback_target(&http_client(), &server.uri(), "docs")
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("no prior version"), "{msg}");
}

#[tokio::test]
async fn missing_auth_toml_errors_with_mnm_login_hint() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let mut env = FakeEnv::default();
    env.0
        .insert("HOME".to_owned(), tmp.path().display().to_string());
    let err = require_admin_token_from(&env).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("mnm login"), "{msg}");
}

#[tokio::test]
async fn list_404_surfaces_clearly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources/unknown/versions"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"code": "not_found", "message": "source `unknown` not found"}
        })))
        .mount(&server)
        .await;
    let err = list_request(&http_client(), &server.uri(), "unknown")
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("404"), "{msg}");
}

//! Wiremock-driven integration tests for `mnm sources` admin subcommands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use midnight_manual::commands::sources::{
    confirm_retire, create_request, list_all_request, require_admin_token_from, retire_request,
    update_request, CreateArgs, CreateKind, RetireArgs, UpdateArgs,
};
use mnm_core::config::ConfigEnv;
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

#[tokio::test]
async fn create_posts_the_right_body_shape() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(Option::<serde_json::Value>::None));
    let cap = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/v1/admin/sources"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value =
                serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
            *cap.lock().unwrap() = Some(body.clone());
            ResponseTemplate::new(201).set_body_json(json!({
                "id": "00000000-0000-0000-0000-000000000000",
                "slug": body["slug"],
                "display_name": "ignored",
                "kind": body["kind"],
                "origin_url": null,
                "retention_count": 5,
                "created_at": "2026-05-14T00:00:00Z",
                "retired_at": null,
            }))
        })
        .mount(&server)
        .await;

    let args = CreateArgs {
        slug: "docs-v2".to_owned(),
        display_name: Some("Docs v2".to_owned()),
        kind: CreateKind::DocsSite,
        origin_url: Some("https://example.com".to_owned()),
        retention_count: Some(7),
    };
    let _v = create_request(&http_client(), &server.uri(), &args, "admin-bearer")
        .await
        .expect("ok");
    let body = captured.lock().unwrap().clone().unwrap();
    assert_eq!(body["slug"], "docs-v2");
    assert_eq!(body["display_name"], "Docs v2");
    assert_eq!(body["kind"], "docs_site");
    assert_eq!(body["origin_url"], "https://example.com");
    assert_eq!(body["retention_count"], 7);
}

#[tokio::test]
async fn create_sends_bearer_header() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(Option::<String>::None));
    let cap = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/v1/admin/sources"))
        .respond_with(move |req: &Request| {
            if let Some(h) = req.headers.get("authorization") {
                *cap.lock().unwrap() = Some(h.to_str().unwrap().to_owned());
            }
            ResponseTemplate::new(201).set_body_json(json!({
                "id": "00000000-0000-0000-0000-000000000000",
                "slug": "x", "display_name": "x", "kind": "docs_site",
                "origin_url": null, "retention_count": 5,
                "created_at": "2026-05-14T00:00:00Z", "retired_at": null,
            }))
        })
        .mount(&server)
        .await;
    let args = CreateArgs {
        slug: "x".to_owned(),
        display_name: None,
        kind: CreateKind::DocsSite,
        origin_url: None,
        retention_count: None,
    };
    create_request(&http_client(), &server.uri(), &args, "abc-token-789")
        .await
        .expect("ok");
    assert_eq!(captured.lock().unwrap().clone().unwrap(), "Bearer abc-token-789");
}

#[tokio::test]
async fn update_requires_at_least_one_flag() {
    let args = UpdateArgs {
        slug: "docs".to_owned(),
        display_name: None,
        origin_url: None,
        retention_count: None,
    };
    let server = MockServer::start().await;
    let err = update_request(&http_client(), &server.uri(), &args, "any")
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("at least one"), "expected hint: {msg}");
}

#[tokio::test]
async fn update_sends_only_set_fields() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(Option::<serde_json::Value>::None));
    let cap = Arc::clone(&captured);
    Mock::given(method("PATCH"))
        .and(path("/v1/admin/sources/docs"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value =
                serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
            *cap.lock().unwrap() = Some(body);
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "00000000-0000-0000-0000-000000000000",
                "slug": "docs", "display_name": "Renamed", "kind": "docs_site",
                "origin_url": null, "retention_count": 5,
                "created_at": "2026-05-14T00:00:00Z", "retired_at": null,
            }))
        })
        .mount(&server)
        .await;

    let args = UpdateArgs {
        slug: "docs".to_owned(),
        display_name: Some("Renamed".to_owned()),
        origin_url: None,
        retention_count: None,
    };
    update_request(&http_client(), &server.uri(), &args, "tok")
        .await
        .expect("ok");
    let body = captured.lock().unwrap().clone().unwrap();
    assert_eq!(body["display_name"], "Renamed");
    assert!(body.get("origin_url").is_none(), "absent flags must NOT be serialized: {body}");
    assert!(body.get("retention_count").is_none(), "{body}");
}

#[tokio::test]
async fn retire_refuses_without_yes_in_non_interactive_mode() {
    // The Rust test harness redirects stdin from /dev/null; IsTerminal is
    // therefore false. confirm_retire must refuse.
    let args = RetireArgs {
        slug: "docs".to_owned(),
        yes: false,
    };
    let err = confirm_retire(&args).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("non-interactively"), "{msg}");
}

#[tokio::test]
async fn retire_with_yes_skips_confirmation() {
    let args = RetireArgs {
        slug: "docs".to_owned(),
        yes: true,
    };
    confirm_retire(&args).expect("--yes must bypass confirmation");
}

#[tokio::test]
async fn retire_sends_bearer_header_on_delete() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(Option::<String>::None));
    let cap = Arc::clone(&captured);
    Mock::given(method("DELETE"))
        .and(path("/v1/admin/sources/docs"))
        .respond_with(move |req: &Request| {
            if let Some(h) = req.headers.get("authorization") {
                *cap.lock().unwrap() = Some(h.to_str().unwrap().to_owned());
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "00000000-0000-0000-0000-000000000000",
                "slug": "docs", "display_name": "X", "kind": "docs_site",
                "origin_url": null, "retention_count": 5,
                "created_at": "2026-05-14T00:00:00Z", "retired_at": "2026-05-14T01:00:00Z",
            }))
        })
        .mount(&server)
        .await;
    retire_request(&http_client(), &server.uri(), "docs", "bearer-xyz")
        .await
        .expect("ok");
    assert_eq!(captured.lock().unwrap().clone().unwrap(), "Bearer bearer-xyz");
}

#[tokio::test]
async fn list_all_decodes_mixed_active_and_retired() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/sources"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": "00000000-0000-0000-0000-000000000001",
                "slug": "active",
                "display_name": "Active",
                "kind": "docs_site",
                "origin_url": null,
                "retention_count": 5,
                "created_at": "2026-05-14T00:00:00Z",
                "retired_at": null,
            },
            {
                "id": "00000000-0000-0000-0000-000000000002",
                "slug": "retired",
                "display_name": "Retired",
                "kind": "code_repo",
                "origin_url": null,
                "retention_count": 5,
                "created_at": "2026-05-14T00:00:00Z",
                "retired_at": "2026-05-14T01:00:00Z",
            },
        ])))
        .mount(&server)
        .await;
    let v = list_all_request(&http_client(), &server.uri(), "tok")
        .await
        .expect("ok");
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let active = arr.iter().find(|r| r["slug"] == "active").unwrap();
    assert!(active["retired_at"].is_null());
    let retired = arr.iter().find(|r| r["slug"] == "retired").unwrap();
    assert!(!retired["retired_at"].is_null());
}

#[tokio::test]
async fn missing_auth_toml_errors_with_mnm_login_hint() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let mut env = FakeEnv::default();
    env.0
        .insert("HOME".to_owned(), tmp.path().display().to_string());
    let err = require_admin_token_from(&env).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("mnm login"), "expected mnm login hint: {msg}");
}

#[tokio::test]
async fn missing_home_errors_with_clear_message() {
    let env = FakeEnv::default();
    let err = require_admin_token_from(&env).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("HOME") || msg.contains("XDG_CONFIG_HOME"),
        "expected HOME/XDG hint: {msg}",
    );
}

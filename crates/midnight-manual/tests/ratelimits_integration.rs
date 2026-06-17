//! Wiremock-driven integration tests for `mnm ratelimits` (Phase 16).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use midnight_manual::commands::ratelimits::{
    add_request, confirm_remove, extend_request, list_request, parse_limit, parse_ttl,
    remove_request, require_admin_token_from, RemoveArgs,
};
use mnm_core::config::ConfigEnv;
use serde_json::json;
use time::Duration;
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

fn override_row(id: &str, cidr: &str, limit: i64) -> serde_json::Value {
    json!({
        "id": id,
        "cidr": cidr,
        "limit_rps": limit,
        "expires_at": "2030-01-01T00:00:00Z",
        "note": "n",
        "created_by": "aaron",
        "created_at": "2026-05-22T00:00:00Z",
    })
}

#[test]
fn parse_limit_and_ttl_unit_behaviour() {
    assert_eq!(parse_limit("200/s").unwrap(), 200);
    assert_eq!(parse_limit("75").unwrap(), 75);
    assert!(parse_limit("0").is_err());
    assert_eq!(parse_ttl("48h").unwrap(), Duration::seconds(172_800));
    assert_eq!(parse_ttl("7d").unwrap(), Duration::seconds(604_800));
    assert!(parse_ttl("10y").is_err());
}

#[tokio::test]
async fn add_posts_expected_body_and_bearer() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(None::<(String, serde_json::Value)>));
    let cap = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/v1/admin/ratelimits"))
        .respond_with(move |req: &Request| {
            let auth = req
                .headers
                .get("authorization")
                .map(|h| h.to_str().unwrap().to_owned())
                .unwrap_or_default();
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            *cap.lock().unwrap() = Some((auth, body));
            ResponseTemplate::new(201).set_body_json(override_row(
                "11111111-1111-1111-1111-111111111111",
                "203.0.113.0/24",
                200,
            ))
        })
        .mount(&server)
        .await;

    let v = add_request(
        &http_client(),
        &server.uri(),
        "203.0.113.0/24",
        200,
        "2030-01-01T00:00:00Z",
        Some("hackathon-london"),
        "admin-tok",
    )
    .await
    .expect("ok");
    assert_eq!(v["cidr"], "203.0.113.0/24");

    let (auth, body) = captured.lock().unwrap().clone().unwrap();
    assert_eq!(auth, "Bearer admin-tok");
    assert_eq!(body["cidr"], "203.0.113.0/24");
    assert_eq!(body["limit_rps"], 200);
    assert_eq!(body["expires_at"], "2030-01-01T00:00:00Z");
    assert_eq!(body["note"], "hackathon-london");
}

#[tokio::test]
async fn add_omits_note_when_absent() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let cap = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/v1/admin/ratelimits"))
        .respond_with(move |req: &Request| {
            *cap.lock().unwrap() = Some(serde_json::from_slice(&req.body).unwrap());
            ResponseTemplate::new(201).set_body_json(override_row("id", "203.0.113.0/24", 10))
        })
        .mount(&server)
        .await;

    add_request(
        &http_client(),
        &server.uri(),
        "203.0.113.0/24",
        10,
        "2030-01-01T00:00:00Z",
        None,
        "tok",
    )
    .await
    .expect("ok");
    let body = captured.lock().unwrap().clone().unwrap();
    assert!(body.get("note").is_none(), "absent note must be omitted: {body}");
}

#[tokio::test]
async fn list_decodes_array_and_sends_bearer() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(None::<String>));
    let cap = Arc::clone(&captured);
    Mock::given(method("GET"))
        .and(path("/v1/admin/ratelimits"))
        .respond_with(move |req: &Request| {
            if let Some(h) = req.headers.get("authorization") {
                *cap.lock().unwrap() = Some(h.to_str().unwrap().to_owned());
            }
            ResponseTemplate::new(200).set_body_json(json!([
                override_row("a", "203.0.113.0/24", 10),
                override_row("b", "198.51.100.0/24", 20),
            ]))
        })
        .mount(&server)
        .await;

    let v = list_request(&http_client(), &server.uri(), "tok")
        .await
        .expect("ok");
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(captured.lock().unwrap().clone().unwrap(), "Bearer tok");
}

#[tokio::test]
async fn extend_patches_expires_at() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let cap = Arc::clone(&captured);
    Mock::given(method("PATCH"))
        .and(path("/v1/admin/ratelimits/abc"))
        .respond_with(move |req: &Request| {
            *cap.lock().unwrap() = Some(serde_json::from_slice(&req.body).unwrap());
            ResponseTemplate::new(200).set_body_json(override_row("abc", "203.0.113.0/24", 10))
        })
        .mount(&server)
        .await;

    extend_request(&http_client(), &server.uri(), "abc", "2031-01-01T00:00:00Z", "tok")
        .await
        .expect("ok");
    let body = captured.lock().unwrap().clone().unwrap();
    assert_eq!(body["expires_at"], "2031-01-01T00:00:00Z");
}

#[tokio::test]
async fn remove_sends_delete() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/v1/admin/ratelimits/xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(override_row(
            "xyz",
            "203.0.113.0/24",
            10,
        )))
        .mount(&server)
        .await;
    let v = remove_request(&http_client(), &server.uri(), "xyz", "tok")
        .await
        .expect("ok");
    assert_eq!(v["id"], "xyz");
}

#[test]
fn remove_refuses_non_interactively_without_yes() {
    // The test harness has no TTY on stdin, so confirm_remove must refuse.
    let refused = confirm_remove(&RemoveArgs {
        id: "abc".to_owned(),
        yes: false,
    });
    assert!(refused.is_err(), "must refuse without --yes when non-interactive");

    // With --yes it proceeds.
    let ok = confirm_remove(&RemoveArgs {
        id: "abc".to_owned(),
        yes: true,
    });
    assert!(ok.is_ok());
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

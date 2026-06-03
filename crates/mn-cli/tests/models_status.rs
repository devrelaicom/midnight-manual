//! Wiremock-driven tests for `mnm models status` / `status_request`.
//!
//! Mocks both `GET /v1/models/active` (returns voyage-code-3@1) and
//! `GET /v1/admin/sources?not_model=voyage-code-3@1` (returns a couple of
//! sources). Validates that the right URL is hit with the right bearer and
//! that the response is parsed correctly.

#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use mn_cli::commands::models::status_request;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

fn active_model_response() -> serde_json::Value {
    json!({
        "name": "voyage-code-3",
        "revision": 1,
        "dim": 1024,
        "provider": "voyageai"
    })
}

fn sources_not_on_model_response() -> serde_json::Value {
    json!({
        "sources": [
            { "slug": "midnight-docs", "origin_url": "https://docs.midnight.network" },
            { "slug": "compact-lang",  "origin_url": null }
        ]
    })
}

/// Helper that mounts the active-model mock and returns the wire id.
async fn mount_active_mock(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(active_model_response()))
        .mount(server)
        .await;
}

// ── status_request unit tests ────────────────────────────────────────────────

#[tokio::test]
async fn status_request_hits_correct_url_with_bearer() {
    let server = MockServer::start().await;
    // Capture the incoming request's Authorization header.
    let captured_auth = Arc::new(Mutex::new(None::<String>));
    let cap = Arc::clone(&captured_auth);

    Mock::given(method("GET"))
        .and(path("/v1/admin/sources"))
        .and(query_param("not_model", "voyage-code-3@1"))
        .respond_with(move |req: &Request| {
            let auth = req
                .headers
                .get("authorization")
                .map(|h| h.to_str().unwrap().to_owned())
                .unwrap_or_default();
            *cap.lock().unwrap() = Some(auth);
            ResponseTemplate::new(200).set_body_json(sources_not_on_model_response())
        })
        .mount(&server)
        .await;

    let value = status_request(&http_client(), &server.uri(), "voyage-code-3@1", "my-admin-tok")
        .await
        .expect("status_request should succeed");

    // Response is the raw JSON object from the server.
    let sources = value["sources"].as_array().expect("sources array");
    assert_eq!(sources.len(), 2, "two sources expected");
    assert_eq!(sources[0]["slug"], "midnight-docs");
    assert_eq!(sources[1]["slug"], "compact-lang");

    // Bearer was forwarded correctly.
    assert_eq!(captured_auth.lock().unwrap().clone().unwrap(), "Bearer my-admin-tok");
}

#[tokio::test]
async fn status_request_propagates_server_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/sources"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": { "code": "forbidden", "message": "admin tier required" },
            "request_id": "rid-test"
        })))
        .mount(&server)
        .await;

    let err = status_request(&http_client(), &server.uri(), "voyage-code-3@1", "bad-tok")
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("403"), "error should mention HTTP status: {msg}");
}

#[tokio::test]
async fn status_request_empty_sources_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/sources"))
        .and(query_param("not_model", "voyage-code-3@1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "sources": [] })))
        .mount(&server)
        .await;

    let value = status_request(&http_client(), &server.uri(), "voyage-code-3@1", "tok")
        .await
        .expect("ok");
    let sources = value["sources"].as_array().expect("sources array");
    assert!(sources.is_empty(), "empty sources must deserialise as empty array");
}

// ── integration-style: active + status together ──────────────────────────────

/// Verifies the wire-id is derived from the active-model response and forwarded
/// unchanged to the admin/sources endpoint as a query parameter.
#[tokio::test]
async fn status_derives_wire_id_from_active_model() {
    let server = MockServer::start().await;
    mount_active_mock(&server).await;

    // Capture the exact query string received by the admin endpoint.
    let captured_wire = Arc::new(Mutex::new(None::<String>));
    let cap = Arc::clone(&captured_wire);

    Mock::given(method("GET"))
        .and(path("/v1/admin/sources"))
        .respond_with(move |req: &Request| {
            // Extract the `not_model` query parameter value.
            let wire = req
                .url
                .query_pairs()
                .find(|(k, _)| k == "not_model")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            *cap.lock().unwrap() = Some(wire);
            ResponseTemplate::new(200).set_body_json(json!({ "sources": [] }))
        })
        .mount(&server)
        .await;

    // Fetch the active model wire id the same way run_status does.
    let active = mn_cli::commands::models::fetch_active(&server.uri())
        .await
        .expect("fetch_active");
    let wire = format!("{}@{}", active.name, active.revision);
    assert_eq!(wire, "voyage-code-3@1");

    // Now call status_request using that wire id.
    status_request(&http_client(), &server.uri(), &wire, "tok")
        .await
        .expect("status_request");

    let forwarded = captured_wire.lock().unwrap().clone().unwrap();
    assert_eq!(forwarded, "voyage-code-3@1", "wire id must be forwarded verbatim");
}

/// Confirms the Authorization header uses the Bearer scheme.
#[tokio::test]
async fn status_request_uses_bearer_scheme() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/sources"))
        .and(header("authorization", "Bearer secret-admin-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "sources": [] })))
        .mount(&server)
        .await;

    status_request(&http_client(), &server.uri(), "voyage-code-3@1", "secret-admin-token")
        .await
        .expect("status_request with correct bearer");
}

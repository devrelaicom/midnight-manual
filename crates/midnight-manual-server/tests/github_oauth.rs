//! HTTP exercises for the `GET /v1/auth/github/{start,callback}` flow
//! (FR-062, FR-115, FR-117).
//!
//! Mocks the GitHub OAuth token endpoint plus the `/user` and
//! `/user/memberships/orgs/<org>` API calls via wiremock so the test can
//! drive the full callback without touching the live GitHub API.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use midnight_manual_server::{app, config::ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal user-store body so `AuthState` boots — we don't exercise admin
/// login in this test, but the auth subsystem itself is required before
/// GitHub OAuth becomes available (admin user-store gates the JWT secret).
const USER_STORE_BODY: &str = r"
schema_version = 1
";

fn cfg_with_github_oauth(gh_mock_uri: &str) -> ServerConfig {
    ServerConfig {
        user_store_body: Some(USER_STORE_BODY.to_owned()),
        jwt_secret: Some(vec![7u8; 32]),
        github_oauth_client_id: Some("cid-test".to_owned()),
        github_oauth_client_secret: Some("csec-test".to_owned()),
        github_oauth_redirect_url: Some("http://test.example/v1/auth/github/callback".to_owned()),
        github_org: Some("midnight-network".to_owned()),
        read_token_ttl_days: 30,
        github_api_base_url: gh_mock_uri.to_owned(),
        github_authorize_url: format!("{gh_mock_uri}/login/oauth/authorize"),
        github_token_url: format!("{gh_mock_uri}/login/oauth/access_token"),
        ..Default::default()
    }
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .expect("send request");
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, headers, v)
}

#[tokio::test]
async fn start_redirects_to_github_with_state_param() {
    let h = common::boot().await;
    let gh = MockServer::start().await;
    let cfg = cfg_with_github_oauth(&gh.uri());
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, headers, _) = get(app, "/v1/auth/github/start?cli_port=12345").await;
    assert_eq!(status, StatusCode::SEE_OTHER, "expected redirect");
    let loc = headers
        .get("location")
        .expect("location header")
        .to_str()
        .unwrap();
    assert!(loc.contains("/login/oauth/authorize"));
    assert!(loc.contains("client_id=cid-test"));
    assert!(loc.contains("scope=read"));
    assert!(loc.contains("state="));
}

#[tokio::test]
async fn start_503_when_oauth_unconfigured() {
    let h = common::boot().await;
    let cfg = ServerConfig {
        user_store_body: Some(USER_STORE_BODY.to_owned()),
        jwt_secret: Some(vec![7u8; 32]),
        ..Default::default()
    };
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, _, _) = get(app, "/v1/auth/github/start").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn callback_happy_path_mints_read_uplift_jwt() {
    let h = common::boot().await;
    let gh = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gho_test_access",
            "token_type": "bearer",
            "scope": "read:org"
        })))
        .mount(&gh)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "login": "aaron",
            "id": 42
        })))
        .mount(&gh)
        .await;
    Mock::given(method("GET"))
        .and(path("/user/memberships/orgs/midnight-network"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "state": "active",
            "role": "member"
        })))
        .mount(&gh)
        .await;

    let cfg = cfg_with_github_oauth(&gh.uri());
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    // First /start to mint a real state.
    let (status, headers, _) = get(app.clone(), "/v1/auth/github/start").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let loc = headers.get("location").unwrap().to_str().unwrap();
    let state = loc
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    // Then /callback with that state.
    let (status, _, body) =
        get(app, &format!("/v1/auth/github/callback?code=abc&state={state}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["github_login"], "aaron");
    let token = body["token"].as_str().expect("token in body");
    assert!(token.split('.').count() == 3, "looks like a JWT");
    assert!(body["expires_at"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn callback_rejects_non_member_with_403() {
    let h = common::boot().await;
    let gh = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gho_test_access"
        })))
        .mount(&gh)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login": "stranger"})))
        .mount(&gh)
        .await;
    // GitHub returns 404 for non-members.
    Mock::given(method("GET"))
        .and(path("/user/memberships/orgs/midnight-network"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "message": "Not Found"
        })))
        .mount(&gh)
        .await;

    let cfg = cfg_with_github_oauth(&gh.uri());
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (_, headers, _) = get(app.clone(), "/v1/auth/github/start").await;
    let loc = headers.get("location").unwrap().to_str().unwrap();
    let state = loc
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    let (status, _, body) =
        get(app, &format!("/v1/auth/github/callback?code=abc&state={state}")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "forbidden");
}

#[tokio::test]
async fn callback_with_unknown_state_returns_400() {
    let h = common::boot().await;
    let gh = MockServer::start().await;
    let cfg = cfg_with_github_oauth(&gh.uri());
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, _, body) =
        get(app, "/v1/auth/github/callback?code=x&state=not-a-real-state").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn callback_redirects_to_cli_port_when_present() {
    let h = common::boot().await;
    let gh = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gho_test_access"
        })))
        .mount(&gh)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login": "aaron"})))
        .mount(&gh)
        .await;
    Mock::given(method("GET"))
        .and(path("/user/memberships/orgs/midnight-network"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"state": "active"})))
        .mount(&gh)
        .await;

    let cfg = cfg_with_github_oauth(&gh.uri());
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (_, headers, _) = get(app.clone(), "/v1/auth/github/start?cli_port=54321").await;
    let loc = headers.get("location").unwrap().to_str().unwrap();
    let state = loc
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/auth/github/callback?code=abc&state={state}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(loc.starts_with("http://127.0.0.1:54321/oauth"));
    assert!(loc.contains("token="));
    assert!(loc.contains("github_login=aaron"));
    assert!(loc.contains("expires_at="));
}

// Issue #177: when the CLI supplies a `cli_state` nonce on start, the callback
// echoes it back verbatim as `state=<nonce>` in the loopback redirect so the
// CLI can bind the callback to the flow it initiated.
#[tokio::test]
async fn callback_echoes_cli_state_into_loopback_redirect() {
    let h = common::boot().await;
    let gh = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gho_test_access"
        })))
        .mount(&gh)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login": "aaron"})))
        .mount(&gh)
        .await;
    Mock::given(method("GET"))
        .and(path("/user/memberships/orgs/midnight-network"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"state": "active"})))
        .mount(&gh)
        .await;

    let cfg = cfg_with_github_oauth(&gh.uri());
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let cli_nonce = "cli-nonce-xyz789";
    let (_, headers, _) = get(
        app.clone(),
        &format!("/v1/auth/github/start?cli_port=54321&cli_state={cli_nonce}"),
    )
    .await;
    let loc = headers.get("location").unwrap().to_str().unwrap();
    let state = loc
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/auth/github/callback?code=abc&state={state}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(loc.starts_with("http://127.0.0.1:54321/oauth"));
    assert!(
        loc.contains(&format!("state={cli_nonce}")),
        "loopback redirect must echo the CLI nonce, got: {loc}",
    );
}

#[tokio::test]
async fn callback_with_user_denial_returns_403() {
    let h = common::boot().await;
    let gh = MockServer::start().await;
    let cfg = cfg_with_github_oauth(&gh.uri());
    let app = app::build(h.pool.clone(), cfg).expect("build app");

    let (status, _, body) = get(
        app,
        "/v1/auth/github/callback?error=access_denied&error_description=The+user+has+denied",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

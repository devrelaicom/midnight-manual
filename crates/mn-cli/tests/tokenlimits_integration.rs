//! Integration test for `mnm tokenlimits add` — verifies the POST body shape
//! and bearer-auth header against a mocked admin endpoint.

#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use mn_cli::commands::tokenlimits::add_request;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

#[tokio::test]
async fn add_posts_subject_and_limits_with_bearer() {
    let server = MockServer::start().await;
    let cap = Arc::new(Mutex::new(None::<(String, serde_json::Value)>));
    let c = Arc::clone(&cap);
    Mock::given(method("POST"))
        .and(path("/v1/admin/tokenlimits"))
        .respond_with(move |req: &Request| {
            let auth = req
                .headers
                .get("authorization")
                .map(|h| h.to_str().unwrap().to_owned())
                .unwrap_or_default();
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            *c.lock().unwrap() = Some((auth, body));
            ResponseTemplate::new(201)
                .set_body_json(serde_json::json!({"id":"00000000-0000-0000-0000-000000000000"}))
        })
        .mount(&server)
        .await;

    add_request(
        &http_client(),
        &server.uri(),
        "user",
        "alice",
        4000,
        40000,
        "2030-01-01T00:00:00Z",
        None,
        "admin-tok",
    )
    .await
    .unwrap();

    let (auth, body) = cap.lock().unwrap().clone().unwrap();
    assert_eq!(auth, "Bearer admin-tok");
    assert_eq!(body["subject_kind"], "user");
    assert_eq!(body["subject"], "alice");
    assert_eq!(body["hourly"], 4000);
    assert_eq!(body["daily"], 40000);
    assert_eq!(body["expires_at"], "2030-01-01T00:00:00Z");
}

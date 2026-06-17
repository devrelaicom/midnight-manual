//! Integration tests for `mnm models active` driven against a `wiremock`
//! mock of `GET /v1/models/active`. `mnm models pull` is not tested here —
//! it only primes the local cache directory now (both the embedder and the
//! reranker are remote `VoyageAI`, so there is nothing to download).

use midnight_manual::commands::models::fetch_active;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn fetch_active_decodes_canonical_shape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "bge-base-en-v1.5",
            "revision": 1,
            "dim": 768,
            "provider": "baai",
        })))
        .mount(&server)
        .await;

    let resp = fetch_active(&server.uri()).await.expect("should decode");
    assert_eq!(resp.name, "bge-base-en-v1.5");
    assert_eq!(resp.revision, 1);
    assert_eq!(resp.dim, 768);
    assert_eq!(resp.provider, "baai");
}

#[tokio::test]
async fn fetch_active_surfaces_503_clearly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": {"code": "service_unavailable", "message": "DB down"}
        })))
        .mount(&server)
        .await;

    let err = fetch_active(&server.uri()).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("503"), "expected 503 in error: {msg}");
}

#[tokio::test]
async fn fetch_active_rejects_malformed_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let err = fetch_active(&server.uri()).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("parse"), "expected parse error: {msg}");
}

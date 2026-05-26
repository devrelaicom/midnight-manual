//! Integration tests for the non-search tool dispatch paths. These thread
//! the `run_passthrough_id` helper through a wiremock cloud and verify the
//! input-validation gates (uuid parsing, missing fields) before the wire
//! call goes out.

use std::sync::Arc;

use mn_mcp::cloud_client::CloudClient;
use mn_mcp::tools::{run_passthrough_id, PassthroughError, PassthroughKind};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn run_passthrough_id_rejects_missing_id() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_passthrough_id(&json!({}), &client, PassthroughKind::Chunk)
        .await
        .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_passthrough_id_rejects_non_uuid() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_passthrough_id(&json!({"id": "not-a-uuid"}), &client, PassthroughKind::Chunk)
        .await
        .unwrap_err();
    match err {
        PassthroughError::InvalidInput(msg) => assert!(msg.contains("UUID")),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[tokio::test]
async fn run_passthrough_id_hits_chunk_endpoint() {
    let server = MockServer::start().await;
    let id = "11111111-1111-1111-1111-111111111111";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::Chunk)
        .await
        .unwrap();
    assert_eq!(v["id"], id);
}

#[tokio::test]
async fn run_passthrough_id_maps_404() {
    let server = MockServer::start().await;
    let id = "22222222-2222-2222-2222-222222222222";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/siblings")))
        .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::Siblings)
        .await
        .unwrap_err();
    assert!(matches!(err, PassthroughError::NotFound(_)));
}

#[tokio::test]
async fn run_passthrough_id_hits_document_endpoint() {
    let server = MockServer::start().await;
    let id = "11111111-1111-1111-1111-111111111100";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::Document)
        .await
        .unwrap();
    assert_eq!(v["id"], id);
}

#[tokio::test]
async fn run_passthrough_id_hits_document_full_endpoint() {
    let server = MockServer::start().await;
    let id = "11111111-1111-1111-1111-111111111101";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/full")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id, "chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::DocumentFull)
        .await
        .unwrap();
    assert_eq!(v["chunks"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn run_passthrough_id_maps_document_full_412() {
    let server = MockServer::start().await;
    let id = "11111111-1111-1111-1111-111111111102";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/full")))
        .respond_with(ResponseTemplate::new(412).set_body_json(json!({
            "error": "too_many_chunks",
            "chunk_count": 1240,
            "cap": 500,
            "hint": "Use GET /v1/documents/.../chunks?from=K&limit=L (default L=20)",
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::DocumentFull)
        .await
        .unwrap_err();
    match err {
        PassthroughError::TooManyChunks { chunk_count, cap, .. } => {
            assert_eq!(chunk_count, 1240);
            assert_eq!(cap, 500);
        }
        other => panic!("expected TooManyChunks, got {other:?}"),
    }
}

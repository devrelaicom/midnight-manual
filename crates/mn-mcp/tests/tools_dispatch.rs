//! Integration tests for the non-search tool dispatch paths. These thread
//! the `run_passthrough_id` helper through a wiremock cloud and verify the
//! input-validation gates (uuid parsing, missing fields) before the wire
//! call goes out.

use std::sync::Arc;

use mn_mcp::cloud_client::CloudClient;
use mn_mcp::tools::{
    run_chunk_nav, run_document_chunks, run_passthrough_id, ChunkNavDirection, PassthroughError,
    PassthroughKind,
};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
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
        .and(path(format!("/v1/documents/{id}/full")))
        .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::DocumentFull)
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

#[tokio::test]
async fn run_chunk_nav_next_uses_count_query_param() {
    let server = MockServer::start().await;
    let id = "22222222-2222-2222-2222-222222222200";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .and(query_param("count", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_chunk_nav(&json!({"id": id, "count": 7}), &client, ChunkNavDirection::Next)
        .await
        .unwrap();
    assert!(v["chunks"].is_array());
}

#[tokio::test]
async fn run_chunk_nav_defaults_count_to_five() {
    let server = MockServer::start().await;
    let id = "22222222-2222-2222-2222-222222222201";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .and(query_param("count", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let _ = run_chunk_nav(&json!({"id": id}), &client, ChunkNavDirection::Next)
        .await
        .unwrap();
}

#[tokio::test]
async fn run_chunk_nav_prev_hits_prev_endpoint() {
    let server = MockServer::start().await;
    let id = "22222222-2222-2222-2222-222222222202";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/prev")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let _ = run_chunk_nav(&json!({"id": id}), &client, ChunkNavDirection::Prev)
        .await
        .unwrap();
}

#[tokio::test]
async fn run_chunk_nav_rejects_count_zero() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_nav(
        &json!({"id": "22222222-2222-2222-2222-222222222203", "count": 0}),
        &client,
        ChunkNavDirection::Next,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_chunk_nav_rejects_count_over_max() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_nav(
        &json!({"id": "22222222-2222-2222-2222-222222222204", "count": 101}),
        &client,
        ChunkNavDirection::Next,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_chunk_nav_rejects_non_integer_count() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_nav(
        &json!({"id": "22222222-2222-2222-2222-222222222205", "count": "five"}),
        &client,
        ChunkNavDirection::Next,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_chunk_nav_rejects_invalid_uuid() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_nav(&json!({"id": "not-a-uuid"}), &client, ChunkNavDirection::Next)
        .await
        .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_document_chunks_sends_from_and_limit() {
    let server = MockServer::start().await;
    let id = "33333333-3333-3333-3333-333333333300";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .and(query_param("from", "3"))
        .and(query_param("limit", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunks": [], "from": 3, "limit": 7, "total_chunks": 0,
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_document_chunks(&json!({"id": id, "from": 3, "limit": 7}), &client)
        .await
        .unwrap();
    assert_eq!(v["from"], 3);
    assert_eq!(v["limit"], 7);
}

#[tokio::test]
async fn run_document_chunks_defaults_from_zero_limit_twenty() {
    let server = MockServer::start().await;
    let id = "33333333-3333-3333-3333-333333333301";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .and(query_param("from", "0"))
        .and(query_param("limit", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunks": [], "from": 0, "limit": 20, "total_chunks": 0,
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let _ = run_document_chunks(&json!({"id": id}), &client)
        .await
        .unwrap();
}

#[tokio::test]
async fn run_document_chunks_rejects_negative_from() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_document_chunks(
        &json!({"id": "33333333-3333-3333-3333-333333333302", "from": -1}),
        &client,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_document_chunks_rejects_limit_zero() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_document_chunks(
        &json!({"id": "33333333-3333-3333-3333-333333333303", "limit": 0}),
        &client,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_document_chunks_rejects_limit_over_max() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_document_chunks(
        &json!({"id": "33333333-3333-3333-3333-333333333304", "limit": 101}),
        &client,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_document_chunks_404_maps_to_not_found() {
    let server = MockServer::start().await;
    let id = "33333333-3333-3333-3333-333333333305";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_document_chunks(&json!({"id": id}), &client)
        .await
        .unwrap_err();
    assert!(matches!(err, PassthroughError::NotFound(_)));
}

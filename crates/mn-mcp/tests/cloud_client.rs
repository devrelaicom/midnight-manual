//! Integration tests for the MCP server's cloud HTTP layer.
//!
//! These exercise the `CloudClient` against a `wiremock` server playing the
//! cloud role. The full `run_search` happy path — which embeds locally — is
//! out of scope here because the embedder requires ~400 MB of ONNX files;
//! the cloud-side behavior we care about (request shape, 409 mismatch
//! detection, 404 mapping, pass-through) is testable independently and so
//! that's what we cover.

use mn_mcp::cloud_client::{CloudClient, CloudError, QueryPair, SearchRequest};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_search_req() -> SearchRequest {
    SearchRequest {
        queries: vec![QueryPair {
            text: "hello".into(),
            vector: vec![0.0_f32; 768],
        }],
        client_embedding_model: "bge-base-en-v1.5@1".into(),
        limit: 10,
        filters: None,
        sort_by: None,
    }
}

#[tokio::test]
async fn search_posts_request_and_returns_results() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .and(body_partial_json(json!({
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 10,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{
                "chunk_id": "00000000-0000-0000-0000-000000000001",
                "content": "doc one",
                "document_id": "00000000-0000-0000-0000-0000000000aa",
                "source_version_id": "00000000-0000-0000-0000-0000000000bb",
                "chunk_index": 0,
                "total_chunks": 1,
                "created_at": "2026-05-13T00:00:00Z",
                "scores": { "vector_similarity": 0.9 },
            }],
            "search_metadata": { "per_query": [], "total_candidates": 1 },
        })))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let resp = client.search(&make_search_req()).await.expect("ok");
    assert_eq!(resp["results"][0]["content"], "doc one");
    assert_eq!(resp["results"][0]["chunk_id"], "00000000-0000-0000-0000-000000000001");
}

#[tokio::test]
async fn search_surfaces_typed_embedding_model_mismatch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": {
                "code": "embedding_model_mismatch",
                "message": "client_embedding_model `bge-base-en-v1.5@1` does not match corpus model `bge-base-en-v1.5@2`",
                "remediation": "re-run `mnm models pull` to fetch the corpus model",
                "context": {
                    "corpus_model": "bge-base-en-v1.5@2",
                    "client_model": "bge-base-en-v1.5@1",
                },
            },
            "request_id": "req-abc",
        })))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.search(&make_search_req()).await.unwrap_err();
    match err {
        CloudError::EmbeddingModelMismatch {
            corpus_model,
            client_model,
            remediation,
            ..
        } => {
            assert_eq!(corpus_model, "bge-base-en-v1.5@2");
            assert_eq!(client_model, "bge-base-en-v1.5@1");
            assert!(remediation.contains("mnm models pull"));
        }
        other => panic!("expected EmbeddingModelMismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn search_falls_back_to_status_on_unrelated_409() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": { "code": "some_other_conflict", "message": "x", "remediation": "y" },
            "request_id": "r",
        })))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.search(&make_search_req()).await.unwrap_err();
    assert!(matches!(err, CloudError::Status { status: 409, .. }));
}

#[tokio::test]
async fn search_maps_500_to_status_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.search(&make_search_req()).await.unwrap_err();
    match err {
        CloudError::Status { status, body } => {
            assert_eq!(status, 500);
            assert!(body.contains("boom"));
        }
        other => panic!("expected Status, got {other:?}"),
    }
}

#[tokio::test]
async fn search_forwards_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .and(header("authorization", "Bearer abc.def.ghi"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [],
            "search_metadata": { "per_query": [], "total_candidates": 0 },
        })))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), Some("abc.def.ghi".into())).unwrap();
    let _ = client.search(&make_search_req()).await.expect("authorized");
}

#[tokio::test]
async fn get_chunk_round_trips() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-000000000007";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id,
            "content": "hi",
            "chunk_index": 0,
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_chunk(id).await.unwrap();
    assert_eq!(v["id"], id);
    assert_eq!(v["content"], "hi");
}

#[tokio::test]
async fn get_chunk_404_maps_to_not_found() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-000000000099";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}")))
        .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.get_chunk(id).await.unwrap_err();
    assert!(matches!(err, CloudError::NotFound(_)));
}

#[tokio::test]
async fn get_chunk_siblings_round_trips() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-000000000008";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/siblings")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "a", "chunk_index": 0},
            {"id": "b", "chunk_index": 1},
        ])))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_chunk_siblings(id).await.unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn get_chunk_parents_round_trips() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-000000000009";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/parents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "p1", "kind": "document"},
            {"id": "p2", "kind": "root"},
        ])))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_chunk_parents(id).await.unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["kind"], "document");
}

#[tokio::test]
async fn list_sources_round_trips() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"slug": "midnight-docs", "kind": "github", "display_name": "Midnight Docs"},
        ])))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.list_sources().await.unwrap();
    assert_eq!(v[0]["slug"], "midnight-docs");
}

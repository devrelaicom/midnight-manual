//! Integration tests for the MCP server's cloud HTTP layer.
//!
//! These exercise the `CloudClient` against a `wiremock` server playing the
//! cloud role. The full `run_search` happy path — which embeds locally — is
//! out of scope here because the embedder requires ~400 MB of ONNX files;
//! the cloud-side behavior we care about (request shape, 409 mismatch
//! detection, 404 mapping, pass-through) is testable independently and so
//! that's what we cover.

use mnm_mcp::cloud_client::{CloudClient, CloudError, QueryPair, SearchRequest};
use serde_json::json;
use wiremock::matchers::{
    body_partial_json, header, method, path, query_param, query_param_is_missing,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_search_req() -> SearchRequest {
    SearchRequest {
        queries: vec![QueryPair {
            text: "hello".into(),
            vector: vec![0.0_f32; 768],
            code_vector: Vec::new(),
        }],
        client_embedding_model: "bge-base-en-v1.5@1".into(),
        limit: 10,
        filters: None,
        sort_by: None,
        mode: None,
        code_mode: None,
        client_code_embedding_model: None,
        rerank: None,
        rerank_instructions: None,
        version_match: None,
    }
}

#[test]
fn search_request_omits_absent_code_fields_from_the_wire() {
    // Pre-dual-embeddings requests must serialize byte-identically: no
    // `code_mode`, no `client_code_embedding_model`, no per-pair `code_vector`.
    let v = serde_json::to_value(make_search_req()).unwrap();
    assert!(v.get("code_mode").is_none());
    assert!(v.get("client_code_embedding_model").is_none());
    assert!(v["queries"][0].get("code_vector").is_none());
    // The rerank knobs (added by the Voyage-reranking work) are likewise
    // `skip_serializing_if = Option::is_none`, so they too must be absent.
    assert!(v.get("rerank").is_none());
    assert!(v.get("rerank_instructions").is_none());
}

#[test]
fn search_request_serializes_code_fields_when_present() {
    let mut req = make_search_req();
    req.code_mode = Some("exclusive");
    req.client_code_embedding_model = Some("voyage-code-3@1".into());
    req.queries[0].code_vector = vec![0.5_f32; 4];
    let v = serde_json::to_value(req).unwrap();
    assert_eq!(v["code_mode"], "exclusive");
    assert_eq!(v["client_code_embedding_model"], "voyage-code-3@1");
    assert_eq!(v["queries"][0]["code_vector"].as_array().unwrap().len(), 4);
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
                "remediation": "run `mnm models active` to see the corpus's active model",
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
            assert!(remediation.contains("mnm models active"));
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
async fn get_chunk_parents_round_trips() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-000000000009";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/parents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "parents": [
                {"id": "p1", "kind": "document", "document_id": "d1"},
                {"id": "p2", "kind": "root", "document_id": null},
            ],
            "source": {"slug": "s", "display_name": "S"},
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_chunk_parents(id).await.unwrap();
    assert_eq!(v["parents"].as_array().unwrap().len(), 2);
    assert_eq!(v["parents"][0]["kind"], "document");
}

#[tokio::test]
async fn list_sources_round_trips() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources"))
        .and(query_param_is_missing("cursor"))
        .and(query_param_is_missing("limit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sources": [
                {"slug": "midnight-docs", "kind": "docs_site", "display_name": "Midnight Docs"},
            ],
            "total": 1,
            "next_cursor": null,
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.list_sources(&[]).await.unwrap();
    assert_eq!(v["sources"][0]["slug"], "midnight-docs");
}

#[tokio::test]
async fn list_sources_sends_pagination_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources"))
        .and(query_param("cursor", "abc=="))
        .and(query_param("limit", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sources": [{"slug": "midnight-docs"}],
            "next_cursor": null,
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client
        .list_sources(&[("cursor", "abc==".to_owned()), ("limit", "20".to_owned())])
        .await
        .unwrap();
    assert_eq!(v["sources"][0]["slug"], "midnight-docs");
}

#[tokio::test]
async fn get_chunks_sends_comma_joined_ids() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/chunks"))
        .and(query_param("ids", "a,b"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunks": [{"id": "a"}, {"id": "b"}],
            "missing": [],
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client
        .get_chunks(&["a".to_owned(), "b".to_owned()])
        .await
        .unwrap();
    assert_eq!(v["chunks"].as_array().unwrap().len(), 2);
    assert_eq!(v["chunks"][1]["id"], "b");
    assert_eq!(v["missing"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn get_facets_sends_drilldown_param() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/facets"))
        .and(query_param("facet", "tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "facet": "tags",
            "values": [{"value": "zk", "count": 12}],
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client
        .get_facets(&[("facet", "tags".to_owned())])
        .await
        .unwrap();
    assert_eq!(v["facet"], "tags");
    assert_eq!(v["values"][0]["value"], "zk");
}

#[tokio::test]
async fn get_me_round_trips() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user": "aaron",
            "rate_limit": {"remaining": 99},
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_me().await.unwrap();
    assert_eq!(v["user"], "aaron");
    assert_eq!(v["rate_limit"]["remaining"], 99);
}

#[tokio::test]
async fn readyz_returns_200_as_data() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/readyz"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    assert_eq!(client.readyz().await.unwrap(), 200);
}

#[tokio::test]
async fn readyz_returns_503_as_data_not_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/readyz"))
        .respond_with(ResponseTemplate::new(503).set_body_string("draining"))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    assert_eq!(client.readyz().await.unwrap(), 503);
}

#[tokio::test]
async fn readyz_maps_refused_connection_to_transport() {
    // Bind to an OS-assigned port, then drop the listener so the port is dead.
    let dead_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let client = CloudClient::new(&format!("http://127.0.0.1:{dead_port}"), None).unwrap();
    let err = client.readyz().await.unwrap_err();
    assert!(matches!(err, CloudError::Transport(_)));
}

#[tokio::test]
async fn get_chunk_next_sends_count_query() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000000a";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .and(query_param("count", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunks": [{"id": "x", "chunk_index": 4}],
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_chunk_next(id, 7).await.unwrap();
    assert_eq!(v["chunks"][0]["chunk_index"], 4);
}

#[tokio::test]
async fn get_chunk_prev_sends_count_query() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000000b";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/prev")))
        .and(query_param("count", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunks": [{"id": "y", "chunk_index": 0}],
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_chunk_prev(id, 3).await.unwrap();
    assert_eq!(v["chunks"][0]["chunk_index"], 0);
}

#[tokio::test]
async fn get_chunk_next_maps_404_to_not_found() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000000c";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.get_chunk_next(id, 5).await.unwrap_err();
    assert!(matches!(err, CloudError::NotFound(_)));
}

#[tokio::test]
async fn get_document_round_trips() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000000d";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id,
            "source_path": "welcome.md",
            "chunks": [
                {"id": "a", "chunk_index": 0, "token_count": 10},
                {"id": "b", "chunk_index": 1, "token_count": 20},
            ],
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_document(id).await.unwrap();
    assert_eq!(v["source_path"], "welcome.md");
    assert_eq!(v["chunks"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn get_document_404_maps_to_not_found() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000001b";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}")))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.get_document(id).await.unwrap_err();
    assert!(matches!(err, CloudError::NotFound(_)));
}

#[tokio::test]
async fn get_document_chunks_sends_from_and_limit() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000001c";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .and(query_param("from", "5"))
        .and(query_param("limit", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunks": [],
            "from": 5,
            "limit": 20,
            "total_chunks": 5,
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_document_chunks(id, 5, 20).await.unwrap();
    assert_eq!(v["from"], 5);
    assert_eq!(v["total_chunks"], 5);
}

#[tokio::test]
async fn get_document_chunks_404_maps_to_not_found() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000001d";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.get_document_chunks(id, 0, 20).await.unwrap_err();
    assert!(matches!(err, CloudError::NotFound(_)));
}

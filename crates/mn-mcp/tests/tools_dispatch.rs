//! Integration tests for the non-search tool dispatch paths. These thread
//! the `run_passthrough_id` helper through a wiremock cloud and verify the
//! input-validation gates (uuid parsing, missing fields) before the wire
//! call goes out.
//!
//! The lower section ("projector dispatch integration") tests that the full
//! tool→projector→`ToolCallResult` pipeline produces the correct
//! `structuredContent` shape, matching the render.rs contracts.

use std::sync::Arc;

use mn_mcp::cloud_client::CloudClient;
use mn_mcp::tools::{
    run_chunk_nav, run_chunk_neighbors, run_document_chunks, run_passthrough_id, ChunkNavDirection,
    PassthroughError, PassthroughKind,
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
async fn run_chunk_neighbors_bundles_three_endpoints() {
    let server = MockServer::start().await;
    let id = "44444444-4444-4444-4444-444444444400";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/prev")))
        .and(query_param("count", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": [
            {"id": "prev-1", "content": "p1"},
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id, "content": "anchor",
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .and(query_param("count", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": [
            {"id": "next-1", "content": "n1"},
            {"id": "next-2", "content": "n2"},
        ]})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_chunk_neighbors(&json!({"id": id}), &client)
        .await
        .unwrap();
    assert_eq!(v["chunk"]["id"], id);
    assert_eq!(v["chunk"]["content"], "anchor");
    assert_eq!(v["prev"]["chunks"].as_array().unwrap().len(), 1);
    assert_eq!(v["next"]["chunks"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn run_chunk_neighbors_honours_count() {
    let server = MockServer::start().await;
    let id = "44444444-4444-4444-4444-444444444401";
    // The handler applies `count` symmetrically: prev=next=4. Each mock asserts
    // the query param explicitly, so a regression (e.g. desync between
    // prev/next) would fail this test rather than silently passing.
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/prev")))
        .and(query_param("count", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .and(query_param("count", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let _ = run_chunk_neighbors(&json!({"id": id, "count": 4}), &client)
        .await
        .unwrap();
}

#[tokio::test]
async fn run_chunk_neighbors_returns_empty_edges_on_first_or_last() {
    let server = MockServer::start().await;
    let id = "44444444-4444-4444-4444-444444444402";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/prev")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_chunk_neighbors(&json!({"id": id}), &client)
        .await
        .unwrap();
    assert!(v["prev"]["chunks"].as_array().unwrap().is_empty());
    assert!(v["next"]["chunks"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn run_chunk_neighbors_anchor_404_maps_to_not_found() {
    let server = MockServer::start().await;
    let id = "44444444-4444-4444-4444-444444444403";
    // The anchor 404s. With try_join! the other legs may or may not race
    // back; either way the call surfaces NotFound.
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/prev")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}")))
        .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_neighbors(&json!({"id": id}), &client)
        .await
        .unwrap_err();
    assert!(matches!(err, PassthroughError::NotFound(_)));
}

#[tokio::test]
async fn run_chunk_neighbors_rejects_invalid_uuid() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_neighbors(&json!({"id": "not-a-uuid"}), &client)
        .await
        .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_chunk_neighbors_rejects_count_zero() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_neighbors(
        &json!({"id": "44444444-4444-4444-4444-444444444404", "count": 0}),
        &client,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_chunk_neighbors_rejects_count_over_max() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_neighbors(
        &json!({"id": "44444444-4444-4444-4444-444444444405", "count": 101}),
        &client,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_chunk_neighbors_rejects_non_integer_count() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_neighbors(
        &json!({"id": "44444444-4444-4444-4444-444444444406", "count": "two"}),
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

// ---------------------------------------------------------------------------
// Projector dispatch integration tests
//
// These tests wire raw tool output through the render projectors and verify
// that the resulting `ToolCallResult` carries the correct `structuredContent`
// shape. They exercise the same pipeline that `run_passthrough_tool` and
// `run_search_dispatch` in server.rs follow.
// ---------------------------------------------------------------------------

/// `get_chunk` → `project_chunk`: structuredContent has top-level `id` (not
/// nested under a `chunk` key) because chunk fields are flattened.
#[tokio::test]
async fn dispatch_get_chunk_structured_content_has_top_level_id() {
    use mn_mcp::render;
    let id = "55555555-5555-5555-5555-555555555500";
    let raw = json!({
        "id": id,
        "chunk_index": 0,
        "total_chunks": 10,
        "content": "body",
        "heading_path": [],
        "document": { "source_path": "docs/intro.md" },
        "source": { "slug": "compact-docs" }
    });
    let result = render::project_chunk(raw).into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    // Chunk fields are flattened — `id` is top-level, NOT `sc["chunk"]["id"]`.
    assert_eq!(sc["id"], id);
    // next_actions injected by the projector.
    assert!(sc["next_actions"].is_array());
    // text block has a fenced JSON summary, not the raw JSON dump.
    let text = match &result.content[0] {
        mn_mcp::protocol::ContentBlock::Text { text } => text.clone(),
    };
    assert!(text.contains("```json"), "text block must contain a fenced json block");
}

/// `get_chunk_next` → `project_chunk_list("after")`: structuredContent has
/// a `chunks` array.
#[tokio::test]
async fn dispatch_get_chunk_next_structured_content_has_chunks_array() {
    use mn_mcp::render;
    let raw = json!({ "chunks": [{"id": "a"}, {"id": "b"}] });
    let result = render::project_chunk_list(raw, "after").into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    assert!(sc["chunks"].is_array(), "structuredContent must have a chunks array");
    assert_eq!(sc["chunks"].as_array().unwrap().len(), 2);
    // isError omitted on success.
    assert!(!result.is_error);
}

/// `get_document_chunks` → `project_document_window`: structuredContent has
/// top-level `from` and the `chunks` array.
#[tokio::test]
async fn dispatch_get_document_chunks_structured_content_has_from_and_limit() {
    use mn_mcp::render;
    let raw = json!({
        "id": "d1", "source_path": "docs/intro.md", "source": { "display_name": "X" },
        "from": 3, "limit": 7, "total_chunks": 35,
        "chunks": [{"chunk_id": "a"}, {"chunk_id": "b"}]
    });
    let result = render::project_document_window(raw).into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    // DocumentChunkWindow is flattened — from/limit are top-level.
    assert_eq!(sc["from"], 3);
    assert_eq!(sc["limit"], 7);
    assert!(sc["chunks"].is_array());
}

/// `get_chunk_neighbors` → `project_neighbors`: structuredContent keeps the
/// `{prev, chunk, next}` shape — `sc["chunk"]["id"]` is where the anchor id lives.
#[tokio::test]
async fn dispatch_get_chunk_neighbors_structured_content_shape() {
    use mn_mcp::render;
    let id = "55555555-5555-5555-5555-555555555501";
    let raw = json!({
        "prev": { "chunks": [{"id": "p1"}] },
        "chunk": { "id": id, "content": "anchor" },
        "next": { "chunks": [{"id": "n1"}, {"id": "n2"}] }
    });
    let result = render::project_neighbors(raw).into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    // Neighbors keeps nested shape: sc["chunk"]["id"]
    assert_eq!(sc["chunk"]["id"], id);
    // prev / next arrays accessible at sc["prev"]["chunks"] / sc["next"]["chunks"]
    assert_eq!(sc["prev"]["chunks"].as_array().unwrap().len(), 1);
    assert_eq!(sc["next"]["chunks"].as_array().unwrap().len(), 2);
    assert!(sc["next_actions"].is_array());
}

/// Tool-execution errors become `isError: true` results (not JSON-RPC errors).
/// Verify `passthrough_failure` on `NotFound` produces the right envelope.
#[tokio::test]
async fn dispatch_passthrough_not_found_produces_iserror_envelope() {
    use mn_mcp::render;
    // Simulate what run_passthrough_tool does on a NotFound error.
    let failure = render::ToolFailure {
        kind: render::ErrorKind::NotFound,
        message: "not found: no chunk abc".into(),
        guidance: "Not found — verify the id from a recent search result.".into(),
        details: json!({}),
        next_actions: vec![render::NextAction {
            tool: "search",
            arguments: json!({ "query": "<terms>" }),
        }],
    };
    let result = failure.into_result();
    assert!(result.is_error, "isError must be true for tool-execution errors");
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["error"]["code"], "NOT_FOUND");
    assert_eq!(sc["error"]["retryable"], false);
    // next_actions still present so the agent can recover.
    assert!(sc["next_actions"].is_array());
}

/// `get_document_full` 412 `too_many_chunks` → `isError: true` envelope with
/// `chunk_count`, `cap`, and a `get_document_chunks` next action.
#[tokio::test]
async fn dispatch_document_full_too_many_chunks_produces_iserror_envelope() {
    use mn_mcp::render;
    let failure = render::ToolFailure {
        kind: render::ErrorKind::TooManyChunks,
        message: "document has 1240 chunks (cap 500)".into(),
        guidance: "Use get_document_chunks to page through the document.".into(),
        details: json!({ "chunk_count": 1240, "cap": 500, "hint": "use /chunks endpoint" }),
        next_actions: vec![render::NextAction {
            tool: "get_document_chunks",
            arguments: json!({ "from": 0, "limit": 20 }),
        }],
    };
    let result = failure.into_result();
    assert!(result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["error"]["code"], "TOO_MANY_CHUNKS");
    // Details merged into the error object.
    assert_eq!(sc["error"]["chunk_count"], 1240);
    assert_eq!(sc["error"]["cap"], 500);
    // next_actions points at get_document_chunks.
    assert_eq!(sc["next_actions"][0]["tool"], "get_document_chunks");
}

/// Verify that a successful `get_chunk` round-trip through the full
/// tool→projector pipeline (with a wiremock cloud) produces:
/// 1. `isError` absent (false)
/// 2. `structuredContent["id"]` == the chunk id (flattened, top-level)
/// 3. `content[0].text` contains a fenced json block (not the raw dump)
#[tokio::test]
async fn dispatch_get_chunk_full_pipeline_via_wiremock() {
    use mn_mcp::render;
    let server = MockServer::start().await;
    let id = "55555555-5555-5555-5555-555555555502";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id, "chunk_index": 2, "total_chunks": 20, "content": "body",
            "heading_path": ["Intro"], "document": {"source_path": "x.md"},
            "source": {"slug": "compact-docs"}
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::Chunk)
        .await
        .unwrap();
    // Thread through the projector (same as run_passthrough_tool does).
    let result = render::project_chunk(v).into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["id"], id);
    let text = match &result.content[0] {
        mn_mcp::protocol::ContentBlock::Text { text } => text.clone(),
    };
    assert!(text.contains("```json"), "text block must contain a fenced json summary");
    // The text block is a summary string, not the raw JSON dump of the whole chunk.
    assert!(!text.starts_with('{'), "text block must not be a raw JSON dump");
}

/// Verify that `get_document_chunks` full pipeline produces correct `from`/`limit`
/// in structuredContent (the window fields are preserved top-level).
#[tokio::test]
async fn dispatch_get_document_chunks_full_pipeline_via_wiremock() {
    use mn_mcp::render;
    let server = MockServer::start().await;
    let id = "55555555-5555-5555-5555-555555555503";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .and(query_param("from", "3"))
        .and(query_param("limit", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id, "source_path": "docs/intro.md", "source": {"display_name": "X"},
            "chunks": [], "from": 3, "limit": 7, "total_chunks": 35,
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_document_chunks(&json!({"id": id, "from": 3, "limit": 7}), &client)
        .await
        .unwrap();
    let result = render::project_document_window(v).into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["from"], 3);
    assert_eq!(sc["limit"], 7);
}

/// Verify that `get_chunk_neighbors` full pipeline preserves `sc["prev"]["chunks"]`
/// and `sc["next"]["chunks"]` length.
#[tokio::test]
async fn dispatch_get_chunk_neighbors_full_pipeline_via_wiremock() {
    use mn_mcp::render;
    let server = MockServer::start().await;
    let id = "55555555-5555-5555-5555-555555555504";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/prev")))
        .and(query_param("count", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "chunks": [{"id": "p1"}] })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": id })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .and(query_param("count", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "chunks": [{"id": "n1"}, {"id": "n2"}] })),
        )
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_chunk_neighbors(&json!({"id": id}), &client).await.unwrap();
    let result = render::project_neighbors(v).into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["chunk"]["id"], id);
    assert_eq!(sc["prev"]["chunks"].as_array().unwrap().len(), 1);
    assert_eq!(sc["next"]["chunks"].as_array().unwrap().len(), 2);
}

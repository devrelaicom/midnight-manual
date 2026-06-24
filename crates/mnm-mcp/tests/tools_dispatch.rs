//! Integration tests for the non-search tool dispatch paths. These thread
//! the `run_passthrough_id` helper through a wiremock cloud and verify the
//! input-validation gates (uuid parsing, missing fields) before the wire
//! call goes out.
//!
//! The lower section ("projector dispatch integration") tests that the full
//! tool→projector→`ToolCallResult` pipeline produces the correct
//! `structuredContent` shape, matching the render.rs contracts.

use std::sync::Arc;

use mnm_mcp::cloud_client::CloudClient;
use mnm_mcp::tools::{
    run_chunk_nav, run_chunk_neighbors, run_document_chunks, run_facets, run_get_chunks,
    run_list_sources, run_passthrough_id, ChunkNavDirection, PassthroughError, PassthroughKind,
};
use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn run_passthrough_id_rejects_missing_id() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_passthrough_id(&json!({}), &client, PassthroughKind::Parents)
        .await
        .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_passthrough_id_rejects_non_uuid() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_passthrough_id(&json!({"id": "not-a-uuid"}), &client, PassthroughKind::Parents)
        .await
        .unwrap_err();
    match err {
        PassthroughError::InvalidInput(msg) => assert!(msg.contains("UUID")),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[tokio::test]
async fn run_passthrough_id_hits_parents_endpoint() {
    let server = MockServer::start().await;
    let id = "11111111-1111-1111-1111-111111111111";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/parents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "parents": [{ "id": "p1", "kind": "root", "name": "Root", "document_id": null }],
            "source": { "slug": "s", "display_name": "S" }
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::Parents)
        .await
        .unwrap();
    assert_eq!(v["parents"][0]["name"], "Root");
}

// ---------------------------------------------------------------------------
// run_get_chunks (batch fetch)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_get_chunks_happy_path_two_chunks() {
    let server = MockServer::start().await;
    let id_a = "66666666-6666-6666-6666-666666666600";
    let id_b = "66666666-6666-6666-6666-666666666601";
    Mock::given(method("GET"))
        .and(path("/v1/chunks"))
        .and(query_param("ids", format!("{id_a},{id_b}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunks": [
                { "id": id_a, "content": "alpha", "document": { "source_path": "docs/a.md" } },
                { "id": id_b, "content": "beta", "document": { "source_path": "docs/b.md" } }
            ],
            "missing": []
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_get_chunks(&json!({"ids": [id_a, id_b]}), &client)
        .await
        .unwrap();
    assert_eq!(v["chunks"].as_array().unwrap().len(), 2);
    assert_eq!(v["chunks"][0]["id"], id_a);
    assert!(v["missing"].as_array().unwrap().is_empty());
}

/// 0 ids → `InvalidInput`, surfaced through the same `isError` envelope the
/// dispatcher builds from `PassthroughError::InvalidInput`.
#[tokio::test]
async fn run_get_chunks_rejects_empty_ids_as_iserror_invalid_input() {
    use mnm_mcp::render::{ErrorKind, ToolFailure};
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_get_chunks(&json!({"ids": []}), &client)
        .await
        .unwrap_err();
    let msg = match err {
        PassthroughError::InvalidInput(msg) => msg,
        other => panic!("expected InvalidInput, got {other:?}"),
    };
    // Mirror passthrough_failure's InvalidInput mapping in server.rs.
    let result = ToolFailure::simple(ErrorKind::InvalidInput, msg.clone(), msg).into_result();
    assert!(result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["error"]["code"], "INVALID_INPUT");
}

#[tokio::test]
async fn run_get_chunks_rejects_twenty_one_ids() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let ids: Vec<String> = (0..21)
        .map(|i| format!("66666666-6666-6666-6666-6666666666{i:02}"))
        .collect();
    let err = run_get_chunks(&json!({ "ids": ids }), &client)
        .await
        .unwrap_err();
    match err {
        PassthroughError::InvalidInput(msg) => {
            assert!(msg.contains("at most 20"), "message must state the cap: {msg}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[tokio::test]
async fn run_get_chunks_accepts_exactly_twenty_ids() {
    let server = MockServer::start().await;
    let ids: Vec<String> = (0..20)
        .map(|i| format!("66666666-6666-6666-6666-6666666666{i:02}"))
        .collect();
    Mock::given(method("GET"))
        .and(path("/v1/chunks"))
        .and(query_param("ids", ids.join(",")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "chunks": [], "missing": ids })),
        )
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_get_chunks(&json!({ "ids": ids }), &client)
        .await
        .unwrap();
    assert_eq!(v["missing"].as_array().unwrap().len(), 20);
}

#[tokio::test]
async fn run_get_chunks_rejects_non_string_entry() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_get_chunks(&json!({"ids": ["66666666-6666-6666-6666-666666666600", 7]}), &client)
        .await
        .unwrap_err();
    match err {
        PassthroughError::InvalidInput(msg) => {
            assert!(msg.contains("ids[1]"), "message must locate the bad entry: {msg}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[tokio::test]
async fn run_get_chunks_rejects_invalid_uuid() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_get_chunks(&json!({"ids": ["not-a-uuid"]}), &client)
        .await
        .unwrap_err();
    match err {
        PassthroughError::InvalidInput(msg) => assert!(msg.contains("UUID")),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[tokio::test]
async fn run_get_chunks_rejects_missing_or_non_array_ids() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    for args in [
        json!({}),
        json!({"ids": "66666666-6666-6666-6666-666666666600"}),
    ] {
        let err = run_get_chunks(&args, &client).await.unwrap_err();
        assert!(matches!(err, PassthroughError::InvalidInput(_)), "args: {args}");
    }
}

#[tokio::test]
async fn run_passthrough_id_maps_404() {
    let server = MockServer::start().await;
    let id = "22222222-2222-2222-2222-222222222222";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}")))
        .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::Document)
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
// run_list_sources (pagination/filter forwarding)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_list_sources_forwards_present_args_as_query_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources"))
        .and(query_param("limit", "5"))
        .and(query_param("kind", "docs_site"))
        .and(query_param_is_missing("cursor"))
        .and(query_param_is_missing("retired"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sources": [{ "id": "u1", "slug": "midnight-docs", "display_name": "Midnight Docs",
                          "kind": "docs_site" }],
            "total": 1,
            "next_cursor": null,
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    // JSON integer 5 must arrive as the query string "5" (not e.g. "5.0").
    let v = run_list_sources(&json!({"limit": 5, "kind": "docs_site"}), &client)
        .await
        .unwrap();
    assert_eq!(v["sources"][0]["slug"], "midnight-docs");
}

#[tokio::test]
async fn run_list_sources_no_args_sends_no_query_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources"))
        .and(query_param_is_missing("cursor"))
        .and(query_param_is_missing("limit"))
        .and(query_param_is_missing("created_after"))
        .and(query_param_is_missing("created_before"))
        .and(query_param_is_missing("kind"))
        .and(query_param_is_missing("retired"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sources": [],
            "total": 0,
            "next_cursor": null,
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_list_sources(&json!({}), &client).await.unwrap();
    assert_eq!(v["total"], 0);
}

#[tokio::test]
async fn run_list_sources_renders_bool_arg_as_query_string() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sources"))
        .and(query_param("retired", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sources": [],
            "total": 0,
            "next_cursor": null,
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_list_sources(&json!({"retired": true}), &client)
        .await
        .unwrap();
    assert_eq!(v["total"], 0);
}

// ---------------------------------------------------------------------------
// run_facets (drill-down param forwarding)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_facets_forwards_present_args_as_query_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/facets"))
        .and(query_param("facet", "tags"))
        .and(query_param("limit", "10"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "facet": "tags",
            "values": ["zk", "proofs"],
            "total": 312,
            "next_cursor": "tok==",
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    // JSON integer 10 must arrive as the query string "10" (not e.g. "10.0").
    let v = run_facets(&json!({"facet": "tags", "limit": 10}), &client)
        .await
        .unwrap();
    assert_eq!(v["facet"], "tags");
    assert_eq!(v["values"][0], "zk");
}

#[tokio::test]
async fn run_facets_no_args_sends_no_query_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/facets"))
        .and(query_param_is_missing("facet"))
        .and(query_param_is_missing("cursor"))
        .and(query_param_is_missing("limit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "modes": ["hybrid", "vector", "fts"],
            "filters": [
                { "key": "source_slug", "type": "open_set", "negatable": true,
                  "values": ["compact-docs"], "truncated": true, "total": 43 },
            ],
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_facets(&json!({}), &client).await.unwrap();
    assert_eq!(v["filters"][0]["key"], "source_slug");
}

// ---------------------------------------------------------------------------
// Projector dispatch integration tests
//
// These tests wire raw tool output through the render projectors and verify
// that the resulting `ToolCallResult` carries the correct `structuredContent`
// shape. They exercise the same pipeline that `run_passthrough_tool` and
// `run_search_dispatch` in server.rs follow.
// ---------------------------------------------------------------------------

/// `get_chunks` → `project_chunks`: structuredContent keeps the cloud's
/// `{chunks, missing}` envelope, and a single-chunk fetch carries the FULL
/// chunk content in the text fence (legacy text-only clients).
#[tokio::test]
async fn dispatch_get_chunks_single_structured_content_and_full_content_fence() {
    use mnm_mcp::render;
    let id = "55555555-5555-5555-5555-555555555500";
    let body = "b".repeat(400); // longer than the 150-char snippet cut
    let raw = json!({
        "chunks": [{
            "id": id,
            "chunk_index": 0,
            "total_chunks": 10,
            "content": body,
            "heading_path": [],
            "document": { "source_path": "docs/intro.md" },
            "source": { "slug": "compact-docs" }
        }],
        "missing": []
    });
    let result =
        render::project_chunks(raw, mnm_core::injection::SecurityLevel::Disabled).into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["chunks"][0]["id"], id);
    // suggested_next_actions injected by the projector.
    assert!(sc["suggested_next_actions"].is_array());
    // text block has a fenced JSON summary carrying the FULL content string.
    let text = match &result.content[0] {
        mnm_mcp::protocol::ContentBlock::Text { text } => text.clone(),
    };
    assert!(text.contains("```json"), "text block must contain a fenced json block");
    assert!(
        text.contains(&body),
        "single-chunk fetch must put the full content in the text fence"
    );
}

/// `get_chunks` with multiple chunks: the fence carries per-chunk snippets,
/// never the full bodies.
#[tokio::test]
async fn dispatch_get_chunks_multi_fence_uses_snippets() {
    use mnm_mcp::render;
    let body = "c".repeat(400);
    let raw = json!({
        "chunks": [
            { "id": "a", "content": body, "document": { "source_path": "docs/a.md" } },
            { "id": "b", "content": "tiny", "document": { "source_path": "docs/b.md" } }
        ],
        "missing": []
    });
    let result =
        render::project_chunks(raw, mnm_core::injection::SecurityLevel::Disabled).into_result();
    assert!(!result.is_error);
    let text = match &result.content[0] {
        mnm_mcp::protocol::ContentBlock::Text { text } => text.clone(),
    };
    assert!(
        !text.contains(&body),
        "multi-chunk fetch must not dump full bodies into the text fence"
    );
    assert!(text.contains("snippet"), "multi-chunk fence carries chunk briefs with snippets");
}

/// `get_chunk_next` → `project_chunk_list("after")`: structuredContent has
/// a `chunks` array.
#[tokio::test]
async fn dispatch_get_chunk_next_structured_content_has_chunks_array() {
    use mnm_mcp::render;
    let raw = json!({ "chunks": [{"id": "a"}, {"id": "b"}] });
    let result =
        render::project_chunk_list(raw, "after", mnm_core::injection::SecurityLevel::Disabled)
            .into_result();
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
    use mnm_mcp::render;
    let raw = json!({
        "id": "d1", "source_path": "docs/intro.md", "source": { "display_name": "X" },
        "from": 3, "limit": 7, "total_chunks": 35,
        "chunks": [{"chunk_id": "a"}, {"chunk_id": "b"}]
    });
    let result = render::project_document_window(raw, mnm_core::injection::SecurityLevel::Disabled)
        .into_result();
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
    use mnm_mcp::render;
    let id = "55555555-5555-5555-5555-555555555501";
    let raw = json!({
        "prev": { "chunks": [{"id": "p1"}] },
        "chunk": { "id": id, "content": "anchor" },
        "next": { "chunks": [{"id": "n1"}, {"id": "n2"}] }
    });
    let result =
        render::project_neighbors(raw, mnm_core::injection::SecurityLevel::Disabled).into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    // Neighbors keeps nested shape: sc["chunk"]["id"]
    assert_eq!(sc["chunk"]["id"], id);
    // prev / next arrays accessible at sc["prev"]["chunks"] / sc["next"]["chunks"]
    assert_eq!(sc["prev"]["chunks"].as_array().unwrap().len(), 1);
    assert_eq!(sc["next"]["chunks"].as_array().unwrap().len(), 2);
    assert!(sc["suggested_next_actions"].is_array());
}

/// Tool-execution errors become `isError: true` results (not JSON-RPC errors).
/// Verify `passthrough_failure` on `NotFound` produces the right envelope.
#[tokio::test]
async fn dispatch_passthrough_not_found_produces_iserror_envelope() {
    use mnm_mcp::render;
    // Simulate what run_passthrough_tool does on a NotFound error.
    let failure = render::ToolFailure {
        kind: render::ErrorKind::NotFound,
        message: "not found: no chunk abc".into(),
        guidance: "Not found — verify the id from a recent search result.".into(),
        details: json!({}),
        suggested_next_actions: vec![render::NextAction::call(
            "Run a fresh search to find a valid id",
            "search",
            json!({ "query": "<terms>" }),
        )],
    };
    let result = failure.into_result();
    assert!(result.is_error, "isError must be true for tool-execution errors");
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["error"]["code"], "NOT_FOUND");
    assert_eq!(sc["error"]["retryable"], false);
    // suggested_next_actions still present so the agent can recover.
    assert!(sc["suggested_next_actions"].is_array());
}

/// Verify that a successful `get_chunks` round-trip through the full
/// tool→projector pipeline (with a wiremock cloud) produces:
/// 1. `isError` absent (false)
/// 2. `structuredContent["chunks"][0]["id"]` == the chunk id
/// 3. `content[0].text` contains a fenced json block (not the raw dump)
#[tokio::test]
async fn dispatch_get_chunks_full_pipeline_via_wiremock() {
    use mnm_mcp::render;
    let server = MockServer::start().await;
    let id = "55555555-5555-5555-5555-555555555502";
    Mock::given(method("GET"))
        .and(path("/v1/chunks"))
        .and(query_param("ids", id))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunks": [{
                "id": id, "chunk_index": 2, "total_chunks": 20, "content": "body",
                "heading_path": ["Intro"], "document": {"source_path": "x.md"},
                "source": {"slug": "compact-docs"}
            }],
            "missing": []
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_get_chunks(&json!({"ids": [id]}), &client)
        .await
        .unwrap();
    // Thread through the projector (same as run_passthrough_tool does).
    let result =
        render::project_chunks(v, mnm_core::injection::SecurityLevel::Disabled).into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["chunks"][0]["id"], id);
    let text = match &result.content[0] {
        mnm_mcp::protocol::ContentBlock::Text { text } => text.clone(),
    };
    assert!(text.contains("```json"), "text block must contain a fenced json summary");
    // The text block is a summary string, not the raw JSON dump of the envelope.
    assert!(!text.starts_with('{'), "text block must not be a raw JSON dump");
}

/// Verify that `get_document_chunks` full pipeline produces correct `from`/`limit`
/// in structuredContent (the window fields are preserved top-level).
#[tokio::test]
async fn dispatch_get_document_chunks_full_pipeline_via_wiremock() {
    use mnm_mcp::render;
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
    let result = render::project_document_window(v, mnm_core::injection::SecurityLevel::Disabled)
        .into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["from"], 3);
    assert_eq!(sc["limit"], 7);
}

/// Search that returns a 409 embedding-model mismatch from the cloud surfaces
/// as `isError: true` with `EMBEDDING_MODEL_MISMATCH`, `retryable: false`, a
/// `corpus_model` field, and no suggested next actions (the cloud-provided
/// `remediation` string is the next step; there is no client-side tool that
/// fixes a corpus-side mismatch).
///
/// Approach: run `run_search` through a full wiremock stack (models/active +
/// embeddings server-proxy + search→409) to obtain `SearchError::Mismatch`,
/// then thread it through the same render pipeline `run_search_dispatch` uses
/// in server.rs. Tests both the error classification (I2) and the dispatch
/// path (I3 requirement).
///
/// NOTE: when `VOYAGE_API_KEY` is set the embed step goes to Voyage directly
/// (not through the wiremock `/v1/embeddings`). The test clears the key for
/// the call via the `mnm_core::config::StdEnv` path — but since we cannot
/// unset process env vars safely across threads, we instead supply a mock
/// `/v1/embeddings` endpoint that `run_search` will use if the key is absent
/// (sandbox always has `VOYAGE_API_KEY=` cleared before mnm-mcp cargo commands
/// per `sandbox-voyage-api-key.md`). The assertion on `SearchError::Mismatch`
/// is independent of whether embed was BYOK or server-proxy, because the 409
/// comes from `/v1/search` in either case.
#[tokio::test]
async fn dispatch_search_mismatch_produces_iserror_envelope() {
    use mnm_mcp::render::{ErrorKind, ToolFailure};
    use mnm_mcp::server::ServerConfig;
    use mnm_mcp::tools::{run_search, SearchError};

    let server = MockServer::start().await;

    // /v1/models/active — returns the corpus's active wire id.
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "voyage-code-3",
            "revision": 2,
            "dim": 4,
            "provider": "voyageai",
        })))
        .mount(&server)
        .await;

    // /v1/embeddings — server-proxy path used when VOYAGE_API_KEY is absent.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-code-3@2",
            "embeddings": [[0.1_f32, 0.2, 0.3, 0.4]],
            "usage": { "total_tokens": 4 },
        })))
        .mount(&server)
        .await;

    // /v1/search — returns 409 mismatch regardless of body.
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": {
                "code": "embedding_model_mismatch",
                "message": "client model `voyage-code-3@2` does not match corpus model `voyage-code-3@1`",
                "remediation": "re-run the search; the client embeds with the corpus's live active model",
                "context": {
                    "corpus_model": "voyage-code-3@1",
                    "client_model": "voyage-code-3@2",
                },
            },
            "request_id": "test-req-001",
        })))
        .mount(&server)
        .await;

    let mut cfg = ServerConfig::with_defaults(std::path::PathBuf::from("/tmp/test-mcp-cache"));
    cfg.cloud_url.clone_from(&server.uri());
    server.uri().clone_into(&mut cfg.telemetry_endpoint);

    let cloud = Arc::new(CloudClient::new(&server.uri(), None).unwrap());

    let parsed = mnm_mcp::tools::ParsedSearchArgs {
        queries: vec!["compact contract".to_owned()],
        limit: 10,
        rerank: false,
        filters: None,
        mode: "fts",
        code_mode: None,
        rerank_instructions: None,
        version_match: None,
    };
    let err = run_search(&parsed, &cfg, &cloud).await.unwrap_err();

    // Extract mismatch fields — same as run_search_dispatch in server.rs.
    let (corpus_model, message, remediation) = match err {
        SearchError::Mismatch {
            corpus_model,
            message,
            remediation,
            ..
        } => (corpus_model, message, remediation),
        other @ SearchError::Cloud(_) => {
            panic!("expected SearchError::Mismatch, got {other:?}")
        }
    };

    // Mirror the render pipeline that run_search_dispatch uses.
    let failure = ToolFailure {
        kind: ErrorKind::EmbeddingModelMismatch,
        message,
        guidance: remediation.clone(),
        details: serde_json::json!({
            "corpus_model": corpus_model,
            "client_model": "voyage-code-3@2",
            "remediation": remediation,
        }),
        suggested_next_actions: vec![],
    };
    let result = failure.into_result();

    assert!(result.is_error, "isError must be true for Mismatch");
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["error"]["code"], "EMBEDDING_MODEL_MISMATCH");
    assert_eq!(sc["error"]["retryable"], false, "mismatch must not be retryable (I2)");
    assert!(sc["error"]["corpus_model"].is_string(), "corpus_model must be a string");
    assert!(
        sc["suggested_next_actions"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "mismatch must not suggest a next tool call"
    );
}

/// Verify that `get_chunk_neighbors` full pipeline preserves `sc["prev"]["chunks"]`
/// and `sc["next"]["chunks"]` length.
#[tokio::test]
async fn dispatch_get_chunk_neighbors_full_pipeline_via_wiremock() {
    use mnm_mcp::render;
    let server = MockServer::start().await;
    let id = "55555555-5555-5555-5555-555555555504";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/prev")))
        .and(query_param("count", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "chunks": [{"id": "p1"}] })))
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
    let v = run_chunk_neighbors(&json!({"id": id}), &client)
        .await
        .unwrap();
    let result =
        render::project_neighbors(v, mnm_core::injection::SecurityLevel::Disabled).into_result();
    assert!(!result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["chunk"]["id"], id);
    assert_eq!(sc["prev"]["chunks"].as_array().unwrap().len(), 1);
    assert_eq!(sc["next"]["chunks"].as_array().unwrap().len(), 2);
}

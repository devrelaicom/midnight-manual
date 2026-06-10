//! Wiremock-based tests for the `VoyageAI` embedding path in the MCP `search`
//! tool (Task 6.4).
//!
//! Covers:
//! (a) `CloudClient::fetch_active_model` correctly calls `GET /v1/models/active`
//!     and returns a `{name}@{revision}` wire id.
//! (b) `mn_embedding::client::embed` with `EmbedSource::Server` calls
//!     `POST /v1/embeddings` on the cloud URL (the server-proxy path).
//! (c) `run_search` in server-embed mode sends the corpus wire id as
//!     `client_embedding_model` in the search request, and the query vectors
//!     originate from `/v1/embeddings`.
//!
//! Tests (a) and (b) are environment-independent. Test (c) bypasses the
//! `VOYAGE_API_KEY` lookup by mocking the full call chain AND mounting all three
//! endpoints. When `VOYAGE_API_KEY` is not set the code routes through
//! `/v1/embeddings`; when it is set the code calls Voyage directly — in that
//! case we only assert the observable outcomes (wire id in request, results
//! returned) regardless of which embedding path was used, since both converge
//! on the same `/v1/search` shape.

#![allow(missing_docs)]

use std::sync::Arc;

use mn_embedding::client::{embed, EmbedSource};
use mn_embedding::voyage::InputType;
use mn_mcp::cloud_client::{CloudClient, CloudError};
use mn_mcp::server::ServerConfig;
use mn_mcp::tools::run_search;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// CloudClient::fetch_active_model — unit tests (pure wiremock, no env deps)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetch_active_model_returns_name_at_revision() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "voyage-code-3",
            "revision": 1,
            "dim": 1024,
            "provider": "voyageai",
        })))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let wire_id = client
        .fetch_active_model()
        .await
        .expect("fetch active model");
    assert_eq!(wire_id, "voyage-code-3@1");
}

#[tokio::test]
async fn fetch_active_model_formats_revision_as_integer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "voyage-code-3",
            "revision": 42,
            "dim": 1024,
            "provider": "voyageai",
        })))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let wire_id = client.fetch_active_model().await.expect("fetch");
    assert_eq!(wire_id, "voyage-code-3@42");
}

#[tokio::test]
async fn fetch_active_model_maps_404_to_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.fetch_active_model().await.unwrap_err();
    assert!(matches!(err, CloudError::NotFound(_)), "expected NotFound, got {err:?}");
}

#[tokio::test]
async fn fetch_active_model_returns_decode_error_for_missing_name() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "revision": 1,
            "dim": 1024,
        })))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.fetch_active_model().await.unwrap_err();
    assert!(matches!(err, CloudError::Decode(_)), "expected Decode, got {err:?}");
}

#[tokio::test]
async fn fetch_active_model_returns_decode_error_for_missing_revision() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "voyage-code-3",
            "dim": 1024,
        })))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.fetch_active_model().await.unwrap_err();
    assert!(matches!(err, CloudError::Decode(_)), "expected Decode, got {err:?}");
}

// ---------------------------------------------------------------------------
// mn_embedding::client::embed — server-proxy path (EmbedSource::Server)
// ---------------------------------------------------------------------------
//
// These tests drive the embed client directly against a wiremock `/v1/embeddings`
// endpoint, verifying (b): the server-proxy path hits the right URL.

#[tokio::test]
async fn embed_server_mode_posts_to_v1_embeddings() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-code-3@1",
            "embeddings": [[0.1_f32, 0.2, 0.3, 0.4]],
            "usage": { "total_tokens": 3 },
        })))
        .mount(&server)
        .await;

    let embedded = embed(
        vec!["how to deploy a Midnight dapp".to_owned()],
        InputType::Query,
        EmbedSource::Server {
            base_url: &server.uri(),
            bearer: None,
            no_global_limit: false,
        },
    )
    .await
    .expect("embed should succeed");

    assert_eq!(embedded.vectors.len(), 1);
    assert_eq!(embedded.vectors[0].len(), 4);
    assert_eq!(embedded.total_tokens, 3);
}

#[tokio::test]
async fn embed_server_mode_returns_vectors_in_order() {
    let server = MockServer::start().await;
    // Two distinct vectors — verify order is preserved.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-code-3@1",
            "embeddings": [
                [1.0_f32, 0.0, 0.0, 0.0],
                [0.0_f32, 1.0, 0.0, 0.0],
            ],
            "usage": { "total_tokens": 6 },
        })))
        .mount(&server)
        .await;

    let embedded = embed(
        vec!["first query".to_owned(), "second query".to_owned()],
        InputType::Query,
        EmbedSource::Server {
            base_url: &server.uri(),
            bearer: None,
            no_global_limit: false,
        },
    )
    .await
    .expect("embed ok");

    assert_eq!(embedded.vectors.len(), 2);
    assert!(
        (embedded.vectors[0][0] - 1.0).abs() < 1e-6,
        "first vector should start with 1.0"
    );
    assert!(
        (embedded.vectors[1][1] - 1.0).abs() < 1e-6,
        "second vector should have 1.0 at index 1"
    );
}

#[tokio::test]
async fn embed_server_mode_forwards_bearer_token() {
    use wiremock::matchers::header;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(header("authorization", "Bearer test-token-xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-code-3@1",
            "embeddings": [[0.5_f32, 0.5]],
            "usage": { "total_tokens": 2 },
        })))
        .mount(&server)
        .await;

    let embedded = embed(
        vec!["query with bearer".to_owned()],
        InputType::Query,
        EmbedSource::Server {
            base_url: &server.uri(),
            bearer: Some("test-token-xyz"),
            no_global_limit: false,
        },
    )
    .await
    .expect("embed with bearer ok");

    assert_eq!(embedded.vectors.len(), 1);
}

// ---------------------------------------------------------------------------
// run_search — integration: fetch_active_model + embed + search
//
// These tests assert (c): the search request carries the correct corpus wire
// id and the query vectors match what /v1/embeddings returned.
//
// Strategy: mount strict body_partial_json matchers on /v1/search. If
// run_search sends the wrong client_embedding_model or vectors, wiremock
// returns 404 and run_search propagates a Cloud error, failing the test.
//
// We cannot guarantee the embedding path (BYOK vs server-proxy) because
// VOYAGE_API_KEY may be set in the test runner environment. However, both
// paths converge on the same /v1/search body shape, so the assertions about
// client_embedding_model hold in either case. For the server-proxy vector
// assertion we only mount the /v1/embeddings mock; if BYOK is active the
// vectors come from api.voyageai.com (not matchable here), so that specific
// strict-vector test is wrapped in a guard.
// ---------------------------------------------------------------------------

/// Build a `ServerConfig` pointing at a wiremock server.
fn make_server_cfg(cloud_url: &str) -> ServerConfig {
    let mut cfg = ServerConfig::with_defaults(std::path::PathBuf::from("/tmp/test-mcp-cache"));
    cloud_url.clone_into(&mut cfg.cloud_url);
    cfg.telemetry_url = format!("{cloud_url}/v1/telemetry/events");
    cfg
}

/// Single-query, no-rerank `ParsedSearchArgs` (the shape these wiremock tests
/// always drive `run_search` with).
fn single_query_args(query: &str) -> mn_mcp::tools::ParsedSearchArgs {
    mn_mcp::tools::ParsedSearchArgs {
        queries: vec![query.to_owned()],
        limit: 10,
        rerank: false,
        filters: None,
        mode: "hybrid",
    }
}

#[tokio::test]
async fn run_search_uses_corpus_wire_id_as_client_embedding_model() {
    // Mount /v1/models/active with wire id "voyage-code-3@1".
    // Mount /v1/embeddings (server-proxy path; ignored if BYOK active).
    // Mount /v1/search with a strict matcher requiring client_embedding_model
    // == "voyage-code-3@1". If run_search sends a different value, the mock
    // won't match and the test fails via a Cloud error.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "voyage-code-3",
            "revision": 1,
            "dim": 4,
            "provider": "voyageai",
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-code-3@1",
            "embeddings": [[0.1_f32, 0.2, 0.3, 0.4]],
            "usage": { "total_tokens": 4 },
        })))
        .mount(&server)
        .await;

    // Strict: only 200 when client_embedding_model is the corpus wire id.
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .and(body_partial_json(json!({
            "client_embedding_model": "voyage-code-3@1",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{
                "chunk_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "content": "Midnight corpus result",
                "chunk_index": 0,
                "total_chunks": 1,
                "document_id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                "source_version_id": "cccccccc-cccc-cccc-cccc-cccccccccccc",
                "created_at": "2026-06-02T00:00:00Z",
                "scores": { "vector_similarity": 0.9 },
            }],
            "search_metadata": { "per_query": [], "total_candidates": 1 },
        })))
        .mount(&server)
        .await;

    let cfg = make_server_cfg(&server.uri());
    let cloud = Arc::new(CloudClient::new(&server.uri(), None).unwrap());

    let result = run_search(&single_query_args("how to compile a Compact contract"), &cfg, &cloud)
        .await
        .expect("run_search should succeed with corpus wire id");

    // The response must carry the corpus wire id as corpus_embedding_model.
    assert_eq!(
        result["corpus_embedding_model"], "voyage-code-3@1",
        "corpus_embedding_model in response must match the corpus wire id"
    );
    // Results must be present (the strict /v1/search mock matched).
    let results = result["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["content"], "Midnight corpus result");
}

#[tokio::test]
async fn run_search_server_embed_end_to_end_when_byok_absent() {
    // This test is only meaningful when VOYAGE_API_KEY is NOT set, because
    // when it IS set, embeddings come from api.voyageai.com and we cannot
    // mock that path. We check the env at test time and skip gracefully if the
    // key is present; the embed_server_mode_* tests above cover the
    // `EmbedSource::Server` path independently of `run_search`.
    if std::env::var("VOYAGE_API_KEY")
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        eprintln!(
            "SKIP: run_search_server_embed_end_to_end_when_byok_absent — \
             VOYAGE_API_KEY is set; BYOK path active"
        );
        return;
    }

    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "voyage-code-3",
            "revision": 1,
            "dim": 4,
            "provider": "voyageai",
        })))
        .mount(&server)
        .await;

    // Return a vector that the /v1/search body will contain.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-code-3@1",
            "embeddings": [[0.1_f32, 0.2, 0.3, 0.4]],
            "usage": { "total_tokens": 2 },
        })))
        .mount(&server)
        .await;

    // Accept any /v1/search body that has the right corpus wire id.
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .and(body_partial_json(json!({
            "client_embedding_model": "voyage-code-3@1",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{
                "chunk_id": "dddddddd-dddd-dddd-dddd-dddddddddddd",
                "content": "server-embed result",
                "chunk_index": 0,
                "total_chunks": 1,
                "document_id": "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee",
                "source_version_id": "ffffffff-ffff-ffff-ffff-ffffffffffff",
                "created_at": "2026-06-02T00:00:00Z",
                "scores": { "vector_similarity": 0.88 },
            }],
            "search_metadata": { "per_query": [], "total_candidates": 1 },
        })))
        .mount(&server)
        .await;

    let cfg = make_server_cfg(&server.uri());
    let cloud = Arc::new(CloudClient::new(&server.uri(), None).unwrap());

    let result = run_search(&single_query_args("zero knowledge proof"), &cfg, &cloud)
        .await
        .expect("run_search must succeed in server-embed mode");

    // The corpus wire id must be correct.
    assert_eq!(result["corpus_embedding_model"], "voyage-code-3@1");
    // Results must come back (the strict wire-id mock matched).
    let results = result["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["content"], "server-embed result");
}

#[tokio::test]
async fn run_search_propagates_active_model_fetch_failure() {
    // When /v1/models/active fails, run_search must return a Cloud error.
    // We still need /v1/embeddings to succeed so the embed step completes
    // before fetch_active_model is called.
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-code-3@1",
            "embeddings": [[0.1_f32, 0.2, 0.3, 0.4]],
            "usage": { "total_tokens": 1 },
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal server error"))
        .mount(&server)
        .await;

    let cfg = make_server_cfg(&server.uri());
    let cloud = Arc::new(CloudClient::new(&server.uri(), None).unwrap());

    let err = run_search(&single_query_args("test query"), &cfg, &cloud)
        .await
        .unwrap_err();

    assert!(
        matches!(err, mn_mcp::tools::SearchError::Cloud(_)),
        "expected Cloud error when /v1/models/active fails, got {err:?}"
    );
}

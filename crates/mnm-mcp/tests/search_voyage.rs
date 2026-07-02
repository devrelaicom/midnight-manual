//! Wiremock-based tests for the `VoyageAI` embedding path in the MCP `search`
//! tool (Task 6.4).
//!
//! Covers:
//! (a) `CloudClient::fetch_active_model` correctly calls `GET /v1/models/active`
//!     and returns a `{name}@{revision}` wire id.
//! (b) `mnm_embedding::client::embed_general` with `GeneralEmbedSource::Server`
//!     calls `POST /v1/embeddings` on the cloud URL (the server-proxy path).
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

use mnm_embedding::client::{embed_general, GeneralEmbedSource};
use mnm_embedding::voyage::InputType;
use mnm_mcp::cloud_client::{CloudClient, CloudError};
use mnm_mcp::server::ServerConfig;
use mnm_mcp::tools::run_search;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// True when `VOYAGE_API_KEY` is set (BYOK active): tests that depend on the
/// server-proxy `/v1/embeddings` path skip themselves.
fn byok_active() -> bool {
    std::env::var("VOYAGE_API_KEY")
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// Fetch the most recent `/v1/search` request body recorded by the mock server.
async fn last_search_body(server: &MockServer) -> serde_json::Value {
    let reqs = server
        .received_requests()
        .await
        .expect("request recording enabled");
    let req = reqs
        .iter()
        .rev()
        .find(|r| r.url.path() == "/v1/search")
        .expect("a /v1/search request was made");
    serde_json::from_slice(&req.body).expect("search body is JSON")
}

/// Count `/v1/embeddings` requests recorded by the mock server.
async fn embeddings_request_count(server: &MockServer) -> usize {
    let reqs = server
        .received_requests()
        .await
        .expect("request recording enabled");
    reqs.iter()
        .filter(|r| r.url.path() == "/v1/embeddings")
        .count()
}

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
    let active = client
        .fetch_active_model()
        .await
        .expect("fetch active model");
    assert_eq!(active.general.wire, "voyage-code-3@1");
    // The identity carries name/dim/dtype so the embedder is built from the
    // same source that labels the vectors.
    assert_eq!(active.general.name, "voyage-code-3");
    assert_eq!(active.general.dim, 1024);
    assert_eq!(active.general.dtype, "float");
    assert!(active.code.is_none(), "no `code` field in the response means no code model");
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
    let active = client.fetch_active_model().await.expect("fetch");
    assert_eq!(active.general.wire, "voyage-code-3@42");
}

#[tokio::test]
async fn fetch_active_model_parses_code_wire_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "voyage-context-3",
            "revision": 1,
            "dim": 1024,
            "provider": "voyageai",
            "code": { "name": "voyage-code-3", "revision": 7, "dim": 1024, "provider": "voyageai" },
        })))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let active = client.fetch_active_model().await.expect("fetch");
    assert_eq!(active.general.wire, "voyage-context-3@1");
    let code = active.code.expect("code model present");
    assert_eq!(code.wire, "voyage-code-3@7");
    assert_eq!(code.name, "voyage-code-3");
    assert_eq!(code.dim, 1024);
    assert_eq!(code.dtype, "float");
}

#[tokio::test]
async fn fetch_active_model_treats_malformed_code_as_absent() {
    // A `code` object missing name/revision degrades to "code unavailable",
    // not a decode error — the general half is unaffected.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "voyage-context-3",
            "revision": 1,
            "dim": 1024,
            "provider": "voyageai",
            "code": { "name": "voyage-code-3" },
        })))
        .mount(&server)
        .await;

    let client = CloudClient::new(&server.uri(), None).unwrap();
    let active = client.fetch_active_model().await.expect("fetch");
    assert_eq!(active.general.wire, "voyage-context-3@1");
    assert!(active.code.is_none());
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
// mnm_embedding::client::embed_general — server-proxy path
// (GeneralEmbedSource::Server)
// ---------------------------------------------------------------------------
//
// These tests drive the embed client directly against a wiremock `/v1/embeddings`
// endpoint, verifying (b): the server-proxy path `run_search` uses for general
// query embedding hits the right URL with `type=general`.

#[tokio::test]
async fn embed_general_server_mode_posts_to_v1_embeddings() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(body_partial_json(json!({ "type": "general" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-context-3@1",
            "embeddings": [[0.1_f32, 0.2, 0.3, 0.4]],
            "usage": { "total_tokens": 3 },
        })))
        .mount(&server)
        .await;

    let embedded = embed_general(
        vec!["how to deploy a Midnight dapp".to_owned()],
        InputType::Query,
        GeneralEmbedSource::Server {
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
async fn embed_general_server_mode_returns_vectors_in_order() {
    let server = MockServer::start().await;
    // Two distinct vectors — verify order is preserved.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-context-3@1",
            "embeddings": [
                [1.0_f32, 0.0, 0.0, 0.0],
                [0.0_f32, 1.0, 0.0, 0.0],
            ],
            "usage": { "total_tokens": 6 },
        })))
        .mount(&server)
        .await;

    let embedded = embed_general(
        vec!["first query".to_owned(), "second query".to_owned()],
        InputType::Query,
        GeneralEmbedSource::Server {
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
async fn embed_general_server_mode_forwards_bearer_token() {
    use wiremock::matchers::header;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(header("authorization", "Bearer test-token-xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-context-3@1",
            "embeddings": [[0.5_f32, 0.5]],
            "usage": { "total_tokens": 2 },
        })))
        .mount(&server)
        .await;

    let embedded = embed_general(
        vec!["query with bearer".to_owned()],
        InputType::Query,
        GeneralEmbedSource::Server {
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
    cloud_url.clone_into(&mut cfg.telemetry_endpoint);
    cfg
}

/// Single-query, no-rerank `ParsedSearchArgs` (the shape these wiremock tests
/// always drive `run_search` with). `rerank: false` keeps the reranker model
/// out of the test path (the public parsers force `rerank` on for basic
/// search, so these tests construct the struct directly).
fn single_query_args(query: &str) -> mnm_mcp::tools::ParsedSearchArgs {
    single_query_args_with(query, None)
}

/// [`single_query_args`] with an explicit `code_mode` override.
fn single_query_args_with(
    query: &str,
    code_mode: Option<&'static str>,
) -> mnm_mcp::tools::ParsedSearchArgs {
    mnm_mcp::tools::ParsedSearchArgs {
        queries: vec![query.to_owned()],
        limit: 10,
        rerank: false,
        filters: None,
        mode: "hybrid",
        code_mode,
        rerank_instructions: None,
        version_match: None,
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
        .expect("run_search should succeed with corpus wire id")
        .envelope;

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
        .expect("run_search must succeed in server-embed mode")
        .envelope;

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
        matches!(err, mnm_mcp::tools::SearchError::Cloud(_)),
        "expected Cloud error when /v1/models/active fails, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// run_search — code_mode (dual embeddings, §11.2)
//
// These tests inspect the recorded /v1/search request body directly (via
// `last_search_body`), so absence of fields is assertable. They mount two
// `type`-discriminated /v1/embeddings mocks returning distinct vectors so the
// general and code halves of each QueryPair are distinguishable. All of them
// depend on the server-proxy embedding path and skip under BYOK.
// ---------------------------------------------------------------------------

/// Mount `/v1/models/active` (optionally with a `code` model), the two
/// `type`-discriminated `/v1/embeddings` mocks (general → [1,0,0,0],
/// code → [0,1,0,0]), and a permissive `/v1/search`.
async fn mount_dual_embedding_stack(server: &MockServer, code_revision: Option<i64>) {
    let mut active = json!({
        "name": "voyage-context-3",
        "revision": 1,
        "dim": 4,
        "provider": "voyageai",
    });
    if let Some(rev) = code_revision {
        active["code"] = json!({
            "name": "voyage-code-3",
            "revision": rev,
            "dim": 4,
            "provider": "voyageai",
        });
    }
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(active))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(body_partial_json(json!({ "type": "general" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-context-3@1",
            "embeddings": [[1.0_f32, 0.0, 0.0, 0.0]],
            "usage": { "total_tokens": 4 },
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(body_partial_json(json!({ "type": "code" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "voyage-code-3@1",
            "embeddings": [[0.0_f32, 1.0, 0.0, 0.0]],
            "usage": { "total_tokens": 4 },
        })))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{
                "chunk_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "content": "dual-embedding result",
                "chunk_index": 0,
                "total_chunks": 1,
                "document_id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                "source_version_id": "cccccccc-cccc-cccc-cccc-cccccccccccc",
                "created_at": "2026-06-10T00:00:00Z",
                "scores": { "vector_similarity": 0.9 },
            }],
            "search_metadata": { "per_query": [], "total_candidates": 1 },
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn run_search_default_sends_code_vector_and_code_model() {
    if byok_active() {
        eprintln!("SKIP: run_search_default_sends_code_vector_and_code_model — BYOK active");
        return;
    }
    let server = MockServer::start().await;
    mount_dual_embedding_stack(&server, Some(7)).await;

    let cfg = make_server_cfg(&server.uri());
    let cloud = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let result = run_search(&single_query_args("fn deploy_contract"), &cfg, &cloud)
        .await
        .expect("run_search ok")
        .envelope;

    let body = last_search_body(&server).await;
    // Both halves embedded, each from its own type-tagged /v1/embeddings call.
    assert_eq!(body["queries"][0]["vector"], json!([1.0, 0.0, 0.0, 0.0]));
    assert_eq!(body["queries"][0]["code_vector"], json!([0.0, 1.0, 0.0, 0.0]));
    assert_eq!(body["client_embedding_model"], "voyage-context-3@1");
    // The code wire id comes from /v1/models/active's `code` field.
    assert_eq!(body["client_code_embedding_model"], "voyage-code-3@7");
    // Default (absent) code_mode is NOT forwarded — the server derives it.
    assert!(body.get("code_mode").is_none(), "absent code_mode must not be forwarded");
    assert_eq!(embeddings_request_count(&server).await, 2, "one general + one code embed");

    assert_eq!(result["corpus_embedding_model"], "voyage-context-3@1");
    assert_eq!(result["corpus_code_embedding_model"], "voyage-code-3@7");
}

#[tokio::test]
async fn run_search_code_mode_off_omits_code_fields() {
    if byok_active() {
        eprintln!("SKIP: run_search_code_mode_off_omits_code_fields — BYOK active");
        return;
    }
    let server = MockServer::start().await;
    mount_dual_embedding_stack(&server, Some(7)).await;

    let cfg = make_server_cfg(&server.uri());
    let cloud = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let result =
        run_search(&single_query_args_with("what is a zk proof", Some("off")), &cfg, &cloud)
            .await
            .expect("run_search ok")
            .envelope;

    let body = last_search_body(&server).await;
    // Explicit off IS forwarded (the caller overrode the server default).
    assert_eq!(body["code_mode"], "off");
    // No code embedding ran, no code fields on the wire.
    assert!(body.get("client_code_embedding_model").is_none());
    assert!(
        body["queries"][0].get("code_vector").is_none(),
        "empty code_vector must be omitted"
    );
    assert_eq!(body["queries"][0]["vector"], json!([1.0, 0.0, 0.0, 0.0]));
    assert_eq!(embeddings_request_count(&server).await, 1, "general embed only");

    assert_eq!(result["corpus_embedding_model"], "voyage-context-3@1");
    assert!(
        result.get("corpus_code_embedding_model").is_none(),
        "code model must not be reported when code search did not run"
    );
}

#[tokio::test]
async fn run_search_code_mode_exclusive_skips_general_embedding() {
    if byok_active() {
        eprintln!("SKIP: run_search_code_mode_exclusive_skips_general_embedding — BYOK active");
        return;
    }
    let server = MockServer::start().await;
    mount_dual_embedding_stack(&server, Some(7)).await;

    let cfg = make_server_cfg(&server.uri());
    let cloud = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let result =
        run_search(&single_query_args_with("VoyageEmbedder::new", Some("exclusive")), &cfg, &cloud)
            .await
            .expect("run_search ok")
            .envelope;

    let body = last_search_body(&server).await;
    assert_eq!(body["code_mode"], "exclusive");
    // Only the code half is embedded; the general vector is sent empty.
    assert_eq!(body["queries"][0]["vector"], json!([]));
    assert_eq!(body["queries"][0]["code_vector"], json!([0.0, 1.0, 0.0, 0.0]));
    assert_eq!(body["client_code_embedding_model"], "voyage-code-3@7");
    assert_eq!(embeddings_request_count(&server).await, 1, "code embed only");

    assert_eq!(result["corpus_code_embedding_model"], "voyage-code-3@7");
}

#[tokio::test]
async fn run_search_falls_back_to_config_code_wire_when_active_has_none() {
    if byok_active() {
        eprintln!(
            "SKIP: run_search_falls_back_to_config_code_wire_when_active_has_none — BYOK active"
        );
        return;
    }
    let server = MockServer::start().await;
    // /v1/models/active carries no `code` field.
    mount_dual_embedding_stack(&server, None).await;

    let cfg = make_server_cfg(&server.uri());
    let cloud = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    run_search(&single_query_args("fn main"), &cfg, &cloud)
        .await
        .expect("run_search ok");

    let body = last_search_body(&server).await;
    // Falls back to the config-pinned code model at revision 1.
    assert_eq!(body["client_code_embedding_model"], "voyage-code-3@1");
}

#[test]
fn fts_with_code_mode_on_is_rejected_at_parse_time() {
    // The fts/code_mode incompatibility fails in the parsers (before
    // run_search and any embedding or wire call) — mirrors the cloud's 400.
    let err = mnm_mcp::tools::parse_basic_search_args(&json!({
        "query": "x", "mode": "fts", "code_mode": "on"
    }))
    .unwrap_err();
    assert!(err.contains("code_mode"), "got: {err}");
}

// ---------------------------------------------------------------------------
// Error taxonomy on the embed step (#133 M1)
//
// On the DEFAULT hybrid/vector search path with NO BYOK key, the cloud's
// `/v1/embeddings` proxy is the FIRST rate-limited/authenticated cloud call —
// so a 429 / 401 there must surface RATE_LIMITED / AUTH_FAILED, NOT collapse
// into the retryable CLOUD_ERROR "retry shortly" the taxonomy work removes.
// The embed client cannot read `Retry-After`, so 429 carries a default snapshot
// and the server advises the conservative default backoff.
// ---------------------------------------------------------------------------

/// A 429 on the embed step of a hybrid `search` → `SearchError::RateLimited`
/// (default snapshot), rendered as `RATE_LIMITED` with the default backoff.
#[tokio::test]
async fn run_search_embed_429_maps_to_rate_limited() {
    if byok_active() {
        eprintln!("SKIP: run_search_embed_429_maps_to_rate_limited — BYOK path active");
        return;
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "voyage-code-3", "revision": 1, "dim": 4, "provider": "voyageai",
        })))
        .mount(&server)
        .await;
    // The embed proxy is rate-limited (the first cloud call the agent hits).
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": { "code": "rate_limited", "message": "slow down" },
        })))
        .mount(&server)
        .await;

    let cfg = make_server_cfg(&server.uri());
    let cloud = Arc::new(CloudClient::new(&server.uri(), None).unwrap());

    let err = run_search(&single_query_args("zero knowledge proof"), &cfg, &cloud)
        .await
        .unwrap_err();
    let snapshot = match err {
        mnm_mcp::tools::SearchError::RateLimited(s) => s,
        other => panic!("expected SearchError::RateLimited from the embed step, got {other:?}"),
    };
    // The embed client does not capture headers → default (empty) snapshot.
    assert_eq!(snapshot.retry_after_secs, None);

    let sc = mnm_mcp::server::rate_limited_failure(&snapshot)
        .into_result()
        .structured_content
        .unwrap();
    assert_eq!(sc["error"]["code"], "RATE_LIMITED");
    assert_eq!(sc["error"]["retryable"], true);
    assert_eq!(
        sc["error"]["retry_after_secs"].as_u64().unwrap(),
        mnm_mcp::server::DEFAULT_RATE_LIMIT_BACKOFF_SECS,
        "no header on the embed path → the conservative default backoff"
    );
}

/// A 401 on the embed step of a hybrid `search` → `SearchError::AuthFailed`,
/// rendered as `AUTH_FAILED` / `invalid_credentials`, `retryable: false`.
#[tokio::test]
async fn run_search_embed_401_maps_to_auth_failed() {
    if byok_active() {
        eprintln!("SKIP: run_search_embed_401_maps_to_auth_failed — BYOK path active");
        return;
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "voyage-code-3", "revision": 1, "dim": 4, "provider": "voyageai",
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "code": "unauthorized", "message": "invalid bearer" },
        })))
        .mount(&server)
        .await;

    let cfg = make_server_cfg(&server.uri());
    let cloud = Arc::new(CloudClient::new(&server.uri(), None).unwrap());

    let err = run_search(&single_query_args("zero knowledge proof"), &cfg, &cloud)
        .await
        .unwrap_err();
    let status = match err {
        mnm_mcp::tools::SearchError::AuthFailed { status } => status,
        other => panic!("expected SearchError::AuthFailed from the embed step, got {other:?}"),
    };
    assert_eq!(status, 401);

    let sc = mnm_mcp::server::auth_failed_failure(status)
        .into_result()
        .structured_content
        .unwrap();
    assert_eq!(sc["error"]["code"], "AUTH_FAILED");
    assert_eq!(sc["error"]["retryable"], false);
    assert_eq!(sc["error"]["auth_reason"], "invalid_credentials");
}

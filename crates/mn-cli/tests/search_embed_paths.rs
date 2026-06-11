//! Tests for Task 6.2: `mnm search` embeds queries via `VoyageAI`.
//!
//! Two surfaces are covered:
//!
//! 1. **Server-proxy embedding** (`GeneralEmbedSource::Server`):
//!    `mn_embedding::client::embed_general` posts to `/v1/embeddings` when no
//!    BYOK key is present, decodes the response, and returns the correct
//!    vectors.
//!
//! 2. **Wire-id propagation** (`run_with_paths` end-to-end in server-proxy
//!    mode): the `client_embedding_model` field in the `/v1/search` body must
//!    equal the corpus wire id returned by `GET /v1/models/active`
//!    (`"name@revision"`), and the query vectors must match what `/v1/embeddings`
//!    returned.
//!
//! We deliberately avoid the BYOK path here — that would require a real (or
//! mock) Voyage API key and a live network call; the unit tests in
//! `mn-embedding` cover the BYOK branch of the embed client itself.

#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use mn_cli::commands::search::{run_with_paths, Args, DEFAULT_EMBEDDING_MODEL};
use mn_embedding::client::{embed_general, GeneralEmbedSource};
use mn_embedding::voyage::InputType;
use mn_telemetry::TelemetryClient;
use serde_json::json;
use tempfile::tempdir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

// ---------------------------------------------------------------------------
// Helper: build a minimal Args for run_with_paths
// ---------------------------------------------------------------------------

fn make_args(query: &str) -> Args {
    Args {
        query: Some(query.to_owned()),
        extra_queries: vec![],
        queries_stdin: false,
        limit: 5,
        embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        rerank: false,
        reranker: None,
        mode: "hybrid".to_owned(),
        code_mode: None,
        kind: vec![],
        language: vec![],
        exclude_language: vec![],
        tag: vec![],
        exclude_tag: vec![],
        symbol: vec![],
        source: vec![],
        content_type: vec![],
        attribution: vec![],
        no_deprecated: false,
        verified: false,
        ingested_after: None,
        ingested_before: None,
        min_tokens: None,
        max_tokens: None,
        filter_json: None,
    }
}

// ---------------------------------------------------------------------------
// Helper: a stub /v1/embeddings response with a single vector
// ---------------------------------------------------------------------------

fn embeddings_body(vec: &[f32]) -> serde_json::Value {
    let embeddings: Vec<Vec<f32>> = vec![vec.to_owned()];
    json!({
        "model": "voyage-code-3@1",
        "embeddings": embeddings,
        "usage": { "total_tokens": 7 }
    })
}

// ---------------------------------------------------------------------------
// Helper: an empty /v1/search success response
// ---------------------------------------------------------------------------

fn empty_search_body() -> serde_json::Value {
    json!({ "results": [], "search_metadata": null })
}

// ---------------------------------------------------------------------------
// Helper: the active-model response
// ---------------------------------------------------------------------------

fn active_model_body() -> serde_json::Value {
    json!({
        "name": "voyage-code-3",
        "revision": 1,
        "dim": 1024,
        "provider": "voyageai"
    })
}

// ---------------------------------------------------------------------------
// Test 1: server-proxy embed returns the mocked vectors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn server_mode_embed_returns_mocked_vectors() {
    let server = MockServer::start().await;

    let expected_vec = vec![0.1_f32, 0.2, 0.3, 0.4];
    let expected_clone = expected_vec.clone();

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |_req: &Request| {
            ResponseTemplate::new(200).set_body_json(embeddings_body(&expected_clone))
        })
        .mount(&server)
        .await;

    let result = embed_general(
        vec!["how do I compile a Compact contract?".to_owned()],
        InputType::Query,
        GeneralEmbedSource::Server {
            base_url: &server.uri(),
            bearer: None,
            no_global_limit: false,
        },
    )
    .await
    .expect("server-proxy embed should succeed");

    assert_eq!(result.vectors.len(), 1, "one vector per query");
    assert_eq!(result.vectors[0], expected_vec, "vector must match mock response");
    assert_eq!(result.total_tokens, 7, "token usage must be forwarded");
}

// ---------------------------------------------------------------------------
// Test 2: run_with_paths (server-proxy mode) sends corpus wire id in /v1/search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_with_paths_server_mode_carries_corpus_wire_id() {
    let server = MockServer::start().await;

    // Mock GET /v1/models/active
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(active_model_body()))
        .mount(&server)
        .await;

    // Mock POST /v1/embeddings
    let embed_vec = vec![0.5_f32, 0.6, 0.7];
    let embed_vec_clone = embed_vec.clone();
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |_req: &Request| {
            ResponseTemplate::new(200).set_body_json(embeddings_body(&embed_vec_clone))
        })
        .mount(&server)
        .await;

    // Capture the /v1/search body
    let captured = Arc::new(Mutex::new(serde_json::Value::Null));
    let captured_clone = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(move |req: &Request| {
            *captured_clone.lock().unwrap() = req.body_json().unwrap_or(serde_json::Value::Null);
            ResponseTemplate::new(200).set_body_json(empty_search_body())
        })
        .mount(&server)
        .await;

    let cache_dir = tempdir().unwrap();
    let auth_dir = tempdir().unwrap();
    // auth_path points at a non-existent file so bearer resolves to None (anonymous).
    let auth_path = auth_dir.path().join("auth.toml");

    run_with_paths(
        make_args("deploy a Midnight contract"),
        &server.uri(),
        Some(&auth_path),
        cache_dir.path(),
        None, // config_path (default discovery)
        None, // no BYOK key → server-proxy mode
        &TelemetryClient::Disabled,
        "0.0.0-test",
        false,
    )
    .await
    .expect("run_with_paths should succeed");

    let body = captured.lock().unwrap().clone();

    // The client_embedding_model must be the corpus wire id from /v1/models/active.
    assert_eq!(
        body["client_embedding_model"], "voyage-code-3@1",
        "client_embedding_model must equal corpus wire id; got: {}",
        body["client_embedding_model"]
    );

    // The query vector must come from the /v1/embeddings mock response.
    #[allow(clippy::cast_possible_truncation)]
    let sent_vector: Vec<f32> = body["queries"][0]["vector"]
        .as_array()
        .expect("queries[0].vector must be an array")
        .iter()
        .map(|v| v.as_f64().unwrap() as f32)
        .collect();
    assert_eq!(sent_vector, embed_vec, "query vector must match the /v1/embeddings response");

    // Basic shape checks.
    assert_eq!(
        body["queries"][0]["text"], "deploy a Midnight contract",
        "query text must be forwarded"
    );
    assert_eq!(body["limit"], 5_u32, "limit must match args");

    // Filters must be present (default shape).
    assert!(body["filters"].is_object(), "filters field must be present");

    // Dual embeddings: hybrid mode without --code-mode also embeds the query
    // with the code model, so a code_vector rides along on each pair...
    assert!(
        body["queries"][0]["code_vector"].is_array(),
        "hybrid default must carry a code_vector; got: {body}"
    );
    // ...labelled with the code wire id. The active-model mock carries no
    // `code` half, so this is the `<config code model>@1` fallback.
    assert_eq!(
        body["client_code_embedding_model"], "voyage-code-3@1",
        "code wire id must fall back to the config code model; got: {}",
        body["client_code_embedding_model"]
    );
    // No --code-mode flag → the key is omitted (server default applies).
    assert!(
        !body.as_object().unwrap().contains_key("code_mode"),
        "code_mode key must be absent when the flag is not given: {body}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: explicit --embedding-model override skips the /v1/models/active call
// ---------------------------------------------------------------------------

#[tokio::test]
async fn explicit_embedding_model_override_skips_active_fetch() {
    let server = MockServer::start().await;

    // /v1/models/active must NOT be called when an explicit override is provided.
    // We register it with a 500 to make the test fail loudly if it is called.
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(500).set_body_string("should not be called"))
        .mount(&server)
        .await;

    // Mock POST /v1/embeddings
    let embed_vec = vec![0.1_f32, 0.2];
    let embed_vec_clone = embed_vec.clone();
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |_req: &Request| {
            ResponseTemplate::new(200).set_body_json(embeddings_body(&embed_vec_clone))
        })
        .mount(&server)
        .await;

    // Capture /v1/search body
    let captured = Arc::new(Mutex::new(serde_json::Value::Null));
    let captured_clone = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(move |req: &Request| {
            *captured_clone.lock().unwrap() = req.body_json().unwrap_or(serde_json::Value::Null);
            ResponseTemplate::new(200).set_body_json(empty_search_body())
        })
        .mount(&server)
        .await;

    let cache_dir = tempdir().unwrap();
    let auth_dir = tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.toml");

    // Build args with an explicit override (not "auto") AND code search off.
    // With dual embeddings the code wire id is resolved from /v1/models/active,
    // so only the `--code-mode off` + explicit-override combination needs no
    // active-model round-trip at all.
    let mut args = make_args("query text");
    args.limit = 3;
    args.embedding_model = "voyage-code-3@2".to_owned();
    args.code_mode = Some("off".to_owned());

    run_with_paths(
        args,
        &server.uri(),
        Some(&auth_path),
        cache_dir.path(),
        None, // config_path (default discovery)
        None, // no BYOK key
        &TelemetryClient::Disabled,
        "0.0.0-test",
        false,
    )
    .await
    .expect("run_with_paths with explicit model override should succeed");

    let body = captured.lock().unwrap().clone();
    assert_eq!(
        body["client_embedding_model"], "voyage-code-3@2",
        "explicit override must be used verbatim; got: {}",
        body["client_embedding_model"]
    );
    // --code-mode off rides along verbatim, and no code wire id is sent.
    assert_eq!(body["code_mode"], "off", "got: {body}");
    assert!(
        !body
            .as_object()
            .unwrap()
            .contains_key("client_code_embedding_model"),
        "no code embedding was made, so the code wire id must be absent: {body}"
    );
}

// ---------------------------------------------------------------------------
// Helpers for the rerank wire-contract tests
// ---------------------------------------------------------------------------

/// Mount /v1/models/active + /v1/embeddings, plus a capturing /v1/search that
/// always returns an EMPTY result set. An empty set lets `--rerank` short-circuit
/// before loading a reranker model (no ~100 MB ONNX download in CI), while we
/// still capture the request body the CLI sent. Returns the shared capture cell.
async fn mount_capturing_search(server: &MockServer) -> Arc<Mutex<serde_json::Value>> {
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(active_model_body()))
        .mount(server)
        .await;

    let embed_vec = vec![0.1_f32, 0.2, 0.3];
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |_req: &Request| {
            ResponseTemplate::new(200).set_body_json(embeddings_body(&embed_vec))
        })
        .mount(server)
        .await;

    let captured = Arc::new(Mutex::new(serde_json::Value::Null));
    let captured_clone = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(move |req: &Request| {
            *captured_clone.lock().unwrap() = req.body_json().unwrap_or(serde_json::Value::Null);
            ResponseTemplate::new(200).set_body_json(empty_search_body())
        })
        .mount(server)
        .await;

    captured
}

// ---------------------------------------------------------------------------
// Test 4: --rerank widens the cloud pool to RERANK_FETCH and sorts by score
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rerank_widens_pool_and_requests_score_sort() {
    let server = MockServer::start().await;
    let captured = mount_capturing_search(&server).await;

    let cache_dir = tempdir().unwrap();
    let auth_dir = tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.toml");

    let mut args = make_args("how do shielded transactions work?");
    // Caller asks for 5; the rerank path must override the CLOUD limit to the
    // wider RERANK_FETCH pool (50) so the cross-encoder can promote a candidate
    // ranked below 5. The empty response short-circuits before any model load.
    args.limit = 5;
    args.rerank = true;

    run_with_paths(
        args,
        &server.uri(),
        Some(&auth_path),
        cache_dir.path(),
        None, // config_path (default discovery)
        None, // no BYOK key → server-proxy mode
        &TelemetryClient::Disabled,
        "0.0.0-test",
        false,
    )
    .await
    .expect("rerank run_with_paths should succeed (empty results short-circuit)");

    let body = captured.lock().unwrap().clone();
    assert_eq!(
        body["limit"], 50_u32,
        "rerank path must request RERANK_FETCH (50) candidates, not the caller's --limit; got: {}",
        body["limit"]
    );
    assert_eq!(
        body["sort_by"], "score",
        "rerank path must ask the cloud for relevance order; got: {}",
        body["sort_by"]
    );
}

// ---------------------------------------------------------------------------
// Test 5: non-rerank keeps the caller's limit and OMITS sort_by entirely
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_rerank_keeps_limit_and_omits_sort_by() {
    let server = MockServer::start().await;
    let captured = mount_capturing_search(&server).await;

    let cache_dir = tempdir().unwrap();
    let auth_dir = tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.toml");

    let mut args = make_args("how do shielded transactions work?");
    args.limit = 5;
    args.rerank = false; // explicit: the default, but pin it for clarity

    run_with_paths(
        args,
        &server.uri(),
        Some(&auth_path),
        cache_dir.path(),
        None,
        None,
        &TelemetryClient::Disabled,
        "0.0.0-test",
        false,
    )
    .await
    .expect("non-rerank run_with_paths should succeed");

    let body = captured.lock().unwrap().clone();
    assert_eq!(
        body["limit"], 5_u32,
        "non-rerank path must forward the caller's --limit unchanged; got: {}",
        body["limit"]
    );
    // skip_serializing_if = "Option::is_none" must drop the key entirely (not
    // serialize it as null) so the wire body is byte-identical to pre-Task-9.4.
    let obj = body
        .as_object()
        .expect("/v1/search body must be a JSON object");
    assert!(
        !obj.contains_key("sort_by"),
        "non-rerank wire body must OMIT sort_by (skip_serializing_if), but it was present: {body}"
    );
}

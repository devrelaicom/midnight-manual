//! Tests for Task 6.2: `mnm search` embeds queries via `VoyageAI`.
//!
//! Two surfaces are covered:
//!
//! 1. **Server-proxy embedding** (`EmbedSource::Server`): `mn_embedding::client::embed`
//!    posts to `/v1/embeddings` when no BYOK key is present, decodes the
//!    response, and returns the correct vectors.
//!
//! 2. **Wire-id propagation** (`run_with_paths` end-to-end in server-proxy
//!    mode): the `client_embedding_model` field in the `/v1/search` body must
//!    equal the corpus wire id returned by `GET /v1/models/active`
//!    (`"name@revision"`), and the query vectors must match what `/v1/embeddings`
//!    returned.
//!
//! We deliberately avoid the BYOK path here — that would require a real (or
//! mock) Voyage API key and a live network call; the unit tests in
//! `mn-embedding` cover the BYOK branch of `embed()` itself.

#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use mn_cli::commands::search::{run_with_paths, Args, DEFAULT_EMBEDDING_MODEL};
use mn_embedding::client::{embed, EmbedSource};
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

    let result = embed(
        vec!["how do I compile a Compact contract?".to_owned()],
        InputType::Query,
        EmbedSource::Server {
            base_url: &server.uri(),
            bearer: None,
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

    // Build args with an explicit override (not "auto").
    let args = Args {
        query: Some("query text".to_owned()),
        extra_queries: vec![],
        queries_stdin: false,
        limit: 3,
        embedding_model: "voyage-code-3@2".to_owned(),
    };

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
}

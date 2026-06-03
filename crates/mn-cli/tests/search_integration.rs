//! Integration tests for `mnm search` driven against a `wiremock` mock of
//! `POST /v1/search`.
//!
//! These tests deliberately bypass the local embedder — loading the
//! ~100 MB ONNX bundle inside CI would be slow and flaky. The tests
//! drive [`mn_cli::commands::search::search_via_http`] directly with a
//! pre-built `SearchRequest`, exercising the HTTP, decoding, rendering,
//! and bearer-resolution surfaces.

use std::sync::{Arc, Mutex};

use mn_cli::commands::search::{search_via_http, QueryPair, SearchRequest};
use mn_core::auth_file::AuthFile;
use mn_retrieval::filters::SearchFilters;
use serde_json::json;
use time::{Duration, OffsetDateTime};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn make_request(query: &str, limit: u32) -> SearchRequest {
    SearchRequest {
        queries: vec![QueryPair {
            text: query.to_owned(),
            vector: vec![0.1, 0.2, 0.3],
        }],
        client_embedding_model: "bge-base-en-v1.5@1".to_owned(),
        limit,
        filters: SearchFilters::default(),
        // Non-rerank path: omitted on the wire (skip_serializing_if).
        sort_by: None,
    }
}

fn sample_results_body(count: usize) -> serde_json::Value {
    let results: Vec<serde_json::Value> = (0..count)
        .map(|i| {
            json!({
                "chunk_id": format!("00000000-0000-0000-0000-{:012x}", i + 1),
                "content": format!("Result {i} body sentence."),
                "document_id": format!("00000000-0000-0000-0000-{:012x}", 100 + i),
                "source_version_id": "00000000-0000-0000-0000-000000000aaa",
                "chunk_index": i,
                "total_chunks": count,
                "created_at": "2026-05-14T00:00:00Z",
                "scores": { "vector_similarity": 0.9 },
            })
        })
        .collect();
    json!({ "results": results, "search_metadata": {"per_query": [], "total_candidates": count} })
}

#[tokio::test]
async fn happy_path_posts_search_and_returns_ok() {
    let server = MockServer::start().await;
    let captured_body = Arc::new(Mutex::new(serde_json::Value::Null));
    let body_capture = Arc::clone(&captured_body);
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(move |req: &Request| {
            *body_capture.lock().unwrap() = req.body_json().unwrap();
            ResponseTemplate::new(200).set_body_json(sample_results_body(3))
        })
        .mount(&server)
        .await;

    let request = make_request("compact contract", 10);
    search_via_http(&server.uri(), None, &request, true)
        .await
        .expect("search ok");

    let captured = captured_body.lock().unwrap().clone();
    assert_eq!(captured["client_embedding_model"], "bge-base-en-v1.5@1");
    assert_eq!(captured["limit"], 10);
    assert_eq!(captured["queries"][0]["text"], "compact contract");
    assert_eq!(captured["queries"][0]["vector"].as_array().unwrap().len(), 3,);
}

#[tokio::test]
async fn multi_query_request_sends_all_pairs() {
    let server = MockServer::start().await;
    let captured_body = Arc::new(Mutex::new(serde_json::Value::Null));
    let body_capture = Arc::clone(&captured_body);
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(move |req: &Request| {
            *body_capture.lock().unwrap() = req.body_json().unwrap();
            ResponseTemplate::new(200).set_body_json(sample_results_body(2))
        })
        .mount(&server)
        .await;

    let request = SearchRequest {
        queries: vec![
            QueryPair {
                text: "primary".to_owned(),
                vector: vec![0.1, 0.2, 0.3],
            },
            QueryPair {
                text: "alt one".to_owned(),
                vector: vec![0.4, 0.5, 0.6],
            },
        ],
        client_embedding_model: "bge-base-en-v1.5@1".to_owned(),
        limit: 10,
        filters: SearchFilters::default(),
        sort_by: None,
    };
    search_via_http(&server.uri(), None, &request, true)
        .await
        .expect("multi-query search ok");

    let captured = captured_body.lock().unwrap().clone();
    let queries = captured["queries"].as_array().unwrap();
    assert_eq!(queries.len(), 2);
    assert_eq!(queries[0]["text"], "primary");
    assert_eq!(queries[1]["text"], "alt one");
}

#[tokio::test]
async fn bearer_is_sent_when_provided() {
    let server = MockServer::start().await;
    let captured_auth = Arc::new(Mutex::new(Option::<String>::None));
    let auth_capture = Arc::clone(&captured_auth);
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(move |req: &Request| {
            if let Some(h) = req.headers.get("authorization") {
                *auth_capture.lock().unwrap() = Some(h.to_str().unwrap().to_owned());
            }
            ResponseTemplate::new(200).set_body_json(sample_results_body(1))
        })
        .mount(&server)
        .await;

    let request = make_request("hello", 5);
    search_via_http(&server.uri(), Some("test-bearer-token"), &request, false)
        .await
        .expect("search ok");

    let bearer = captured_auth.lock().unwrap().clone().unwrap();
    assert_eq!(bearer, "Bearer test-bearer-token");
}

#[tokio::test]
async fn bearer_omitted_when_none() {
    let server = MockServer::start().await;
    let saw_auth = Arc::new(Mutex::new(false));
    let auth_flag = Arc::clone(&saw_auth);
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(move |req: &Request| {
            if req.headers.contains_key("authorization") {
                *auth_flag.lock().unwrap() = true;
            }
            ResponseTemplate::new(200).set_body_json(sample_results_body(0))
        })
        .mount(&server)
        .await;

    let request = make_request("anon", 5);
    search_via_http(&server.uri(), None, &request, false)
        .await
        .expect("anonymous search ok");
    assert!(!*saw_auth.lock().unwrap(), "no Authorization header expected");
}

#[tokio::test]
async fn model_mismatch_409_surfaces_clear_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": {
                "code": "embedding_model_mismatch",
                "message": "client model bge-base-en-v1.5@1 does not match corpus bge-base-en-v1.5@2",
                "remediation": "run `mnm models pull` to refresh the local model",
            },
            "request_id": "rid_x",
        })))
        .mount(&server)
        .await;

    let request = make_request("q", 10);
    let err = search_via_http(&server.uri(), None, &request, false)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("409"), "expected 409 in error: {msg}");
    assert!(msg.contains("embedding_model_mismatch"), "expected code in error: {msg}",);
}

#[tokio::test]
async fn non_2xx_body_redacts_long_blobs_in_error() {
    let server = MockServer::start().await;
    let leak = "eyJhbGciOiJIUzI1NiJ9.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let body = json!({
        "error": {"code": "forbidden", "message": format!("see token={leak}")}
    });
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(403).set_body_json(body))
        .mount(&server)
        .await;

    let request = make_request("x", 1);
    let err = search_via_http(&server.uri(), Some(leak), &request, false)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(!msg.contains(leak), "long blob must be redacted: {msg}");
}

#[tokio::test]
async fn missing_auth_file_resolves_to_anonymous_at_command_level() {
    // Confirms the higher-level fn picks up missing auth.toml as anonymous.
    // We don't run `run` end-to-end (no embedder in tests) but exercise the
    // surface via search_via_http with a None bearer.
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist.toml");
    assert!(AuthFile::read_optional(&missing).unwrap().is_none());

    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sample_results_body(0)))
        .mount(&server)
        .await;
    let request = make_request("q", 1);
    search_via_http(&server.uri(), None, &request, false)
        .await
        .expect("anonymous search ok");
}

#[tokio::test]
async fn expired_admin_token_in_auth_file_is_ignored() {
    // active_admin_token returns None when expires_at <= now.
    let dir = tempfile::tempdir().unwrap();
    let auth_path = dir.path().join("auth.toml");
    let past = OffsetDateTime::now_utc() - Duration::hours(1);
    AuthFile::write_admin_token(&auth_path, "aaron", "expired-token-xyz", past).unwrap();

    let file = AuthFile::read_optional(&auth_path).unwrap().unwrap();
    assert!(file.active_admin_token(OffsetDateTime::now_utc()).is_none());
}

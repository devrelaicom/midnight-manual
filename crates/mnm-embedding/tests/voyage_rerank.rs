//! Task 9.1: `VoyageReranker` posts to /v1/rerank and maps results.
#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use mnm_embedding::voyage::VoyageReranker;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test]
async fn reranks_and_returns_sorted_indices() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object":"list",
            "data":[{"relevance_score":0.2,"index":0},{"relevance_score":0.9,"index":1}],
            "model":"rerank-2.5-lite","usage":{"total_tokens":8}})))
        .mount(&server)
        .await;
    let r = VoyageReranker::new("k", "rerank-2.5-lite").with_base_url(&server.uri());
    let out = r
        .rerank("q".into(), vec!["a".into(), "b".into()], None)
        .await
        .unwrap();
    // returns RerankResult{index,score}; index 1 has the higher score
    assert_eq!(
        out.results
            .iter()
            .max_by(|a, b| a.score.total_cmp(&b.score))
            .unwrap()
            .index,
        1
    );
    assert_eq!(out.total_tokens, 8);
}

#[tokio::test]
async fn maps_non_2xx_to_status_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .mount(&server)
        .await;
    let r = VoyageReranker::new("k", "rerank-2.5-lite").with_base_url(&server.uri());
    let err = r
        .rerank("q".into(), vec!["a".into(), "b".into()], None)
        .await
        .unwrap_err();
    match err {
        mnm_embedding::voyage::VoyageError::Status { status, .. } => assert_eq!(status, 429),
        other => panic!("expected Status, got {other:?}"),
    }
}

/// Issue a rerank with the given `top_k` and return the JSON body the client
/// actually put on the wire.
async fn captured_rerank_body(top_k: Option<usize>) -> serde_json::Value {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(serde_json::Value::Null));
    let sink = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(move |req: &Request| {
            *sink.lock().unwrap() = req.body_json().unwrap_or(serde_json::Value::Null);
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data":[{"relevance_score":0.5,"index":0}],"usage":{"total_tokens":3}}))
        })
        .mount(&server)
        .await;
    let r = VoyageReranker::new("k", "rerank-2.5-lite").with_base_url(&server.uri());
    r.rerank("q".into(), vec!["a".into()], top_k).await.unwrap();
    let body = captured.lock().unwrap().clone();
    body
}

#[tokio::test]
async fn top_k_is_omitted_when_none_and_present_when_some() {
    // None → the field must be absent entirely (skip_serializing_if), not null.
    let none_body = captured_rerank_body(None).await;
    assert!(
        none_body.get("top_k").is_none(),
        "top_k must be omitted from the body when None; got: {none_body}"
    );
    // Sanity: the rest of the contract is on the wire.
    assert_eq!(none_body["model"], "rerank-2.5-lite");
    assert_eq!(none_body["query"], "q");
    assert_eq!(none_body["documents"][0], "a");

    // Some(k) → present with the value.
    let some_body = captured_rerank_body(Some(1)).await;
    assert_eq!(some_body["top_k"], 1, "top_k must be sent when Some; got: {some_body}");
}

//! Integration tests for the corpus section of `mnm doctor`.
//!
//! Drives [`fetch_corpus_report`] against a wiremock server so we cover
//! transport, auth-header propagation, and decoding without booting the
//! real cloud server.

use mn_cli::commands::doctor::fetch_corpus_report;
use serde_json::json;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test]
async fn fetch_corpus_report_decodes_canonical_shape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/ingest/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "active_embedding_model": "voyage-code-3@1",
            "sources": [
                {
                    "slug": "docs",
                    "active_revision": 3,
                    "total_chunks": 150,
                    "ready_chunks": 145,
                    "embed_failed_chunks": 5,
                },
                {
                    "slug": "code",
                    "active_revision": null,
                    "total_chunks": 0,
                    "ready_chunks": 0,
                    "embed_failed_chunks": 0,
                },
            ],
        })))
        .mount(&server)
        .await;

    let r = fetch_corpus_report(&server.uri(), "test-bearer")
        .await
        .expect("decode");
    assert_eq!(r.active_embedding_model, "voyage-code-3@1");
    assert_eq!(r.sources.len(), 2);
    let docs = r.sources.iter().find(|s| s.slug == "docs").unwrap();
    assert_eq!(docs.active_revision, Some(3));
    assert_eq!(docs.total_chunks, 150);
    assert_eq!(docs.embed_failed_chunks, 5);
    let code = r.sources.iter().find(|s| s.slug == "code").unwrap();
    assert_eq!(code.active_revision, None);
    assert_eq!(code.total_chunks, 0);
}

#[tokio::test]
async fn fetch_corpus_report_sends_bearer_header() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(Option::<String>::None));
    let auth_capture = Arc::clone(&captured);
    Mock::given(method("GET"))
        .and(path("/v1/admin/ingest/status"))
        .respond_with(move |req: &Request| {
            if let Some(h) = req.headers.get("authorization") {
                *auth_capture.lock().unwrap() = Some(h.to_str().unwrap().to_owned());
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "active_embedding_model": "x@1",
                "sources": [],
            }))
        })
        .mount(&server)
        .await;

    fetch_corpus_report(&server.uri(), "admin-bearer-123")
        .await
        .expect("ok");
    let bearer = captured.lock().unwrap().clone().unwrap();
    assert_eq!(bearer, "Bearer admin-bearer-123");
}

#[tokio::test]
async fn fetch_corpus_report_surfaces_401_clearly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/ingest/status"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "unauthorized", "message": "admin bearer required"}
        })))
        .mount(&server)
        .await;

    let err = fetch_corpus_report(&server.uri(), "anything")
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("401"), "{msg}");
}

#[tokio::test]
async fn fetch_corpus_report_rejects_malformed_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/admin/ingest/status"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;

    let err = fetch_corpus_report(&server.uri(), "tok").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("decoding response") || msg.contains("expected") || msg.contains("error"),
        "expected decode error: {msg}",
    );
}

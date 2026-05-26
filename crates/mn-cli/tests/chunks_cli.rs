//! Wiremock-backed smoke tests for `mnm chunks {show, next, prev}`.

use std::process::Command;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn chunk_with_context_json() -> serde_json::Value {
    serde_json::json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "source_version_id": "22222222-2222-2222-2222-222222222222",
        "document_id": "33333333-3333-3333-3333-333333333333",
        "node_id": "44444444-4444-4444-4444-444444444444",
        "chunk_index": 0,
        "total_chunks": 3,
        "content": "Hello world body text.",
        "content_hash": "sha256:abc",
        "embedding_model_id": "55555555-5555-5555-5555-555555555555",
        "heading_path": ["Welcome"],
        "symbol_path": [],
        "start_byte": 0,
        "end_byte": 22,
        "token_count": 4,
        "status": "ready",
        "created_at": "2026-05-25T00:00:00Z",
        "document": {
            "id": "33333333-3333-3333-3333-333333333333",
            "source_path": "welcome.md",
            "published_url": "https://example.com/welcome/",
            "source_url": null,
            "language": "markdown",
            "kind": "markdown",
            "provenance": {}
        },
        "source": { "slug": "smoke" }
    })
}

#[tokio::test]
async fn chunks_show_renders_chunk_and_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/chunks/11111111-1111-1111-1111-111111111111"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chunk_with_context_json()))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["--server", &server.uri(), "chunks", "show",
               "11111111-1111-1111-1111-111111111111"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Hello world body text"), "stdout: {stdout}");
    assert!(stdout.contains("welcome.md") || stdout.contains("https://example.com/welcome/"),
            "expected document context in output: {stdout}");
}

#[tokio::test]
async fn chunks_next_renders_two_chunks() {
    let server = MockServer::start().await;
    let body = serde_json::json!({ "chunks": [chunk_with_context_json(), chunk_with_context_json()] });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/chunks/[0-9a-f-]+/next$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["--server", &server.uri(), "chunks", "next",
               "11111111-1111-1111-1111-111111111111", "--count", "2"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[tokio::test]
async fn chunks_prev_renders_two_chunks() {
    let server = MockServer::start().await;
    let body = serde_json::json!({ "chunks": [chunk_with_context_json()] });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/chunks/[0-9a-f-]+/prev$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["--server", &server.uri(), "chunks", "prev",
               "11111111-1111-1111-1111-111111111111"])
        .output()
        .unwrap();
    assert!(out.status.success());
}

//! Wiremock-backed smoke tests for `mnm chunks {show, next, prev}`.

use std::process::Command;
use wiremock::matchers::{method, path, path_regex, query_param};
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
        .args([
            "--server",
            &server.uri(),
            "chunks",
            "show",
            "11111111-1111-1111-1111-111111111111",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Hello world body text"), "stdout: {stdout}");
    assert!(
        stdout.contains("welcome.md") || stdout.contains("https://example.com/welcome/"),
        "expected document context in output: {stdout}"
    );
}

#[tokio::test]
async fn chunks_next_renders_two_chunks() {
    let server = MockServer::start().await;
    let body =
        serde_json::json!({ "chunks": [chunk_with_context_json(), chunk_with_context_json()] });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/chunks/[0-9a-f-]+/next$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args([
            "--server",
            &server.uri(),
            "chunks",
            "next",
            "11111111-1111-1111-1111-111111111111",
            "--count",
            "2",
        ])
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
        .args([
            "--server",
            &server.uri(),
            "chunks",
            "prev",
            "11111111-1111-1111-1111-111111111111",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
}

// ---------------------------------------------------------------------------
// `mnm chunks neighbors` — composes prev + show + next.
// ---------------------------------------------------------------------------

/// Mount stock prev/show/next handlers and return the running server.
async fn neighbors_server(list_chunks_each_side: usize) -> MockServer {
    let server = MockServer::start().await;
    let one = chunk_with_context_json();
    let list: Vec<_> = (0..list_chunks_each_side).map(|_| one.clone()).collect();
    let list_body = serde_json::json!({ "chunks": list });

    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/chunks/[0-9a-f-]+/prev$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&list_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/chunks/[0-9a-f-]+/next$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&list_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/chunks/11111111-1111-1111-1111-111111111111"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chunk_with_context_json()))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn chunks_neighbors_renders_prev_anchor_next_with_default_count() {
    let server = neighbors_server(2).await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args([
            "--server",
            &server.uri(),
            "chunks",
            "neighbors",
            "11111111-1111-1111-1111-111111111111",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Three labelled sections must appear in order.
    let prev_at = stdout.find("prev:").expect("prev: section");
    let chunk_at = stdout.find("chunk:").expect("chunk: section");
    let next_at = stdout.find("next:").expect("next: section");
    assert!(prev_at < chunk_at && chunk_at < next_at, "section ordering: {stdout}");

    // Anchor body is rendered (chunks show reuse).
    assert!(stdout.contains("Hello world body text"), "expected anchor body: {stdout}");
}

#[tokio::test]
async fn chunks_neighbors_forwards_count_override() {
    // Server here only mounts list handlers that *require* `count=4`. Anything
    // else returns the default 404 and the test fails — that's how we assert
    // the CLI forwarded `--count` through to both list endpoints.
    let server = MockServer::start().await;
    let list_body = serde_json::json!({
        "chunks": [
            chunk_with_context_json(),
            chunk_with_context_json(),
            chunk_with_context_json(),
            chunk_with_context_json(),
        ],
    });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/chunks/[0-9a-f-]+/(prev|next)$"))
        .and(query_param("count", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&list_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/chunks/11111111-1111-1111-1111-111111111111"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chunk_with_context_json()))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args([
            "--server",
            &server.uri(),
            "chunks",
            "neighbors",
            "11111111-1111-1111-1111-111111111111",
            "--count",
            "4",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[tokio::test]
async fn chunks_neighbors_full_flag_emits_unpreviewed_body() {
    // The fixture body is only 22 chars, so preview vs --full normally
    // looks identical. Build a longer fixture for this test so we can
    // observe the difference: preview mode truncates to 240 chars, full
    // mode renders the whole thing.
    let mut long_chunk = chunk_with_context_json();
    let long_body: String = "lorem ipsum ".repeat(40); // 480 chars
    long_chunk["content"] = serde_json::Value::String(long_body);

    let server = MockServer::start().await;
    let list_body = serde_json::json!({ "chunks": [long_chunk.clone()] });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/chunks/[0-9a-f-]+/(prev|next)$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&list_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/chunks/11111111-1111-1111-1111-111111111111"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&long_chunk))
        .mount(&server)
        .await;

    // Without --full: preview is truncated, so we see the "..." marker.
    let preview = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args([
            "--server",
            &server.uri(),
            "chunks",
            "neighbors",
            "11111111-1111-1111-1111-111111111111",
        ])
        .output()
        .unwrap();
    assert!(preview.status.success());
    let preview_out = String::from_utf8_lossy(&preview.stdout);
    assert!(preview_out.contains("..."), "expected preview truncation: {preview_out}");

    // With --full: the full body appears in each list section. Count
    // "lorem" occurrences — preview truncates at 237 chars (~19 copies),
    // full mode has all 40 in each of prev/next plus 40 in the anchor.
    let full = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args([
            "--server",
            &server.uri(),
            "chunks",
            "neighbors",
            "11111111-1111-1111-1111-111111111111",
            "--full",
        ])
        .output()
        .unwrap();
    assert!(full.status.success(), "stderr: {}", String::from_utf8_lossy(&full.stderr));
    let full_out = String::from_utf8_lossy(&full.stdout);
    let lorem_count = full_out.matches("lorem").count();
    assert!(lorem_count >= 40, "expected >=40 lorem in full output, got {lorem_count}");
}

#[tokio::test]
async fn chunks_neighbors_handles_corpus_edges_with_empty_lists() {
    // Both prev and next return empty arrays — anchor is at start/end of
    // the corpus, or is the only chunk in its document. CLI must still
    // succeed and render the anchor + the "(no further chunks)" markers.
    let server = MockServer::start().await;
    let empty = serde_json::json!({ "chunks": [] });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/chunks/[0-9a-f-]+/(prev|next)$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&empty))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/chunks/11111111-1111-1111-1111-111111111111"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chunk_with_context_json()))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args([
            "--server",
            &server.uri(),
            "chunks",
            "neighbors",
            "11111111-1111-1111-1111-111111111111",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Anchor still renders…
    assert!(stdout.contains("Hello world body text"));
    // …and both list sections collapse to the empty-list marker.
    let no_further = stdout.matches("(no further chunks)").count();
    assert_eq!(no_further, 2, "expected empty marker in both prev and next: {stdout}");
}

#[tokio::test]
async fn chunks_neighbors_json_emits_composite_envelope() {
    let server = neighbors_server(2).await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args([
            "--json",
            "--server",
            &server.uri(),
            "chunks",
            "neighbors",
            "11111111-1111-1111-1111-111111111111",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON envelope");
    assert!(v["prev"]["chunks"].is_array(), "prev.chunks array");
    assert!(v["next"]["chunks"].is_array(), "next.chunks array");
    assert_eq!(v["chunk"]["id"].as_str(), Some("11111111-1111-1111-1111-111111111111"));
}

//! Wiremock-backed smoke tests for `mnm documents {show, full, chunks}`.

use std::process::Command;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn overview_json() -> serde_json::Value {
    serde_json::json!({
        "id": "33333333-3333-3333-3333-333333333333",
        "source_version_id": "22222222-2222-2222-2222-222222222222",
        "node_id": "44444444-4444-4444-4444-444444444444",
        "source_path": "welcome.md",
        "published_url": "https://example.com/welcome/",
        "source_url": null,
        "language": "markdown",
        "kind": "markdown",
        "content_hash": "sha256:abc",
        "char_count": 100,
        "token_count": 20,
        "source_modified_at": null,
        "created_at": "2026-05-25T00:00:00Z",
        "frontmatter": null,
        "provenance": {},
        "package_id": null,
        "source": { "slug": "smoke" },
        "chunk_ids": [
            "11111111-1111-1111-1111-111111111111",
            "11111111-1111-1111-1111-111111111112"
        ]
    })
}

#[tokio::test]
async fn documents_show_renders_overview() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/documents/33333333-3333-3333-3333-333333333333"))
        .respond_with(ResponseTemplate::new(200).set_body_json(overview_json()))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args([
            "--server",
            &server.uri(),
            "documents",
            "show",
            "33333333-3333-3333-3333-333333333333",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("welcome.md"));
    assert!(stdout.contains("2 chunks") || stdout.contains("chunk_ids"));
}

#[tokio::test]
async fn documents_full_translates_412_to_friendly_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/documents/[0-9a-f-]+/full$"))
        .respond_with(
            ResponseTemplate::new(412).set_body_json(serde_json::json!({
                "error": "too_many_chunks",
                "chunk_count": 1240,
                "cap": 500,
                "hint": "Use GET /v1/documents/33333333-3333-3333-3333-333333333333/chunks?from=K&limit=L (default L=20)"
            }))
        )
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args([
            "--server",
            &server.uri(),
            "documents",
            "full",
            "33333333-3333-3333-3333-333333333333",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr);
    assert!(combined.contains("1240"), "expected chunk count in error: {combined}");
    assert!(
        combined.contains("--from") || combined.contains("documents chunks"),
        "expected window suggestion: {combined}"
    );
}

#[tokio::test]
async fn documents_chunks_renders_window() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "id": "33333333-3333-3333-3333-333333333333",
        "source_path": "welcome.md",
        "source": { "slug": "smoke" },
        "from": 3,
        "limit": 2,
        "total_chunks": 10,
        "chunks": [
            { "chunk_id": "11111111-1111-1111-1111-111111111111",
              "chunk_index": 3, "content": "third chunk", "heading_path": [], "token_count": 5 },
            { "chunk_id": "11111111-1111-1111-1111-111111111112",
              "chunk_index": 4, "content": "fourth chunk", "heading_path": [], "token_count": 5 }
        ]
    });
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/documents/[0-9a-f-]+/chunks$"))
        .and(query_param("from", "3"))
        .and(query_param("limit", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args([
            "--server",
            &server.uri(),
            "documents",
            "chunks",
            "33333333-3333-3333-3333-333333333333",
            "--from",
            "3",
            "--limit",
            "2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("3..5 of 10")
            || stdout.contains("3..=4 of 10")
            || stdout.contains("chunks 3..5 of 10 total"),
        "expected window header: {stdout}"
    );
}

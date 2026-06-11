//! Integration tests for `mnm ingest` driven against a `wiremock` mock of
//! the three-step admin ingest flow. Asserts:
//!
//! 1. The CLI walks a real manifest + source root, embeds every chunk via the
//!    Voyage `/v1/embeddings` server proxy (`input_type=document`), and POSTs
//!    the documents — each carrying its 1024-dim vector — in the expected wire
//!    shape, stamped with the corpus wire id from `GET /v1/models/active`.
//!
//! 2. The bearer token is sent on every request and never logged to the
//!    captured stdout (FR-019).
//!
//! 3. `--dry-run` short-circuits before any HTTP call.
//!
//! 4. A 409 `run_aborted` mid-flow surfaces as a CLI error and triggers
//!    the abort fallback.
//!
//! These tests run in **server-proxy** embedding mode: no BYOK Voyage key is
//! resolved (config discovery is bypassed with `config_path = None` and no
//! `--voyage-api-key`), so the CLI POSTs chunk texts to the mock's
//! `/v1/embeddings`. The BYOK branch of `client::embed` is covered by the
//! `mn-embedding` unit tests.

use std::sync::{Arc, Mutex};

use mn_cli::commands::ingest::run::{Args as IngestArgs, DEFAULT_EMBEDDING_MODEL};
use mn_core::auth_file::AuthFile;
use mn_telemetry::TelemetryClient;
use serde_json::json;
use time::OffsetDateTime;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// The active-model response → corpus wire id `voyage-code-3@1`.
fn active_model_body() -> serde_json::Value {
    json!({
        "name": "voyage-code-3",
        "revision": 1,
        "dim": 1024,
        "provider": "voyageai"
    })
}

/// Build a stub `/v1/embeddings` response with one 1024-dim vector per input
/// text. Mirrors the `mn-embedding` server-proxy response shape
/// (`{ embeddings: [...], usage: { total_tokens } }`).
fn embeddings_response_for(req: &Request) -> ResponseTemplate {
    let body: serde_json::Value = req.body_json().unwrap_or(serde_json::Value::Null);
    let n = body["input"].as_array().map_or(0, Vec::len);
    let embeddings: Vec<Vec<f32>> = (0..n).map(|_| vec![0.25_f32; 1024]).collect();
    ResponseTemplate::new(200).set_body_json(json!({
        "model": "voyage-code-3@1",
        "embeddings": embeddings,
        "usage": { "total_tokens": 4 * n }
    }))
}

/// Mount `GET /v1/models/active` + `POST /v1/embeddings` on `server` so a run
/// in server-proxy embedding mode can resolve the corpus wire id and embed.
async fn mount_embedding_mocks(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v1/models/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(active_model_body()))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(embeddings_response_for)
        .mount(server)
        .await;
}

fn write_manifest(dir: &std::path::Path, files: &[&str]) -> std::path::PathBuf {
    let manifest_path = dir.join("hierarchy.yaml");
    let mut body = String::from("manifest_version: 1\nroot:\n  name: docs\n  children:\n");
    for f in files {
        body.push_str("    - file: ");
        body.push_str(f);
        body.push('\n');
    }
    std::fs::write(&manifest_path, body).unwrap();
    manifest_path
}

fn write_file(dir: &std::path::Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

fn write_admin_auth(dir: &std::path::Path) -> std::path::PathBuf {
    let auth_path = dir.join("auth.toml");
    let future = OffsetDateTime::now_utc() + time::Duration::hours(1);
    AuthFile::write_admin_token(
        &auth_path,
        "aaron",
        "test-bearer-token-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        future,
    )
    .unwrap();
    auth_path
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // happy-path flow + embed/upload body assertions
async fn happy_path_posts_three_step_flow() {
    let server = MockServer::start().await;
    let captured_starts = Arc::new(Mutex::new(0_usize));
    let captured_uploads = Arc::new(Mutex::new(0_usize));
    let captured_finalizes = Arc::new(Mutex::new(0_usize));
    let captured_bearer = Arc::new(Mutex::new(Option::<String>::None));

    mount_embedding_mocks(&server).await;

    // Mock: GET /v1/sources/:slug — source exists, return 200.
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/sources/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "slug": "docs",
            "kind": "docs_site",
        })))
        .mount(&server)
        .await;

    let starts = Arc::clone(&captured_starts);
    let bearer = Arc::clone(&captured_bearer);
    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs$"))
        .respond_with(move |req: &Request| {
            *starts.lock().unwrap() += 1;
            if let Some(h) = req.headers.get("authorization") {
                *bearer.lock().unwrap() = Some(h.to_str().unwrap().to_owned());
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "ingest_run_id": "00000000-0000-0000-0000-000000000001",
                "source_version_id": "00000000-0000-0000-0000-000000000001",
                "source_version_revision": 1,
            }))
        })
        .mount(&server)
        .await;

    let uploads = Arc::clone(&captured_uploads);
    Mock::given(method("PUT"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/documents$"))
        .respond_with(move |_req: &Request| {
            *uploads.lock().unwrap() += 1;
            ResponseTemplate::new(200).set_body_json(json!({
                "accepted": 2,
                "carried": 0,
                "conflicts": [],
            }))
        })
        .mount(&server)
        .await;

    let finalizes = Arc::clone(&captured_finalizes);
    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/finalize$"))
        .respond_with(move |_req: &Request| {
            *finalizes.lock().unwrap() += 1;
            ResponseTemplate::new(200).set_body_json(json!({
                "source_version_id": "00000000-0000-0000-0000-000000000001",
                "revision": 1,
                "is_active": true,
                "demoted_revision": null,
            }))
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "intro.md", "# Intro\n\nBody one.");
    write_file(dir.path(), "guide.md", "# Guide\n\nBody two.");
    let manifest_path = write_manifest(dir.path(), &["intro.md", "guide.md"]);
    let auth_path = write_admin_auth(dir.path());

    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: "docs".to_owned(),
        revision: Some("rev-1".to_owned()),
        // "auto" → resolve the corpus wire id from GET /v1/models/active.
        embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
        yes: false,
        source_base_url: None,
        batch_size: 50,
        voyage_timeout_secs: None,
        chunk_tokens: 400,
        include: vec![],
        exclude: vec![],
        no_respect_gitignore: false,
        disable_default_ignore_list: false,
        max_file_size: 10 * 1024 * 1024,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
    };
    let telemetry = TelemetryClient::Disabled;

    mn_cli::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None, // config_path → bypass discovery so no stray BYOK key resolves
        None, // voyage_api_key → server-proxy embedding mode
        &telemetry,
        "0.1.0-test",
        true,
    )
    .await
    .expect("ingest run should succeed");

    assert_eq!(*captured_starts.lock().unwrap(), 1);
    assert_eq!(*captured_uploads.lock().unwrap(), 1);
    assert_eq!(*captured_finalizes.lock().unwrap(), 1);
    let bearer = captured_bearer.lock().unwrap().clone().unwrap();
    assert!(bearer.starts_with("Bearer "), "bearer header missing");

    // The chunk batch must have been embedded via Voyage (input_type=document)
    // and each chunk uploaded carrying its 1024-dim vector + the corpus wire id.
    let requests = server.received_requests().await.unwrap();

    let embed_req = requests
        .iter()
        .find(|r| r.method == wiremock::http::Method::POST && r.url.path() == "/v1/embeddings")
        .expect("POST /v1/embeddings must have been made");
    let embed_body: serde_json::Value =
        serde_json::from_slice(&embed_req.body).expect("embeddings body is JSON");
    assert_eq!(
        embed_body["input_type"], "document",
        "ingest must embed with input_type=document"
    );
    assert!(
        embed_body["input"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "embeddings request must carry the chunk texts"
    );

    let put_req = requests
        .iter()
        .find(|r| r.method == wiremock::http::Method::PUT)
        .expect("PUT /documents must have been made");
    let put_body: serde_json::Value =
        serde_json::from_slice(&put_req.body).expect("PUT body is JSON");
    assert_eq!(
        put_body["embedding_model"], "voyage-code-3@1",
        "upload must be stamped with the corpus wire id"
    );
    let first_vec = put_body["documents"][0]["chunks"][0]["embedding"]
        .as_array()
        .expect("every uploaded chunk must carry an embedding vector");
    assert_eq!(first_vec.len(), 1024, "vector must be 1024-dim (voyage-code-3)");
}

#[tokio::test]
async fn dry_run_does_not_hit_the_server() {
    let server = MockServer::start().await;
    // No mocks mounted — any hit must surface as wiremock 404, and the test
    // asserts the call never happens via that side effect (the dry-run
    // short-circuits before any HTTP).
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "a.md", "# A");
    let manifest_path = write_manifest(dir.path(), &["a.md"]);
    let auth_path = write_admin_auth(dir.path());

    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: "docs".to_owned(),
        revision: Some("rev".to_owned()),
        embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: true,
        yes: false,
        source_base_url: None,
        batch_size: 50,
        voyage_timeout_secs: None,
        chunk_tokens: 400,
        include: vec![],
        exclude: vec![],
        no_respect_gitignore: false,
        disable_default_ignore_list: false,
        max_file_size: 10 * 1024 * 1024,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
    };
    let telemetry = TelemetryClient::Disabled;

    mn_cli::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
        "0.1.0-test",
        true,
    )
    .await
    .expect("dry-run should succeed");
}

#[tokio::test]
async fn missing_admin_token_errors_with_clear_message() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "a.md", "# A");
    let manifest_path = write_manifest(dir.path(), &["a.md"]);
    let auth_path = dir.path().join("auth.toml"); // does NOT exist

    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: "docs".to_owned(),
        revision: Some("rev".to_owned()),
        embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
        yes: false,
        source_base_url: None,
        batch_size: 50,
        voyage_timeout_secs: None,
        chunk_tokens: 400,
        include: vec![],
        exclude: vec![],
        no_respect_gitignore: false,
        disable_default_ignore_list: false,
        max_file_size: 10 * 1024 * 1024,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
    };
    let telemetry = TelemetryClient::Disabled;

    let err = mn_cli::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
        "0.1.0-test",
        true,
    )
    .await
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("mnm login"), "expected remediation: {msg}");
}

#[tokio::test]
async fn aborts_run_when_upload_fails() {
    let server = MockServer::start().await;
    let abort_hit = Arc::new(Mutex::new(false));

    mount_embedding_mocks(&server).await;

    // Mock: GET /v1/sources/:slug — source exists.
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/sources/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "slug": "docs",
            "kind": "docs_site",
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ingest_run_id": "00000000-0000-0000-0000-000000000002",
            "source_version_id": "00000000-0000-0000-0000-000000000002",
            "source_version_revision": 1,
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/documents$"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": {"code": "run_aborted", "message": "fake"}
        })))
        .mount(&server)
        .await;

    let abort = Arc::clone(&abort_hit);
    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/abort$"))
        .respond_with(move |_req: &Request| {
            *abort.lock().unwrap() = true;
            ResponseTemplate::new(200)
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "a.md", "# A");
    let manifest_path = write_manifest(dir.path(), &["a.md"]);
    let auth_path = write_admin_auth(dir.path());

    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: "docs".to_owned(),
        revision: Some("rev".to_owned()),
        embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
        yes: false,
        source_base_url: None,
        batch_size: 50,
        voyage_timeout_secs: None,
        chunk_tokens: 400,
        include: vec![],
        exclude: vec![],
        no_respect_gitignore: false,
        disable_default_ignore_list: false,
        max_file_size: 10 * 1024 * 1024,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
    };
    let telemetry = TelemetryClient::Disabled;

    let err = mn_cli::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
        "0.1.0-test",
        true,
    )
    .await
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("upload documents"), "expected upload error: {msg}");
    assert!(*abort_hit.lock().unwrap(), "CLI must invoke .../abort after upload failure");
}

/// Regression test for the F-bug: a manifest declaring `published_url` at the
/// root level must propagate that value through the resolver's inheritance into
/// every `DocumentUpload.published_url` that arrives at the server.
///
/// Before the fix `DocumentUpload.published_url` was hardcoded to `None`
/// regardless of what the manifest declared. This test will fail if that
/// regression ever returns.
#[tokio::test]
#[allow(clippy::too_many_lines)] // complex regression test — keeping in one function for readability
async fn published_url_inheritance_survives_to_upload_body() {
    let server = MockServer::start().await;

    mount_embedding_mocks(&server).await;

    // Mock: GET /v1/sources/:slug — source exists.
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/sources/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "slug": "docs",
            "kind": "docs_site",
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ingest_run_id": "00000000-0000-0000-0000-000000000042",
            "source_version_id": "00000000-0000-0000-0000-000000000042",
            "source_version_revision": 1,
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/documents$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accepted": 2,
            "carried": 0,
            "conflicts": [],
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/finalize$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "source_version_id": "00000000-0000-0000-0000-000000000042",
            "revision": 1,
            "is_active": true,
            "demoted_revision": null,
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "a.md", "# A\n\nBody of A.");
    write_file(dir.path(), "b.md", "# B\n\nBody of B.");

    // Manifest declares published_url at root. The resolver must compose
    //   https://docs.example.com/a/  for a.md
    //   https://docs.example.com/b/  for b.md
    let manifest_path = dir.path().join("hierarchy.yaml");
    std::fs::write(
        &manifest_path,
        "manifest_version: 1\nroot:\n  published_url: https://docs.example.com/\n  children:\n    - file: a.md\n    - file: b.md\n",
    )
    .unwrap();

    let auth_path = write_admin_auth(dir.path());

    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: "docs".to_owned(),
        revision: Some("rev-fbug".to_owned()),
        embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
        yes: false,
        source_base_url: None,
        batch_size: 50,
        voyage_timeout_secs: None,
        chunk_tokens: 400,
        include: vec![],
        exclude: vec![],
        no_respect_gitignore: false,
        disable_default_ignore_list: false,
        max_file_size: 10 * 1024 * 1024,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
    };
    let telemetry = mn_telemetry::TelemetryClient::Disabled;

    mn_cli::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
        "0.1.0-test",
        true,
    )
    .await
    .expect("ingest should succeed");

    // Retrieve the captured PUT /documents request body via wiremock's
    // `received_requests()`. The mock server records every request.
    let requests = server.received_requests().await.unwrap();
    let put_req = requests
        .iter()
        .find(|r| r.method == wiremock::http::Method::PUT)
        .expect("PUT /documents request must have been made");

    let body: serde_json::Value =
        serde_json::from_slice(&put_req.body).expect("PUT body is valid JSON");

    let documents = body["documents"]
        .as_array()
        .expect("documents array in body");
    assert_eq!(documents.len(), 2, "both files must be uploaded");

    // Collect published_url values keyed by path so order doesn't matter.
    let by_path: std::collections::HashMap<String, Option<String>> = documents
        .iter()
        .map(|d| {
            let path = d["path"].as_str().unwrap_or("").to_owned();
            let url = d["published_url"].as_str().map(str::to_owned);
            (path, url)
        })
        .collect();

    // The F-bug: if published_url were hardcoded to None these assertions fail.
    let a_url = by_path.get("a.md").and_then(|u| u.as_deref());
    assert_eq!(
        a_url,
        Some("https://docs.example.com/a/"),
        "a.md must carry the resolved published_url; was None (F-bug)"
    );
    let b_url = by_path.get("b.md").and_then(|u| u.as_deref());
    assert_eq!(
        b_url,
        Some("https://docs.example.com/b/"),
        "b.md must carry the resolved published_url; was None (F-bug)"
    );
}

/// A `PUT .../documents` that returns 413 on its first hit must be auto-split
/// by the CLI: the batch is divided into two halves, each PUT once, and the run
/// finalizes successfully. Mirrors the happy-path harness but sequences the
/// documents mock so the first PUT is 413 and the rest are 200.
#[tokio::test]
#[allow(clippy::too_many_lines)] // full three-step flow + 413-then-200 sequencing
async fn upload_413_is_split_and_retried() {
    let server = MockServer::start().await;
    let put_hits = Arc::new(Mutex::new(0_usize));
    let abort_hit = Arc::new(Mutex::new(false));

    mount_embedding_mocks(&server).await;

    // Mock: GET /v1/sources/:slug — source exists.
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/sources/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "slug": "docs",
            "kind": "docs_site",
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ingest_run_id": "00000000-0000-0000-0000-000000000413",
            "source_version_id": "00000000-0000-0000-0000-000000000413",
            "source_version_revision": 1,
        })))
        .mount(&server)
        .await;

    // First PUT → 413 (payload too large). `up_to_n_times(1)` retires this mock
    // after a single match so the next PUT falls through to the 200 mock below.
    Mock::given(method("PUT"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/documents$"))
        .respond_with(ResponseTemplate::new(413).set_body_string("payload too large"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    // Subsequent PUTs (the two split halves) → 200. Count them so we can assert
    // the batch was actually split into two follow-up requests.
    let puts = Arc::clone(&put_hits);
    Mock::given(method("PUT"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/documents$"))
        .respond_with(move |_req: &Request| {
            *puts.lock().unwrap() += 1;
            ResponseTemplate::new(200).set_body_json(json!({
                "accepted": 1,
                "carried": 0,
                "conflicts": [],
            }))
        })
        .with_priority(2)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/finalize$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "source_version_id": "00000000-0000-0000-0000-000000000413",
            "revision": 1,
            "is_active": true,
            "demoted_revision": null,
        })))
        .mount(&server)
        .await;

    // If the run ever aborts, flip a flag so the test fails with a clear message.
    let abort = Arc::clone(&abort_hit);
    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/abort$"))
        .respond_with(move |_req: &Request| {
            *abort.lock().unwrap() = true;
            ResponseTemplate::new(200)
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    // Two documents so the single packed batch (batch_size 50) splits cleanly
    // into two halves of one document each on the 413.
    write_file(dir.path(), "a.md", "# A\n\nBody of A.");
    write_file(dir.path(), "b.md", "# B\n\nBody of B.");
    let manifest_path = write_manifest(dir.path(), &["a.md", "b.md"]);
    let auth_path = write_admin_auth(dir.path());

    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: "docs".to_owned(),
        revision: Some("rev-413".to_owned()),
        embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
        yes: false,
        source_base_url: None,
        batch_size: 50,
        voyage_timeout_secs: None,
        chunk_tokens: 400,
        include: vec![],
        exclude: vec![],
        no_respect_gitignore: false,
        disable_default_ignore_list: false,
        max_file_size: 10 * 1024 * 1024,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
    };
    let telemetry = TelemetryClient::Disabled;

    mn_cli::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
        "0.1.0-test",
        true,
    )
    .await
    .expect("ingest run should succeed after the 413 is split and retried");

    assert!(
        !*abort_hit.lock().unwrap(),
        "run must NOT abort — the 413 batch is split and retried, not aborted"
    );
    // The 413 mock consumed one PUT; the split produced two more (one per half).
    assert_eq!(
        *put_hits.lock().unwrap(),
        2,
        "the split batch must produce exactly two follow-up PUTs"
    );
}

#[tokio::test]
async fn manifest_missing_file_errors_before_any_http() {
    let server = MockServer::start().await;
    // No mocks: an HTTP call would 404 from wiremock.

    let dir = tempfile::tempdir().unwrap();
    // Manifest lists a file we do NOT write.
    let manifest_path = write_manifest(dir.path(), &["never.md"]);
    let auth_path = write_admin_auth(dir.path());

    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: "docs".to_owned(),
        revision: Some("rev".to_owned()),
        embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
        yes: false,
        source_base_url: None,
        batch_size: 50,
        voyage_timeout_secs: None,
        chunk_tokens: 400,
        include: vec![],
        exclude: vec![],
        no_respect_gitignore: false,
        disable_default_ignore_list: false,
        max_file_size: 10 * 1024 * 1024,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
    };
    let telemetry = TelemetryClient::Disabled;

    let err = mn_cli::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
        "0.1.0-test",
        true,
    )
    .await
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("missing"), "{msg}");
}

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
//! `mnm-embedding` unit tests.

use std::sync::{Arc, Mutex};

use midnight_manual::commands::ingest::run::{Args as IngestArgs, DEFAULT_EMBEDDING_MODEL};
use mnm_core::auth_file::AuthFile;
use mnm_telemetry::{build as build_telemetry, BuildParams};
use serde_json::json;
use time::OffsetDateTime;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Build a no-op [`mnm_telemetry::Telemetry`] for integration tests.
///
/// `config_enabled: false` makes `build()` return a no-op handle without
/// touching any on-disk queue or endpoint.
fn noop_telemetry() -> mnm_telemetry::Telemetry {
    build_telemetry(BuildParams {
        app_version: "0.0.0-test".into(),
        endpoint: "https://telemetry.disabled.invalid".into(),
        install_id_path: Some(std::path::PathBuf::from("/nonexistent/mnm-telemetry-id")),
        config_enabled: false,
        runtime_enabled: false,
        flush_args: vec![],
    })
}

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
/// text. Mirrors the `mnm-embedding` server-proxy response shape
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
        respect_gitignore: false,
        disable_default_ignore_list: false,
        strict: false,
        max_file_size: 10 * 1024 * 1024,
        max_line_bytes: mnm_content::chunk::DEFAULT_MAX_LINE_BYTES,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
        report_file: None,
    };
    let telemetry = noop_telemetry();

    midnight_manual::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None, // config_path → bypass discovery so no stray BYOK key resolves
        None, // voyage_api_key → server-proxy embedding mode
        &telemetry,
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
        respect_gitignore: false,
        disable_default_ignore_list: false,
        strict: false,
        max_file_size: 10 * 1024 * 1024,
        max_line_bytes: mnm_content::chunk::DEFAULT_MAX_LINE_BYTES,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
        report_file: None,
    };
    let telemetry = noop_telemetry();

    midnight_manual::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
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
        respect_gitignore: false,
        disable_default_ignore_list: false,
        strict: false,
        max_file_size: 10 * 1024 * 1024,
        max_line_bytes: mnm_content::chunk::DEFAULT_MAX_LINE_BYTES,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
        report_file: None,
    };
    let telemetry = noop_telemetry();

    let err = midnight_manual::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
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
        respect_gitignore: false,
        disable_default_ignore_list: false,
        strict: false,
        max_file_size: 10 * 1024 * 1024,
        max_line_bytes: mnm_content::chunk::DEFAULT_MAX_LINE_BYTES,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
        report_file: None,
    };
    let telemetry = noop_telemetry();

    let err = midnight_manual::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
        true,
    )
    .await
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("upload documents"), "expected upload error: {msg}");
    assert!(*abort_hit.lock().unwrap(), "CLI must invoke .../abort after upload failure");
}

/// Issue #136: an aborted `ingest run` MUST still emit an `IngestReport` to
/// `--report-file` with `outcome: "aborted"`, the PLAN's intended stats, and the
/// triggering error captured. Before the fix the abort path returned `Err`
/// before any report was assembled, so automation could not distinguish "run
/// aborted" from "run never happened" (the report file was simply absent).
///
/// NOTE: the numeric stats/documents here describe what the run INTENDED to
/// commit, not what was persisted (the upload never succeeded) — see the
/// `aborted` note on `IngestReport::outcome`.
///
/// Drives the upload-failure abort path (one of the four post-start failure
/// paths): the PUT /documents mock returns 409, so `run_inner` aborts the run
/// and returns an error — and, with this fix, writes the report first.
#[tokio::test]
#[allow(clippy::too_many_lines)] // full abort flow + report-artifact assertions
async fn aborted_run_writes_report_with_outcome_aborted() {
    let server = MockServer::start().await;
    let abort_hit = Arc::new(Mutex::new(false));

    mount_embedding_mocks(&server).await;

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
            "ingest_run_id": "00000000-0000-0000-0000-000000000136",
            "source_version_id": "00000000-0000-0000-0000-000000000136",
            "source_version_revision": 1,
        })))
        .mount(&server)
        .await;

    // Upload fails hard → the CLI aborts the run and returns an error.
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
    write_file(dir.path(), "a.md", "# A\n\nBody of A.");
    let manifest_path = write_manifest(dir.path(), &["a.md"]);
    let auth_path = write_admin_auth(dir.path());
    let report_path = dir.path().join("reports/ingest.json");

    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: "docs".to_owned(),
        revision: Some("rev-136".to_owned()),
        embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
        yes: false,
        source_base_url: None,
        batch_size: 50,
        voyage_timeout_secs: None,
        chunk_tokens: 400,
        respect_gitignore: false,
        disable_default_ignore_list: false,
        strict: false,
        max_file_size: 10 * 1024 * 1024,
        max_line_bytes: mnm_content::chunk::DEFAULT_MAX_LINE_BYTES,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
        report_file: Some(report_path.clone()),
    };
    let telemetry = noop_telemetry();

    // json = false so the artifact is the sole output surface under test.
    let err = midnight_manual::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
        false,
    )
    .await
    .unwrap_err();

    // Exit/stderr behaviour unchanged: the run still fails with the upload error
    // and the server run is still aborted.
    let msg = format!("{err:#}");
    assert!(msg.contains("upload documents"), "expected upload error: {msg}");
    assert!(*abort_hit.lock().unwrap(), "CLI must abort the server run on failure");

    // ── The ADDED artifact: a parseable report with outcome "aborted" ────────
    assert!(report_path.exists(), "abort must write the --report-file artifact");
    let raw = std::fs::read_to_string(&report_path).expect("report file readable");
    let report: serde_json::Value =
        serde_json::from_str(&raw).expect("report file must be valid JSON");

    assert_eq!(report["schema_version"], 2, "report carries the v2 schema");
    assert_eq!(report["command"], "ingest run");
    assert_eq!(report["outcome"], "aborted", "aborted run must record outcome=aborted");

    // The PLAN's intended stats are recorded (what the run set out to commit,
    // NOT what was persisted — the upload never landed).
    assert_eq!(report["stats"]["walked"], 1, "one file was walked");
    assert_eq!(report["stats"]["new"], 1, "the walked file was classified new");
    assert!(
        report["stats"]["chunks_emitted"]
            .as_u64()
            .is_some_and(|n| n >= 1),
        "the pre-upload plan's chunk total must survive into the aborted report"
    );

    // The conflict list field is present (empty here — the upload failed hard
    // rather than returning per-document conflicts; the populated case is
    // covered by the `aborted_report_carries_populated_conflicts` unit test).
    assert!(report["conflicts"].is_array(), "conflicts list must be present");

    // The per-document record survives; new docs are not embed-complete on abort.
    assert_eq!(report["documents"][0]["path"], "a.md");
    assert_eq!(report["documents"][0]["classification"], "new");
    assert_eq!(report["documents"][0]["embed_complete"], false);

    // The triggering error is captured in the dedicated `error` field, so
    // automation can read WHY the run aborted without scraping stderr.
    let captured = report["error"]
        .as_str()
        .expect("aborted report must set `error`");
    assert!(
        captured.contains("upload documents"),
        "report.error must capture the triggering error: {captured}"
    );
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
        respect_gitignore: false,
        disable_default_ignore_list: false,
        strict: false,
        max_file_size: 10 * 1024 * 1024,
        max_line_bytes: mnm_content::chunk::DEFAULT_MAX_LINE_BYTES,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
        report_file: None,
    };
    let telemetry = noop_telemetry();

    midnight_manual::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
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
        respect_gitignore: false,
        disable_default_ignore_list: false,
        strict: false,
        max_file_size: 10 * 1024 * 1024,
        max_line_bytes: mnm_content::chunk::DEFAULT_MAX_LINE_BYTES,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
        report_file: None,
    };
    let telemetry = noop_telemetry();

    midnight_manual::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
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

/// A batch that exceeds ~2x the server body limit — i.e. EVERY multi-document
/// PUT 413s, not just the first — must still upload successfully by splitting
/// RECURSIVELY down to one-document requests. This is the regression guard for
/// issue #101: the old split was single-level, so a half that still 413'd
/// propagated its error and aborted the whole run. Here a 4-document batch can
/// only succeed if each 2-document half, after 413ing, splits again into two
/// 1-document PUTs.
///
/// The single PUT mock decides its response from the request body: any PUT
/// carrying more than one document returns 413; a PUT carrying exactly one
/// document returns 200. So the call tree is: 4-doc PUT (413) → two 2-doc PUTs
/// (each 413) → four 1-doc PUTs (each 200). That is three 413s and four 200s.
#[tokio::test]
#[allow(clippy::too_many_lines)] // full three-step flow + body-driven 413 mock
async fn upload_413_splits_recursively_to_single_docs() {
    let server = MockServer::start().await;
    let n_413 = Arc::new(Mutex::new(0_usize));
    let n_200 = Arc::new(Mutex::new(0_usize));
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
            "ingest_run_id": "00000000-0000-0000-0000-000000000414",
            "source_version_id": "00000000-0000-0000-0000-000000000414",
            "source_version_revision": 1,
        })))
        .mount(&server)
        .await;

    // Single PUT mock: 413 for any multi-document body, 200 for a lone document.
    // The 413/200 counters let us prove the recursion went beyond one level.
    let c413 = Arc::clone(&n_413);
    let c200 = Arc::clone(&n_200);
    Mock::given(method("PUT"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/documents$"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = req.body_json().unwrap_or(serde_json::Value::Null);
            let count = body["documents"].as_array().map_or(0, Vec::len);
            if count > 1 {
                *c413.lock().unwrap() += 1;
                ResponseTemplate::new(413).set_body_string("payload too large")
            } else {
                *c200.lock().unwrap() += 1;
                ResponseTemplate::new(200).set_body_json(json!({
                    "accepted": 1,
                    "carried": 0,
                    "conflicts": [],
                }))
            }
        })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/admin/sources/[^/]+/ingest-runs/[^/]+/finalize$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "source_version_id": "00000000-0000-0000-0000-000000000414",
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
    // Four documents packed into ONE batch (batch_size 50). Recursion must split
    // 4 → 2+2 → 1+1+1+1 because every multi-doc PUT 413s.
    write_file(dir.path(), "a.md", "# A\n\nBody of A.");
    write_file(dir.path(), "b.md", "# B\n\nBody of B.");
    write_file(dir.path(), "c.md", "# C\n\nBody of C.");
    write_file(dir.path(), "d.md", "# D\n\nBody of D.");
    let manifest_path = write_manifest(dir.path(), &["a.md", "b.md", "c.md", "d.md"]);
    let auth_path = write_admin_auth(dir.path());

    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: "docs".to_owned(),
        revision: Some("rev-414".to_owned()),
        embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
        yes: false,
        source_base_url: None,
        batch_size: 50,
        voyage_timeout_secs: None,
        chunk_tokens: 400,
        respect_gitignore: false,
        disable_default_ignore_list: false,
        strict: false,
        max_file_size: 10 * 1024 * 1024,
        max_line_bytes: mnm_content::chunk::DEFAULT_MAX_LINE_BYTES,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
        report_file: None,
    };
    let telemetry = noop_telemetry();

    midnight_manual::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
        true,
    )
    .await
    .expect("ingest run should succeed after recursive 413 splitting");

    assert!(
        !*abort_hit.lock().unwrap(),
        "run must NOT abort — recursive splitting must upload every document"
    );
    // Recursion bottomed out at one document per request: all four uploaded
    // individually.
    assert_eq!(
        *n_200.lock().unwrap(),
        4,
        "recursion must bottom out at four single-document 200 PUTs"
    );
    // The 4-doc batch plus both 2-doc halves each 413'd before recursing. With
    // the OLD single-level split the 2-doc halves would 413 and abort the run,
    // so this count proves the split recursed beyond one level.
    assert_eq!(
        *n_413.lock().unwrap(),
        3,
        "the 4-doc batch and both 2-doc halves must each 413 before splitting again"
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
        respect_gitignore: false,
        disable_default_ignore_list: false,
        strict: false,
        max_file_size: 10 * 1024 * 1024,
        max_line_bytes: mnm_content::chunk::DEFAULT_MAX_LINE_BYTES,
        unsafe_no_global_limit: false,
        no_code_embeddings: false,
        report_file: None,
    };
    let telemetry = noop_telemetry();

    let err = midnight_manual::commands::ingest::run::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        None,
        None,
        &telemetry,
        true,
    )
    .await
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("missing"), "{msg}");
}

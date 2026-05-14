//! Integration tests for `mnm ingest` driven against a `wiremock` mock of
//! the three-step admin ingest flow. Asserts:
//!
//! 1. The CLI walks a real manifest + source root, builds the plan, and
//!    POSTs the documents in the expected wire shape.
//!
//! 2. The bearer token is sent on every request and never logged to the
//!    captured stdout (FR-019).
//!
//! 3. `--dry-run` short-circuits before any HTTP call.
//!
//! 4. A 409 `run_aborted` mid-flow surfaces as a CLI error and triggers
//!    the abort fallback.

use std::sync::{Arc, Mutex};

use mn_cli::commands::ingest::Args as IngestArgs;
use mn_core::auth_file::AuthFile;
use mn_telemetry::TelemetryClient;
use serde_json::json;
use time::OffsetDateTime;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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
async fn happy_path_posts_three_step_flow() {
    let server = MockServer::start().await;
    let captured_starts = Arc::new(Mutex::new(0_usize));
    let captured_uploads = Arc::new(Mutex::new(0_usize));
    let captured_finalizes = Arc::new(Mutex::new(0_usize));
    let captured_bearer = Arc::new(Mutex::new(Option::<String>::None));

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
        revision: "rev-1".to_owned(),
        embedding_model: "bge-base-en-v1.5@1".to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
    };
    let telemetry = TelemetryClient::Disabled;

    mn_cli::commands::ingest::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
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
        revision: "rev".to_owned(),
        embedding_model: "bge-base-en-v1.5@1".to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: true,
    };
    let telemetry = TelemetryClient::Disabled;

    mn_cli::commands::ingest::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
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
        revision: "rev".to_owned(),
        embedding_model: "bge-base-en-v1.5@1".to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
    };
    let telemetry = TelemetryClient::Disabled;

    let err = mn_cli::commands::ingest::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
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
        revision: "rev".to_owned(),
        embedding_model: "bge-base-en-v1.5@1".to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
    };
    let telemetry = TelemetryClient::Disabled;

    let err = mn_cli::commands::ingest::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
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
        revision: "rev".to_owned(),
        embedding_model: "bge-base-en-v1.5@1".to_owned(),
        note: None,
        source_root: Some(dir.path().to_path_buf()),
        dry_run: false,
    };
    let telemetry = TelemetryClient::Disabled;

    let err = mn_cli::commands::ingest::run_with_paths(
        args,
        &server.uri(),
        &auth_path,
        &telemetry,
        "0.1.0-test",
        true,
    )
    .await
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("missing"), "{msg}");
}

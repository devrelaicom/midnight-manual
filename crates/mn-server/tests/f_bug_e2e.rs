//! End-to-end Postgres-backed regression for the F-bug fix from PR #50.
//!
//! The F-bug (spec §3.4 of `docs/superpowers/specs/2026-05-25-ingest-ux-design.md`)
//! was that `mn_cli`'s `DocumentUpload` hardcoded five document fields to
//! `None`/`0` regardless of what the manifest declared:
//!
//! - `published_url`
//! - `source_url`
//! - `source_modified_at`
//! - `language`
//! - `token_count`
//!
//! PR #50 fixed the wiring so these values flow from `ResolvedLeaf`
//! (manifest inheritance + filesystem metadata + the new
//! `mn_content::language` lookup + tokens approximation) through
//! `PlannedDocument` and onto the wire.
//!
//! The existing in-tree regression (`crates/mn-cli/tests/ingest_integration.rs::
//! published_url_inheritance_survives_to_upload_body`) verifies the **wire**
//! shape against a `wiremock` mock — that catches the obvious case where the
//! CLI sends `None` again but does NOT prove the server schema accepts the
//! field and persists it across the full ingest lifecycle.
//!
//! This test fills that gap: it boots a real `mn-server` axum app against a
//! real Postgres+pgvector (via `common::boot()`), drives the actual
//! `mn_cli::commands::ingest::run::run_with_paths` CLI entry point against
//! the live HTTP listener, then SELECTs from the `document` table to assert
//! every formerly-hardcoded field landed correctly. If `DocumentUpload` were
//! ever reverted to the F-bug shape the assertions on `published_url`,
//! `source_url`, `language`, `source_modified_at`, and `token_count` would
//! all fail.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use std::path::Path;
use std::time::Duration;

use mn_auth::{mint_jwt, Claims, Keypair, Role, SigningSecret, DEFAULT_ADMIN_TTL};
use mn_cli::commands::ingest::run::{run_with_paths, Args as IngestArgs};
use mn_core::auth_file::AuthFile;
use mn_core::types::SourceKind;
use mn_server::{app, config::ServerConfig};
use mn_store::entities::{embedding_model, source};
use mn_telemetry::TelemetryClient;
use time::OffsetDateTime;
use uuid::Uuid;

/// Subset of the `document` columns the F-bug fix is responsible for.
/// Selected from Postgres after the ingest completes.
#[derive(sqlx::FromRow, Debug)]
struct DocRow {
    source_path: String,
    published_url: Option<String>,
    source_url: Option<String>,
    language: Option<String>,
    source_modified_at: Option<OffsetDateTime>,
    token_count: i32,
}

/// Boot the axum app, bind it to an ephemeral 127.0.0.1 port, spawn the
/// `axum::serve` task, and return the base URL the CLI should hit.
async fn spawn_server(pool: sqlx::PgPool, cfg: ServerConfig) -> String {
    let app = app::build(pool, cfg).expect("build app");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("axum::serve");
    });
    // Tiny readiness wait — the listener is already accepting by the time
    // bind() returns, but axum::serve needs a tick to install its acceptor.
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

fn user_store_for(user_id: &str, kp: &Keypair) -> String {
    format!(
        r#"
schema_version = 1

[[users]]
user_id = "{user_id}"
role = "admin"
public_key = "{wire}"
created_at = "2026-05-14"
"#,
        wire = kp.public_wire(),
    )
}

fn write_admin_auth_toml(dir: &Path, user_id: &str, token: &str) -> std::path::PathBuf {
    let auth_path = dir.join("auth.toml");
    let future = OffsetDateTime::now_utc() + time::Duration::hours(1);
    AuthFile::write_admin_token(&auth_path, user_id, token, future).expect("write admin auth.toml");
    auth_path
}

/// The F-bug regression, end-to-end.
///
/// Manifest:
/// ```yaml
/// manifest_version: 1
/// root:
///   name: docs
///   published_url: https://docs.example.com/
///   children:
///     - file: a.md
///     - file: b.md
/// ```
///
/// After ingest, the `document` rows for `a.md` and `b.md` MUST have:
/// - `published_url` = the inheritance-joined URL (`.../a/`, `.../b/`)
/// - `source_url`    = the value built from `--source-base-url`
/// - `language`      = `markdown` (from `mn_content::language::from_path`)
/// - `source_modified_at IS NOT NULL` (the walker captures `mtime`)
/// - `token_count > 0`
///
/// If any of those revert to `None`/`0` the assertion message names the
/// specific field so the failure is self-explanatory.
#[tokio::test]
async fn document_metadata_persists_to_postgres_through_full_ingest() {
    let h = common::boot().await;

    // ── Pre-seed: embedding model + source ─────────────────────────────────
    embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .expect("seed embedding model");
    let source_slug = format!("f-bug-e2e-{}", Uuid::new_v4());
    let _source_id =
        source::insert(&h.pool, &source_slug, "F-Bug E2E", SourceKind::DocsSite, None, 5)
            .await
            .expect("seed source");

    // ── Build server config: admin user "aaron" + a known JWT secret ──────
    let kp = Keypair::generate();
    let user_id = "aaron";
    let jwt_secret_bytes = vec![0xAA_u8; 32];
    let cfg = ServerConfig {
        corpus_model: Some("bge-base-en-v1.5@1".to_owned()),
        user_store_body: Some(user_store_for(user_id, &kp)),
        jwt_secret: Some(jwt_secret_bytes.clone()),
        ..Default::default()
    };
    let server_url = spawn_server(h.pool.clone(), cfg).await;

    // ── Mint an admin JWT directly (skipping the challenge-response dance).
    //
    // The CLI loads this token from auth.toml and sends it as a bearer on
    // every request. Because the user store carries `aaron` with role=admin
    // and the JWT carries `sub=aaron` + `tier=admin` + `role=admin`, the
    // server's bearer middleware accepts it for `/v1/admin/*`.
    let secret = SigningSecret::from_bytes(jwt_secret_bytes).expect("32-byte secret");
    let claims = Claims::admin(user_id, Role::Admin, OffsetDateTime::now_utc(), DEFAULT_ADMIN_TTL);
    let token = mint_jwt(&secret, &claims).expect("mint admin JWT");

    // ── Author manifest + source files in a tempdir ────────────────────────
    let workdir = tempfile::tempdir().expect("tempdir");
    let auth_path = write_admin_auth_toml(workdir.path(), user_id, &token);

    let body_a = "# Alpha\n\nFirst page body — several words to bump token_count over zero.";
    let body_b = "# Bravo\n\nSecond page with distinct content for a separate row.";
    std::fs::write(workdir.path().join("a.md"), body_a).unwrap();
    std::fs::write(workdir.path().join("b.md"), body_b).unwrap();

    let manifest_path = workdir.path().join("hierarchy.yaml");
    std::fs::write(
        &manifest_path,
        // published_url at the root must propagate into both leaves via
        // `mn_content::manifest::resolve` inheritance. The resolved leaf
        // URLs are <prefix>/<basename>/ — i.e. `.../a/` and `.../b/`.
        "manifest_version: 1\n\
         root:\n\
         \x20\x20name: docs\n\
         \x20\x20published_url: https://docs.example.com/\n\
         \x20\x20children:\n\
         \x20\x20\x20\x20- file: a.md\n\
         \x20\x20\x20\x20- file: b.md\n",
    )
    .unwrap();

    // ── Drive the CLI through the real ingest pipeline ─────────────────────
    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: source_slug.clone(),
        revision: Some("rev-fbug-e2e".to_owned()),
        embedding_model: "bge-base-en-v1.5@1".to_owned(),
        note: Some("f-bug e2e regression".to_owned()),
        source_root: Some(workdir.path().to_path_buf()),
        dry_run: false,
        yes: true, // not strictly needed (source pre-seeded) but defensive
        source_base_url: Some("https://github.com/example/docs/blob/main".to_owned()),
        batch_size: 50,
        code_chunk_tokens: 400,
        code_chunk_lines: 60,
        code_chunk_overlap: 20,
        include: vec![],
        exclude: vec![],
        no_respect_gitignore: false,
        disable_default_ignore_list: false,
        max_file_size: 10 * 1024 * 1024,
    };
    let telemetry = TelemetryClient::Disabled;

    run_with_paths(args, &server_url, &auth_path, &telemetry, "0.1.0-e2e", true)
        .await
        .expect("ingest run completes against live mn-server");

    // ── Assert the F-bug fields landed in Postgres ─────────────────────────
    //
    // Query directly against the `document` table. We join via the slug
    // through `source` and the active `source_version` to be order-
    // independent and resilient to multi-version corpora.
    let rows: Vec<DocRow> = sqlx::query_as::<_, DocRow>(
        "SELECT d.source_path, d.published_url, d.source_url, d.language, \
                d.source_modified_at, d.token_count \
           FROM document d \
           JOIN source_version sv ON sv.id = d.source_version_id \
           JOIN source s ON s.id = sv.source_id \
          WHERE s.slug = $1 AND sv.status = 'active' \
          ORDER BY d.source_path",
    )
    .bind(&source_slug)
    .fetch_all(&h.pool)
    .await
    .expect("query persisted documents");

    assert_eq!(rows.len(), 2, "expected 2 documents (a.md, b.md), got {}", rows.len(),);

    for row in &rows {
        // published_url — F-bug field #1. Without the fix this is None.
        let expected_published = match row.source_path.as_str() {
            "a.md" => "https://docs.example.com/a/",
            "b.md" => "https://docs.example.com/b/",
            other => panic!("unexpected document path {other:?}"),
        };
        assert_eq!(
            row.published_url.as_deref(),
            Some(expected_published),
            "F-bug: published_url for {} should be {expected_published:?} but was {:?} \
             — manifest published_url inheritance is not surviving to Postgres",
            row.source_path,
            row.published_url,
        );

        // source_url — F-bug field #2. The CLI builds it from
        // --source-base-url + the relative path when the manifest doesn't
        // override.
        let expected_source_url =
            format!("https://github.com/example/docs/blob/main/{}", row.source_path);
        assert_eq!(
            row.source_url.as_deref(),
            Some(expected_source_url.as_str()),
            "F-bug: source_url for {} should be {expected_source_url:?} but was {:?} \
             — --source-base-url join is not surviving to Postgres",
            row.source_path,
            row.source_url,
        );

        // language — F-bug field #3. `mn_content::language::from_path`
        // returns `markdown` for `.md`.
        assert_eq!(
            row.language.as_deref(),
            Some("markdown"),
            "F-bug: language for {} should be `markdown` but was {:?} \
             — extension → language lookup is not surviving to Postgres",
            row.source_path,
            row.language,
        );

        // source_modified_at — F-bug field #4. The walker captures
        // fs::metadata().modified() at walk time.
        assert!(
            row.source_modified_at.is_some(),
            "F-bug: source_modified_at for {} should be Some(mtime) but was None \
             — walker's fs::metadata().modified() is not surviving to Postgres",
            row.source_path,
        );

        // token_count — F-bug field #5. Approximated at chunk time
        // (whitespace-split word count, summed onto the document).
        assert!(
            row.token_count > 0,
            "F-bug: token_count for {} should be > 0 but was {} \
             — chunk-time token approximation is not surviving to Postgres",
            row.source_path,
            row.token_count,
        );
    }
}

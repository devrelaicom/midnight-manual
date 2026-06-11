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
use std::sync::{Arc, RwLock};
use std::time::Duration;

use mn_auth::{mint_jwt, Claims, Keypair, Role, SigningSecret, DEFAULT_ADMIN_TTL};
use mn_cli::commands::ingest::run::{run_with_paths, Args as IngestArgs};
use mn_core::auth_file::AuthFile;
use mn_core::types::SourceKind;
use mn_embedding::voyage::VoyageEmbedder;
use mn_server::{app, config::ServerConfig};
use mn_store::entities::source;
use mn_telemetry::TelemetryClient;
use time::OffsetDateTime;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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

/// Boot the axum app with a mock Voyage embedder and resolved corpus model,
/// bind it to an ephemeral 127.0.0.1 port, spawn the `axum::serve` task,
/// and return the base URL the CLI should hit.
///
/// `voyage_mock_uri` must point at a running `wiremock::MockServer` that
/// handles `POST /v1/embeddings`. The caller keeps the `MockServer` alive for
/// the duration of the test.
async fn spawn_server(pool: sqlx::PgPool, cfg: ServerConfig, voyage_mock_uri: &str) -> String {
    // Resolve the corpus model that migration 0008 registered (voyage-code-3@1).
    let cm = mn_server::corpus_model::resolve(&pool).await.ok();
    let corpus_model = Arc::new(RwLock::new(cm));

    let limiter = mn_server::ratelimit::RateLimiter::from_config(&cfg);
    let token_limiter = mn_server::tokenlimit::TokenUsageLimiter::from_config(&cfg);

    // Point the server-side VoyageEmbedder at the local wiremock, so
    // POST /v1/embeddings is served in-process without network egress.
    let voyage = Some(Arc::new(
        VoyageEmbedder::new("test-key", "voyage-code-3", 1024, "float")
            .with_base_url(voyage_mock_uri),
    ));

    let app = app::build_with_limiter(
        pool,
        cfg,
        limiter,
        corpus_model,
        token_limiter,
        voyage,
        None,
        Arc::new(RwLock::new(None)),
    )
    .expect("build app");

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

/// Mount a dynamic `POST /v1/embeddings` mock that reads `input.len()` from
/// the request body and returns that many zero-filled 1024-dim vectors. The
/// mock remains active for the lifetime of the returned `MockServer`.
async fn voyage_mock() -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value =
                serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
            let n = body["input"].as_array().map_or(0, Vec::len);
            let data: Vec<serde_json::Value> = (0..n)
                .map(|k| {
                    serde_json::json!({
                        "embedding": vec![0.0_f32; 1024],
                        "index": k
                    })
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": data,
                "model": "voyage-code-3",
                "usage": { "total_tokens": n }
            }))
        })
        .mount(&mock)
        .await;
    mock
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

    // ── Pre-seed: source (migration 0008 already registered voyage-code-3@1) ─
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
        user_store_body: Some(user_store_for(user_id, &kp)),
        jwt_secret: Some(jwt_secret_bytes.clone()),
        ..Default::default()
    };

    // ── Start the Voyage mock and boot the server ──────────────────────────
    //
    // Keep `voyage_mock_server` alive until the test ends: dropping it shuts
    // down the mock, which would cause in-flight embedding requests to fail.
    let voyage_mock_server = voyage_mock().await;
    let server_url = spawn_server(h.pool.clone(), cfg, &voyage_mock_server.uri()).await;

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
        // "auto" → resolve the corpus wire id from the live server.
        embedding_model: mn_cli::commands::ingest::run::DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: Some("f-bug e2e regression".to_owned()),
        source_root: Some(workdir.path().to_path_buf()),
        dry_run: false,
        yes: true, // not strictly needed (source pre-seeded) but defensive
        source_base_url: Some("https://github.com/example/docs/blob/main".to_owned()),
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

    // Server-proxy embedding mode: no BYOK key (config discovery bypassed,
    // no --voyage-api-key) so the CLI embeds via the live server's
    // /v1/embeddings.
    run_with_paths(args, &server_url, &auth_path, None, None, &telemetry, "0.1.0-e2e", true)
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

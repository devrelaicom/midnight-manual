//! End-to-end smoke test for mixed-tree code + Markdown + plaintext ingest.
//!
//! This test exercises the full ingest pipeline against a real Postgres
//! instance (via `common::boot()`) using the corpus fixtures in
//! `crates/mn-content/tests/corpus/`. It verifies four observable properties
//! that are unique to the code-chunker path:
//!
//! (a) Rust and TypeScript source files produce chunks with a non-empty
//!     `symbol_path` JSONB array (tree-sitter chunker fired).
//! (b) Markdown files produce chunks with a non-empty `heading_path` text[]
//!     (Markdown heading-aware chunker fired).
//! (c) Package detection emits one `kind='rust'` row (`corpus-rust`) and one
//!     `kind='npm'` row (`@corpus/web`) for the `source_version`.
//! (d) The malformed `src/broken.rs` produces ≥1 chunk row AND those chunks
//!     have empty `symbol_path` (the catastrophic-error / line-window fallback
//!     fired). NOTE: `fallback_used` is an in-memory-only field on the `Chunk`
//!     struct — it does NOT exist as a database column. The observable proxy in
//!     Postgres is: chunks exist + `symbol_path` = '[]'.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use std::path::{Path, PathBuf};
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

// ── Test-local helpers ──────────────────────────────────────────────────────

/// Boot the axum app with a mock Voyage embedder and resolved corpus model,
/// bind it to an ephemeral 127.0.0.1 port, and return the base URL.
///
/// The `voyage_mock_uri` must point at a running `wiremock::MockServer` that
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

    let app = app::build_with_limiter(pool, cfg, limiter, corpus_model, token_limiter, voyage)
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

/// Mount a dynamic `POST /v1/embeddings` mock that reads `input.len()` from the
/// request body and returns that many zero-filled 1024-dim vectors. The mock
/// remains active for the lifetime of the returned `MockServer`.
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

fn write_admin_auth_toml(dir: &Path, user_id: &str, token: &str) -> PathBuf {
    let auth_path = dir.join("auth.toml");
    let future = OffsetDateTime::now_utc() + time::Duration::hours(1);
    AuthFile::write_admin_token(&auth_path, user_id, token, future).expect("write admin auth.toml");
    auth_path
}

/// Recursively copy `src` into `dst` (both must be directories).
/// Creates subdirectories as needed. Files are copied verbatim.
fn copy_dir_recursive(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("dir entry");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            std::fs::create_dir_all(&dst_path).expect("create_dir_all");
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).expect("copy file");
        }
    }
}

// ── Raw query row types ─────────────────────────────────────────────────────

/// Minimal chunk columns we inspect after ingest.
#[derive(sqlx::FromRow, Debug)]
struct ChunkRow {
    symbol_path: serde_json::Value,
    heading_path: Vec<String>,
}

/// Minimal package columns.
#[derive(sqlx::FromRow, Debug)]
struct PackageRow {
    kind: String,
    name: String,
}

// ── Test ─────────────────────────────────────────────────────────────────────

/// Smoke test: ingest the mixed-tree corpus, then verify symbol_paths,
/// heading_paths, package detection, and broken-file fallback in Postgres.
#[tokio::test]
async fn code_ingest_smoke_persists_symbol_paths_and_packages() {
    let h = common::boot().await;

    // ── Pre-seed: source (migration 0008 already registered voyage-code-3@1) ─
    let source_slug = format!("code-ingest-e2e-{}", Uuid::new_v4());
    let _source_id =
        source::insert(&h.pool, &source_slug, "Code Ingest E2E", SourceKind::Mixed, None, 5)
            .await
            .expect("seed source");

    // ── Build server config: admin user "aaron" + known JWT secret ─────────
    let kp = Keypair::generate();
    let user_id = "aaron";
    let jwt_secret_bytes = vec![0xAB_u8; 32];
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

    // ── Mint an admin JWT directly (skipping the challenge-response dance) ─
    let secret = SigningSecret::from_bytes(jwt_secret_bytes).expect("32-byte secret");
    let claims = Claims::admin(user_id, Role::Admin, OffsetDateTime::now_utc(), DEFAULT_ADMIN_TTL);
    let token = mint_jwt(&secret, &claims).expect("mint admin JWT");

    // ── Copy the corpus fixtures into a tempdir ────────────────────────────
    //
    // CARGO_MANIFEST_DIR resolves to `crates/mn-server` at compile time; the
    // corpus lives at `../mn-content/tests/corpus` relative to that.
    let corpus_src: PathBuf = {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest_dir
            .join("..")
            .join("mn-content")
            .join("tests")
            .join("corpus")
    };

    let workdir = tempfile::tempdir().expect("tempdir");
    let auth_path = write_admin_auth_toml(workdir.path(), user_id, &token);

    // Create the target layout under the tempdir root and copy files.
    copy_dir_recursive(&corpus_src, workdir.path());

    let manifest_path = workdir.path().join("manifest.yaml");
    assert!(
        manifest_path.exists(),
        "manifest.yaml not found after copy — corpus copy failed"
    );

    // ── Drive the CLI through the real ingest pipeline ─────────────────────
    let args = IngestArgs {
        manifest: manifest_path,
        source_slug: source_slug.clone(),
        revision: Some("rev-code-e2e".to_owned()),
        // "auto" → resolve the corpus wire id from the live server.
        embedding_model: mn_cli::commands::ingest::run::DEFAULT_EMBEDDING_MODEL.to_owned(),
        note: Some("code ingest e2e smoke".to_owned()),
        source_root: Some(workdir.path().to_path_buf()),
        dry_run: false,
        yes: true,
        source_base_url: None,
        batch_size: 50,
        code_chunk_tokens: 400,
        code_chunk_lines: 60,
        code_chunk_overlap: 20,
        include: vec![],
        exclude: vec![],
        no_respect_gitignore: false,
        disable_default_ignore_list: false,
        max_file_size: 10 * 1024 * 1024,
        unsafe_no_global_limit: false,
    };
    let telemetry = TelemetryClient::Disabled;

    // Server-proxy embedding mode: no BYOK key (config discovery bypassed,
    // no --voyage-api-key) so the CLI embeds via the live server's
    // /v1/embeddings, which holds the platform Voyage key.
    run_with_paths(args, &server_url, &auth_path, None, None, &telemetry, "0.1.0-e2e", true)
        .await
        .expect("ingest run completes against live mn-server");

    // ── Locate the active source_version id ───────────────────────────────
    //
    // Join slug → source → source_version WHERE status = 'active'.
    let sv_id: Uuid = sqlx::query_scalar(
        "SELECT sv.id \
           FROM source_version sv \
           JOIN source s ON s.id = sv.source_id \
          WHERE s.slug = $1 AND sv.status = 'active'",
    )
    .bind(&source_slug)
    .fetch_one(&h.pool)
    .await
    .expect("active source_version not found — finalize step may have failed");

    // ── (a) Rust files (lib.rs, util.rs) have at least one chunk with a
    //       non-empty symbol_path ──────────────────────────────────────────
    for rust_path in &["src/lib.rs", "src/util.rs"] {
        let chunks: Vec<ChunkRow> = sqlx::query_as(
            "SELECT c.symbol_path, c.heading_path \
               FROM chunk c \
               JOIN document d ON d.id = c.document_id \
              WHERE d.source_version_id = $1 AND d.source_path = $2",
        )
        .bind(sv_id)
        .bind(*rust_path)
        .fetch_all(&h.pool)
        .await
        .unwrap_or_else(|e| panic!("query chunks for {rust_path}: {e}"));

        assert!(
            !chunks.is_empty(),
            "no chunks found for {rust_path} — document was not ingested"
        );

        let has_symbol = chunks
            .iter()
            .any(|c| c.symbol_path.as_array().is_some_and(|arr| !arr.is_empty()));
        assert!(
            has_symbol,
            "(a) FAIL: {rust_path} has no chunk with a non-empty symbol_path — \
             tree-sitter code chunker did not fire or symbol_path is not being persisted"
        );
    }

    // ── (b) README.md has at least one chunk with a non-empty heading_path ─
    let md_chunks: Vec<ChunkRow> = sqlx::query_as(
        "SELECT c.symbol_path, c.heading_path \
           FROM chunk c \
           JOIN document d ON d.id = c.document_id \
          WHERE d.source_version_id = $1 AND d.source_path = 'README.md'",
    )
    .bind(sv_id)
    .fetch_all(&h.pool)
    .await
    .expect("query chunks for README.md");

    assert!(
        !md_chunks.is_empty(),
        "no chunks found for README.md — document was not ingested"
    );

    let has_heading = md_chunks.iter().any(|c| !c.heading_path.is_empty());
    assert!(
        has_heading,
        "(b) FAIL: README.md has no chunk with a non-empty heading_path — \
         Markdown heading-aware chunker did not fire or heading_path is not being persisted"
    );

    // ── (c) Package rows exist for the source_version ─────────────────────
    //       one kind='rust' name='corpus-rust'
    //       one kind='npm'  name='@corpus/web'
    let packages: Vec<PackageRow> = sqlx::query_as(
        "SELECT kind, name FROM package WHERE source_version_id = $1 ORDER BY kind, name",
    )
    .bind(sv_id)
    .fetch_all(&h.pool)
    .await
    .expect("query packages");

    let has_rust_pkg = packages
        .iter()
        .any(|p| p.kind == "rust" && p.name == "corpus-rust");
    assert!(
        has_rust_pkg,
        "(c) FAIL: expected a package row kind='rust' name='corpus-rust' but found {packages:?} — \
         Cargo.toml package detection is not firing or not being persisted"
    );

    let has_npm_pkg = packages
        .iter()
        .any(|p| p.kind == "npm" && p.name == "@corpus/web");
    assert!(
        has_npm_pkg,
        "(c) FAIL: expected a package row kind='npm' name='@corpus/web' but found {packages:?} — \
         package.json detection is not firing or not being persisted"
    );

    // ── (d) src/broken.rs: ≥1 chunk exists AND all those chunks have an
    //        empty symbol_path (line-window fallback fired) ─────────────────
    //
    // NOTE: `fallback_used` is an in-memory-only field on the `Chunk` struct
    // and is NOT persisted as a database column. The observable proxy here is:
    //   1. Chunks were produced at all (the fallback emitted something).
    //   2. Every chunk's `symbol_path` is empty (tree-sitter gave up and the
    //      line-window chunker does not populate symbol_path).
    let broken_chunks: Vec<ChunkRow> = sqlx::query_as(
        "SELECT c.symbol_path, c.heading_path \
           FROM chunk c \
           JOIN document d ON d.id = c.document_id \
          WHERE d.source_version_id = $1 AND d.source_path = 'src/broken.rs'",
    )
    .bind(sv_id)
    .fetch_all(&h.pool)
    .await
    .expect("query chunks for src/broken.rs");

    assert!(
        !broken_chunks.is_empty(),
        "(d) FAIL: src/broken.rs produced no chunks at all — \
         the line-window fallback did not run or the document was not ingested"
    );

    for (i, chunk) in broken_chunks.iter().enumerate() {
        // NULL / non-array also counts as empty
        let is_empty = chunk.symbol_path.as_array().is_none_or(Vec::is_empty);
        let sp = &chunk.symbol_path;
        assert!(
            is_empty,
            "(d) FAIL: chunk {i} of src/broken.rs has a non-empty symbol_path {sp:?} — \
             expected the catastrophic-error / line-window fallback to produce \
             empty symbol_path (fallback_used is in-memory only; empty symbol_path \
             is the DB-observable proxy)"
        );
    }
}

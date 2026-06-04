//! `mnm ingest <manifest>` — admin command that runs an end-to-end ingest
//! against the cloud server (Story 10).
//!
//! Flow:
//!
//! 1. Read the manifest (`hierarchy.yaml`) and validate it.
//!
//! 2. Walk the source root, parse frontmatter, run the Markdown chunker
//!    (via the orchestrator in [`mn_content::ingest`]).
//!
//! 3. Load the admin bearer from `auth.toml`.
//!
//! 4. Check that the source slug exists (`GET /v1/sources/:slug`); on 404,
//!    prompt the user (or honor `--yes`) and POST to create it.
//!
//! 5. `POST /v1/admin/sources/:slug/ingest-runs` — allocate a building
//!    source_version.
//!
//! 6. `PUT  /v1/admin/sources/:slug/ingest-runs/:id/documents` — upload
//!    documents in batches of `--batch-size` (default 25) each. Every chunk is
//!    embedded by the CLI via VoyageAI (`input_type=document`) before upload —
//!    either BYOK (Voyage direct, when a key resolves) or through the server's
//!    `/v1/embeddings` proxy — so the server never loads an embedding model.
//!
//! 7. `POST /v1/admin/sources/:slug/ingest-runs/:id/finalize` — promote
//!    the run to `active`.
//!
//! 8. Emit a single `IngestComplete` telemetry event with the per-run stats.
//!
//! On any failure between steps 5 and 8 the CLI calls `.../abort` so the
//! building source_version doesn't block the next attempt (FR-022).

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mn_content::ingest::{PlanBuilder, PriorState, WalkContext, Walker};
use mn_content::manifest::Manifest;
use mn_core::auth_file::AuthFile;
use mn_core::provenance::Provenance;
use mn_core::types::{DocumentKind, SourceKind};
use mn_embedding::client::{embed, EmbedSource};
use mn_embedding::voyage::{InputType, VoyageEmbedder};
use mn_telemetry::events::{Component, EventPayload, Outcome};
use mn_telemetry::{Event, TelemetryClient};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Sentinel embedding-model wire id used when `--embedding-model` is not
/// explicitly overridden. At runtime the CLI resolves the true corpus wire id
/// from `GET /v1/models/active`; this constant is only the `clap` default so
/// `args.embedding_model` has a value for the explicit-override comparison.
pub const DEFAULT_EMBEDDING_MODEL: &str = "auto";

/// Max input texts per Voyage embeddings sub-request. `voyage-code-3` rejects
/// requests over ~120K tokens with HTTP 400 (measured: 500 chunks / 74K tokens
/// = 200 OK; 1000 chunks / ~148K tokens = 400), so the old 1000-item ceiling
/// could overshoot. 250 items of ≤400-token chunks stays ≤100K tokens; we also
/// bound each sub-request by [`VOYAGE_MAX_TOKENS_PER_REQUEST`] for safety.
const VOYAGE_MAX_TEXTS_PER_REQUEST: usize = 250;

/// Max summed token count per Voyage embeddings sub-request. Sits below the
/// ~120K-token hard limit `voyage-code-3` enforces (over which it returns 400)
/// so a sub-batch never trips the cap regardless of per-chunk size.
const VOYAGE_MAX_TOKENS_PER_REQUEST: usize = 100_000;

/// Args for `mnm ingest run`.
#[derive(Debug, ClapArgs)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Path to the `hierarchy.yaml` manifest.
    pub manifest: PathBuf,

    /// Slug of the target source. If the source does not exist on the server,
    /// the CLI will prompt (or auto-create when `--yes` is passed).
    #[arg(long)]
    pub source_slug: String,

    /// Free-form revision label (often a git SHA). Defaults to
    /// `git rev-parse --short HEAD` in the source root; falls back to
    /// "unknown".
    #[arg(long)]
    pub revision: Option<String>,

    /// Override the embedding-model wire id (`name@revision`) recorded with the
    /// run and stamped on every uploaded chunk. When omitted (or set to
    /// `"auto"`), the CLI fetches the corpus's active model from
    /// `GET /v1/models/active` and uses that wire id. Only set this explicitly
    /// when you need to pin a specific `name@revision`.
    #[arg(long, default_value = DEFAULT_EMBEDDING_MODEL)]
    pub embedding_model: String,

    /// Optional ingest note, recorded on the source_version row.
    #[arg(long)]
    pub note: Option<String>,

    /// Override the source root directory (default: the manifest's parent dir).
    #[arg(long)]
    pub source_root: Option<PathBuf>,

    /// Dry-run: walk + build the plan, print stats, do NOT post anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Non-interactive: auto-confirm the source-create prompt.
    #[arg(long)]
    pub yes: bool,

    /// Base URL prepended to each document's repo-relative path to build
    /// `source_url` when the manifest does not supply one
    /// (e.g. `https://github.com/org/repo/blob/main/docs`).
    #[arg(long = "source-base-url")]
    pub source_base_url: Option<String>,

    /// Number of documents per upload batch (default: 25). Reduce if you hit
    /// 413 responses from the server (each chunk carries a 1024-dim vector).
    #[arg(long, default_value_t = 25)]
    pub batch_size: usize,

    /// Override the per-request timeout (seconds) for BYOK Voyage embedding
    /// calls. Precedence: this flag > `VOYAGE_TIMEOUT_SECS` env > config
    /// (default 120s). Raise it if large batches time out before Voyage
    /// finishes. The server-proxy embed path is not tuned by this flag; it
    /// uses the same 120s default.
    #[arg(long)]
    pub voyage_timeout_secs: Option<u64>,

    /// Semantic code-chunk budget in tokens.
    #[arg(long, default_value_t = 400)]
    pub code_chunk_tokens: u32,

    /// Line-window fallback size (lines).
    #[arg(long, default_value_t = 60)]
    pub code_chunk_lines: u32,

    /// Line-window fallback overlap (lines).
    #[arg(long, default_value_t = 20)]
    pub code_chunk_overlap: u32,

    /// Whitelist glob (repeatable).
    ///
    /// Fed into file-list filtering when directory discovery is used (follow-up).
    #[arg(long)]
    pub include: Vec<String>,

    /// Exclude glob (repeatable), additive over defaults + gitignore.
    ///
    /// Fed into file-list filtering when directory discovery is used (follow-up).
    #[arg(long)]
    pub exclude: Vec<String>,

    /// Disable .gitignore/.ignore filtering.
    ///
    /// Fed into file-list filtering when directory discovery is used (follow-up).
    #[arg(long)]
    pub no_respect_gitignore: bool,

    /// Disable the built-in default skip list (node_modules, target, …).
    ///
    /// Fed into file-list filtering when directory discovery is used (follow-up).
    #[arg(long)]
    pub disable_default_ignore_list: bool,

    /// Skip files larger than this many bytes.
    #[arg(long, default_value_t = 10 * 1024 * 1024)]
    pub max_file_size: u64,

    /// Admin-only: exempt THIS ingest's server-side embedding from the
    /// site-wide token cap. Ignored for BYOK/local embedding. The server
    /// enforces the admin-role check — a non-admin caller setting this is still
    /// counted against the global cap.
    #[arg(long)]
    pub unsafe_no_global_limit: bool,
}

/// Dispatch.
///
/// # Errors
///
/// Returns `anyhow::Error` if the manifest cannot be read, the source tree
/// walk fails, the auth.toml cannot be loaded, or any of the HTTP round-trips
/// fail.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    args: Args,
    server_flag: Option<&str>,
    config_path: Option<&Path>,
    voyage_api_key: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server_flag);
    let env = mn_core::config::StdEnv;
    let auth_path = mn_core::paths::auth_file_path(&env)
        .ok_or_else(|| anyhow!("could not resolve auth.toml path (set XDG_CONFIG_HOME or HOME)"))?;
    run_with_paths(
        args,
        &server_url,
        &auth_path,
        config_path,
        voyage_api_key,
        telemetry,
        cli_version,
        json,
    )
    .await
}

/// Path-explicit driver, exposed for integration tests. Returns `Result<()>`
/// for callers that only care about success/failure; see
/// [`run_with_paths_stats`] when you need the per-run [`RunStats`] (e.g. the
/// model-migration driver, which budgets at source boundaries).
///
/// # Errors
///
/// Returns the same errors as [`run`].
#[allow(clippy::too_many_arguments)]
pub async fn run_with_paths(
    args: Args,
    server_url: &str,
    auth_path: &Path,
    config_path: Option<&Path>,
    voyage_api_key: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    run_with_paths_stats(
        args,
        server_url,
        auth_path,
        config_path,
        voyage_api_key,
        telemetry,
        cli_version,
        json,
    )
    .await
    .map(|_| ())
}

/// Run a single-source ingest and return the per-run [`RunStats`] (document and
/// token counts). Identical to [`run_with_paths`] except the stats are returned
/// instead of discarded. Emits exactly one `IngestComplete` telemetry event, so
/// [`run_with_paths`] delegates here rather than duplicating the emit.
///
/// The returned `RunStats.total_tokens` sums the VoyageAI usage reported across
/// every embed call (BYOK *and* server-proxy both surface `usage.total_tokens`
/// via [`mn_embedding::client::Embedded`]). The migration driver uses
/// this to enforce a session token budget at source boundaries.
///
/// # Errors
///
/// Returns the same errors as [`run`].
#[allow(clippy::too_many_arguments)]
pub async fn run_with_paths_stats(
    args: Args,
    server_url: &str,
    auth_path: &Path,
    config_path: Option<&Path>,
    voyage_api_key: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<RunStats> {
    let started = Instant::now();
    let outcome = run_inner(&args, server_url, auth_path, config_path, voyage_api_key, json).await;

    let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    let (added, carried, deleted, batch_count, failed_batch_index, telemetry_outcome) =
        match &outcome {
            Ok(stats) => (
                stats.added,
                stats.carried,
                stats.deleted,
                stats.batch_count,
                stats.failed_batch_index,
                Outcome::Ok,
            ),
            Err(_) => (0, 0, 0, 0, None, Outcome::Error),
        };
    telemetry
        .emit(Event::new(
            Component::Cli,
            cli_version,
            EventPayload::IngestComplete {
                documents_added: u32::try_from(added).unwrap_or(u32::MAX),
                documents_updated: u32::try_from(carried).unwrap_or(u32::MAX),
                documents_skipped: u32::try_from(deleted).unwrap_or(u32::MAX),
                duration_ms,
                outcome: telemetry_outcome,
                batch_count: Some(batch_count),
                failed_batch_index,
            },
        ))
        .await;

    outcome
}

/// Per-run ingest statistics. `added` and `total_tokens` are `pub` so the
/// model-migration driver can budget at source boundaries.
pub struct RunStats {
    /// Documents newly added in this run (accepted minus carried).
    pub added: usize,
    carried: usize,
    deleted: usize,
    batch_count: u32,
    failed_batch_index: Option<u32>,
    /// Total VoyageAI tokens consumed embedding this run's chunks, summed across
    /// every embed call (BYOK and server-proxy alike report usage).
    pub total_tokens: u64,
}

#[allow(clippy::too_many_lines)]
async fn run_inner(
    args: &Args,
    server_url: &str,
    auth_path: &Path,
    config_path: Option<&Path>,
    voyage_api_key: Option<&str>,
    json: bool,
) -> Result<RunStats> {
    let mut reporter = crate::progress::pick(json);

    // ── Phase: resolve server ────────────────────────────────────────────────
    reporter.phase("resolved_server", serde_json::json!({"url": server_url}));
    reporter.phase_done("resolved_server", serde_json::json!({"url": server_url}));

    // ── Phase: validate manifest ─────────────────────────────────────────────
    reporter.phase("manifest_validated", serde_json::json!({}));

    let manifest_path = &args.manifest;
    let body = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read manifest at {}", manifest_path.display()))?;
    let manifest = Manifest::parse(&body).context("parse manifest")?;
    manifest.validate().context("validate manifest")?;

    let source_root = args.source_root.clone().unwrap_or_else(|| {
        manifest_path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });

    // ── Phase: walk source tree ──────────────────────────────────────────────
    reporter.phase("walk", serde_json::json!({"source_root": source_root.display().to_string()}));

    let missing = manifest.validate_files_exist(&source_root);
    if !missing.is_empty() {
        let list = missing
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow!("manifest references {} missing file(s):\n{list}", missing.len()));
    }

    let walker = Walker::new(manifest, source_root.clone());
    let walked_docs = walker.walk().context("walk source tree")?;

    reporter.phase_done("walk", serde_json::json!({"files": walked_docs.len()}));

    // ── Phase: chunk ─────────────────────────────────────────────────────────
    reporter.phase("chunk", serde_json::json!({}));

    let revision = args
        .revision
        .clone()
        .unwrap_or_else(|| super::infer_revision(&source_root));

    let chunker_config = mn_content::chunk::ChunkerConfig {
        max_tokens: args.code_chunk_tokens,
        fallback_lines: args.code_chunk_lines,
        fallback_overlap_lines: args.code_chunk_overlap,
        max_file_bytes: args.max_file_size,
    };
    let mut builder =
        PlanBuilder::new(&args.source_slug, SourceKind::DocsSite, &revision, PriorState::default())
            .with_chunker_config(chunker_config);
    for doc in &walked_docs {
        let ctx = WalkContext {
            path: doc.rel_path.clone(),
            kind: doc.resolved.kind,
            content: &doc.content,
            split: &doc.split,
            resolved: &doc.resolved,
            source_modified_at: doc.source_modified_at,
            package: detect_package_ref(&source_root, &doc.rel_path, &doc.content),
        };
        builder
            .add_walked_document(&ctx)
            .with_context(|| format!("plan add {}", doc.rel_path.display()))?;
    }
    let plan = builder.finalize();

    reporter.phase_done(
        "chunk",
        serde_json::json!({
            "documents": plan.stats.documents_added,
            "chunks": plan.stats.chunks_emitted,
        }),
    );

    if args.dry_run {
        println!(
            "{}",
            format_dry_run(
                &args.source_slug,
                plan.stats.documents_added,
                plan.stats.chunks_emitted,
                json
            ),
        );
        return Ok(RunStats {
            added: plan.stats.documents_added,
            carried: 0,
            deleted: 0,
            batch_count: 0,
            failed_batch_index: None,
            total_tokens: 0,
        });
    }

    // ── Load admin bearer ────────────────────────────────────────────────────
    let auth_file = AuthFile::read_optional(auth_path)
        .with_context(|| format!("read auth.toml at {}", auth_path.display()))?
        .ok_or_else(|| anyhow!("no admin token — run `mnm login --user-id <id>` first"))?;
    let admin = auth_file
        .admin
        .ok_or_else(|| anyhow!("auth.toml has no [admin] section — run `mnm login` first"))?;
    if admin.expires_at <= OffsetDateTime::now_utc() {
        return Err(anyhow!(
            "admin token expired at {}; run `mnm login` to refresh",
            admin.expires_at,
        ));
    }
    let token = admin.token;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build HTTP client")?;

    // ── Phase: auto-create source if missing ─────────────────────────────────
    reporter.phase("check_source", serde_json::json!({"slug": args.source_slug}));

    let source_check = client
        .get(format!("{server_url}/v1/sources/{}", url_encode(&args.source_slug)))
        .send()
        .await
        .with_context(|| format!("GET /v1/sources/{}", &args.source_slug))?;

    if source_check.status() == reqwest::StatusCode::NOT_FOUND {
        if should_create_source(args)? {
            reporter.phase("source_creating", serde_json::json!({"slug": args.source_slug}));
            client
                .post(format!("{server_url}/v1/admin/sources"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "slug": args.source_slug,
                    "display_name": args.source_slug,
                    "kind": "docs_site",
                    "retention_count": 5,
                }))
                .send()
                .await
                .with_context(|| "POST /v1/admin/sources")?
                .error_for_status()
                .with_context(|| "create source")?;
            reporter.phase_done(
                "source_created",
                serde_json::json!({"slug": args.source_slug, "kind": "docs_site"}),
            );
        } else {
            return Err(anyhow!(
                "cancelled; run `mnm sources create` manually if you want different defaults"
            ));
        }
    } else {
        reporter.phase_done(
            "check_source",
            serde_json::json!({"slug": args.source_slug, "exists": true}),
        );
    }

    // Resolve the embedding context (BYOK Voyage vs server-proxy) and the
    // bearer up front. Doing this before we create a server-side run means a
    // missing key / unreachable model resolves fast without leaving an orphaned
    // `building` source_version. We build the BYOK `VoyageEmbedder` once here so
    // every batch reuses the same client; the corpus is always embedded
    // CLI-side now (the old local-fastembed and server-side-embed branches are
    // gone), so the server never has to load an embedding model.
    let bearer = resolve_admin_bearer_str(&token);
    let env = mn_core::config::StdEnv;
    let (cfg, _) = mn_core::config::Config::discover(config_path, &env).unwrap_or_default();
    let voyage_key = mn_core::config::resolve_voyage_api_key(voyage_api_key, &cfg.models, &env);
    let voyage_timeout_secs =
        mn_core::config::resolve_voyage_timeout_secs(args.voyage_timeout_secs, &cfg.models, &env);
    let byok_embedder = voyage_key.as_deref().map(|key| {
        VoyageEmbedder::new(
            key,
            &cfg.models.embedding,
            cfg.models.voyage_output_dimension,
            &cfg.models.voyage_output_dtype,
        )
        .with_timeout_secs(voyage_timeout_secs)
    });
    reporter.phase(
        "embedder_resolved",
        serde_json::json!({"mode": if byok_embedder.is_some() { "byok" } else { "server" }}),
    );

    // Resolve the corpus wire id. When the sentinel "auto" is present, fetch the
    // active model from the server so the wire id always matches the active
    // corpus model; an explicit --embedding-model override is honoured directly
    // (and skips the round-trip). This labels both the start-run request and the
    // per-batch upload bodies.
    let embedding_model = if args.embedding_model == DEFAULT_EMBEDDING_MODEL {
        let active = crate::commands::models::fetch_active(server_url)
            .await
            .context("resolve active corpus model")?;
        format!("{}@{}", active.name, active.revision)
    } else {
        args.embedding_model.clone()
    };

    // ── Phase: start ingest run ──────────────────────────────────────────────
    reporter.phase("start_run", serde_json::json!({"slug": args.source_slug}));

    let start: StartIngestRunResponse = post_json(
        &client,
        &format!(
            "{server_url}/v1/admin/sources/{slug}/ingest-runs",
            slug = url_encode(&args.source_slug),
        ),
        &token,
        &StartIngestRunRequest {
            ingest_cli_version: env!("CARGO_PKG_VERSION").to_owned(),
            embedding_model: embedding_model.clone(),
            note: args.note.clone(),
        },
    )
    .await
    .map_err(|e| translate_start_error(e, &embedding_model))
    .context("start ingest run")?;

    reporter
        .phase_done("start_run", serde_json::json!({"run_id": start.ingest_run_id.to_string()}));

    // ── Phase: upload documents (chunked) ────────────────────────────────────
    reporter.phase("upload_documents", serde_json::json!({"documents": plan.new_documents.len()}));

    let docs: Vec<DocumentUpload> = plan
        .new_documents
        .iter()
        .map(|d| DocumentUpload {
            path: d.path.display().to_string(),
            kind: d.kind,
            content_hash: d.content_hash.clone(),
            source_url: d.source_url.clone().or_else(|| {
                args.source_base_url.as_ref().map(|base| {
                    let base = base.trim_end_matches('/');
                    format!("{base}/{}", d.path.display())
                })
            }),
            published_url: d.published_url.clone(),
            language: d.language.clone(),
            source_modified_at: d.source_modified_at,
            frontmatter: d.frontmatter.clone(),
            provenance: d.provenance.clone(),
            char_count: i32::try_from(d.char_count).unwrap_or(i32::MAX),
            token_count: i32::try_from(d.token_count).unwrap_or(i32::MAX),
            chunks: d
                .chunks
                .iter()
                .map(|c| ChunkUpload {
                    chunk_index: i32::try_from(c.chunk_index).unwrap_or(i32::MAX),
                    total_chunks: i32::try_from(c.total_chunks).unwrap_or(i32::MAX),
                    content: c.content.clone(),
                    content_hash: c.content_hash.clone(),
                    heading_path: c.heading_path.clone(),
                    symbol_path: c.symbol_path.clone(),
                    start_byte: i32::try_from(c.start_byte).unwrap_or(i32::MAX),
                    end_byte: i32::try_from(c.end_byte).unwrap_or(i32::MAX),
                    token_count: i32::try_from(c.token_count).unwrap_or(i32::MAX),
                    embedding: None,
                })
                .collect(),
            package: d.package.clone(),
        })
        .collect();

    let batch_size = args.batch_size.max(1);
    let batch_count = if docs.is_empty() {
        1
    } else {
        docs.len().div_ceil(batch_size)
    };
    let upload_url = format!(
        "{server_url}/v1/admin/sources/{slug}/ingest-runs/{id}/documents",
        slug = url_encode(&args.source_slug),
        id = start.ingest_run_id,
    );

    let mut accepted = 0usize;
    let mut carried = 0usize;
    // Sum of VoyageAI tokens consumed across every embed call this run. Surfaced
    // on RunStats so the model-migration driver can budget at source boundaries.
    let mut total_tokens = 0u64;

    // Build the embed context once: BYOK when a Voyage key resolved, else proxy
    // through the server's /v1/embeddings (which holds the platform key). The
    // admin-only `--unsafe-no-global-limit` opt-out only applies on the
    // server-proxy path; the server still enforces the admin-role check, so a
    // non-admin caller setting it has no effect. It is meaningless for BYOK
    // (Voyage has no such cap), so the BYOK branch ignores it.
    let embed_ctx = byok_embedder.as_ref().map_or(
        EmbedCtx::Server {
            base_url: server_url,
            bearer: bearer.as_deref(),
            no_global_limit: args.unsafe_no_global_limit,
        },
        EmbedCtx::Byok,
    );

    for (i, batch) in docs.chunks(batch_size).enumerate() {
        let mut batch_docs = batch.to_vec();
        // Embedding is the slow per-batch step; surface it as its own phase so
        // progress consumers don't appear to hang on "uploading".
        reporter.batch(i + 1, batch_count, "embedding documents");
        match embed_batch(&embed_ctx, &mut batch_docs).await {
            Ok(tokens) => total_tokens = total_tokens.saturating_add(tokens),
            Err(e) => {
                abort_run(&client, server_url, &args.source_slug, start.ingest_run_id, &token)
                    .await;
                return Err(e.context(format!("embed batch {}/{batch_count}", i + 1)));
            }
        }
        reporter.batch(i + 1, batch_count, "uploading documents");
        let body = UploadDocumentsRequest {
            documents: batch_docs,
            batch_index: i,
            batch_count,
            embedding_model: Some(embedding_model.clone()),
        };
        let result: Result<UploadDocumentsResponse> =
            put_json(&client, &upload_url, &token, &body).await;
        match result {
            Ok(r) => {
                accepted += r.accepted;
                carried += r.carried;
            }
            Err(e) => {
                abort_run(&client, server_url, &args.source_slug, start.ingest_run_id, &token)
                    .await;
                return Err(translate_upload_error(e, i + 1, batch_count, start.ingest_run_id)
                    .context("upload documents"));
            }
        }
    }

    reporter.phase_done(
        "upload_documents",
        serde_json::json!({"accepted": accepted, "carried": carried}),
    );

    // ── Phase: finalize ──────────────────────────────────────────────────────
    reporter.phase("finalize", serde_json::json!({}));

    let finalize_url = format!(
        "{server_url}/v1/admin/sources/{slug}/ingest-runs/{id}/finalize",
        slug = url_encode(&args.source_slug),
        id = start.ingest_run_id,
    );
    let finalize: FinalizeResult = match post_empty(&client, &finalize_url, &token).await {
        Ok(r) => r,
        Err(e) => {
            abort_run(&client, server_url, &args.source_slug, start.ingest_run_id, &token).await;
            return Err(e.context("finalize ingest run"));
        }
    };

    reporter.phase_done("finalize", serde_json::json!({"revision": finalize.revision}));

    let stats = RunStats {
        added: accepted.saturating_sub(carried),
        carried,
        deleted: 0,
        batch_count: u32::try_from(batch_count).unwrap_or(u32::MAX),
        failed_batch_index: None,
        total_tokens,
    };
    println!(
        "{}",
        format_success(
            &args.source_slug,
            finalize.revision,
            finalize.demoted_revision,
            stats.added,
            stats.carried,
            json,
        )
    );
    Ok(stats)
}

/// Prompt the user (or honor `--yes` / non-TTY) for auto-creating a missing
/// source.
fn should_create_source(args: &Args) -> Result<bool> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    if args.yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(anyhow!(
            "source '{}' does not exist; re-run with --yes or create it explicitly with `mnm sources create`",
            args.source_slug
        ));
    }
    eprint!(
        "Source '{}' doesn't exist on this server. Create it as kind=docs_site (retention=5)? [Y/n] ",
        args.source_slug
    );
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let ans = line.trim().to_ascii_lowercase();
    Ok(ans.is_empty() || ans == "y" || ans == "yes")
}

/// Translate a `start_ingest_run` HTTP error into a helpful message.
fn translate_start_error(e: anyhow::Error, requested: &str) -> anyhow::Error {
    let msg = e.to_string();
    if msg.contains("409") {
        return anyhow!(
            "server's active embedding model differs from --embedding-model={requested}; \
             run `mnm models pull` and retry, or pass --embedding-model to match"
        );
    }
    e
}

/// Translate a batch-upload HTTP error into a helpful message, **preserving**
/// the underlying error (which carries the server's `{status}: {body}`, naming
/// the failing field). The CLI prints with `{:#}`, so the whole chain shows.
fn translate_upload_error(
    e: anyhow::Error,
    batch: usize,
    of: usize,
    run_id: Uuid,
) -> anyhow::Error {
    let msg = e.to_string();
    if msg.contains("413") {
        return e.context(format!(
            "batch {batch} exceeded the server payload limit; aborted run {run_id}. \
             Re-run with --batch-size 15 (or lower) — current default is 25 docs/batch"
        ));
    }
    e.context(format!(
        "upload failed at batch {batch}/{of}; aborted run {run_id}. \
         The server's response is shown above; re-run `mnm ingest run` to retry"
    ))
}

async fn abort_run(
    client: &reqwest::Client,
    server_url: &str,
    slug: &str,
    run_id: Uuid,
    token: &str,
) {
    let url = format!(
        "{server_url}/v1/admin/sources/{slug}/ingest-runs/{run_id}/abort",
        slug = url_encode(slug),
    );
    let _ = client.post(&url).bearer_auth(token).send().await;
}

fn url_encode(s: &str) -> String {
    // Slugs are lowercase alnum + hyphen by convention; nothing to encode in
    // practice. Replace any forward slash defensively.
    s.replace('/', "%2F")
}

/// Distribute one embedding vector per chunk, in document-then-chunk order.
///
/// # Errors
///
/// Errors if `vectors.len()` does not equal the total chunk count.
fn attach_embeddings(docs: &mut [DocumentUpload], vectors: Vec<Vec<f32>>) -> Result<()> {
    let total: usize = docs.iter().map(|d| d.chunks.len()).sum();
    if vectors.len() != total {
        return Err(anyhow!("embedder returned {} vectors for {total} chunks", vectors.len()));
    }
    let mut it = vectors.into_iter();
    for d in docs.iter_mut() {
        for c in &mut d.chunks {
            c.embedding = it.next();
        }
    }
    Ok(())
}

/// Resolve a non-empty admin bearer string for use on the server-proxy embed
/// path. The admin token has already been validated (presence + expiry) before
/// the run starts; this just hands it back as the `EmbedSource::Server` bearer.
fn resolve_admin_bearer_str(token: &str) -> Option<String> {
    if token.is_empty() {
        None
    } else {
        Some(token.to_owned())
    }
}

/// Where the per-batch Voyage embedding call goes. Holds the borrows needed to
/// (re)construct an [`EmbedSource`] per sub-batch — `EmbedSource` is consumed by
/// value by [`embed`], so we rebuild it for each window rather than move it.
enum EmbedCtx<'a> {
    /// BYOK: call Voyage directly with the supplied embedder.
    Byok(&'a VoyageEmbedder),
    /// Server-proxy via `/v1/embeddings`.
    Server {
        base_url: &'a str,
        bearer: Option<&'a str>,
        /// Admin-only opt-out from the server's site-wide token cap (from
        /// `--unsafe-no-global-limit`). Meaningless on the BYOK path.
        no_global_limit: bool,
    },
}

impl EmbedCtx<'_> {
    const fn source(&self) -> EmbedSource<'_> {
        match self {
            Self::Byok(v) => EmbedSource::Byok(v),
            Self::Server {
                base_url,
                bearer,
                no_global_limit,
            } => EmbedSource::Server {
                base_url,
                bearer: *bearer,
                no_global_limit: *no_global_limit,
            },
        }
    }
}

/// Embed every chunk of `docs` in place via VoyageAI (`input_type=document`),
/// using `ctx` (BYOK direct, or the server `/v1/embeddings` proxy), and return
/// the total VoyageAI tokens consumed across this batch's sub-requests.
///
/// The collected chunk texts are greedily packed into sub-requests bounded by
/// BOTH [`VOYAGE_MAX_TEXTS_PER_REQUEST`] items AND [`VOYAGE_MAX_TOKENS_PER_REQUEST`]
/// tokens, using each chunk's recorded token count. `voyage-code-3` returns 400
/// for requests over ~120K tokens (measured: 500 chunks / 74K tokens = 200 OK;
/// 1000 chunks / ~148K tokens = 400), so the token bound is the real guard; the
/// item bound is a secondary safety net. A single chunk that alone exceeds the
/// token budget is still sent as its own sub-request (never dropped). Input order
/// is preserved: vectors are concatenated in order before being distributed back
/// across the docs' chunks, and the per-request `usage.total_tokens` is summed
/// (both BYOK and server-proxy report it via [`mn_embedding::client::Embedded`]).
///
/// # Errors
///
/// Errors if any Voyage call fails or the returned vector count does not match
/// the chunk count.
async fn embed_batch(ctx: &EmbedCtx<'_>, docs: &mut [DocumentUpload]) -> Result<u64> {
    // Pair each chunk text with its token count so we can bound sub-requests by
    // both item count and summed tokens. `token_count` is a non-negative i32;
    // the `0` fallback only affects budgeting — never vector alignment, which is
    // positional.
    let texts: Vec<(String, usize)> = docs
        .iter()
        .flat_map(|d| {
            d.chunks
                .iter()
                .map(|c| (c.content.clone(), usize::try_from(c.token_count).unwrap_or(0)))
        })
        .collect();
    if texts.is_empty() {
        return Ok(0);
    }

    let token_counts: Vec<usize> = texts.iter().map(|(_, t)| *t).collect();
    let plan =
        plan_subbatches(&token_counts, VOYAGE_MAX_TEXTS_PER_REQUEST, VOYAGE_MAX_TOKENS_PER_REQUEST);

    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    let mut tokens = 0u64;
    let mut chunks = texts.into_iter().map(|(s, _)| s);
    for size in plan {
        let sub: Vec<String> = chunks.by_ref().take(size).collect();
        let embedded = embed(sub, InputType::Document, ctx.source())
            .await
            .map_err(|e| anyhow!("embed chunks via Voyage: {e}"))?;
        tokens = tokens.saturating_add(embedded.total_tokens);
        vectors.extend(embedded.vectors);
    }

    attach_embeddings(docs, vectors)?;
    Ok(tokens)
}

/// Greedily group chunk `token_counts` into sub-request sizes bounded by BOTH
/// `max_items` items AND `max_tokens` summed tokens, preserving order. A chunk
/// whose own token count exceeds `max_tokens` forms its own sub-request (it is
/// never dropped or merged with a neighbour). The returned sizes sum to
/// `token_counts.len()`.
fn plan_subbatches(token_counts: &[usize], max_items: usize, max_tokens: usize) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut cur = 0usize;
    let mut cur_tokens = 0usize;
    for &tok in token_counts {
        // Close the current sub-batch before this chunk would push it over either
        // bound; skip when empty so a lone oversized chunk still goes out alone.
        if cur > 0 && (cur >= max_items || cur_tokens.saturating_add(tok) > max_tokens) {
            sizes.push(cur);
            cur = 0;
            cur_tokens = 0;
        }
        cur += 1;
        cur_tokens = cur_tokens.saturating_add(tok);
    }
    if cur > 0 {
        sizes.push(cur);
    }
    sizes
}

#[cfg(test)]
mod plan_subbatches_tests {
    use super::plan_subbatches;

    #[test]
    fn splits_on_item_cap() {
        // 600 zero-token chunks, item cap 250 -> 250 + 250 + 100.
        assert_eq!(plan_subbatches(&[0usize; 600], 250, 100_000), vec![250, 250, 100]);
    }

    #[test]
    fn splits_on_token_cap() {
        // 30k-token chunks: three fit (90k); the fourth (120k) starts a new batch.
        assert_eq!(plan_subbatches(&[30_000usize; 7], 250, 100_000), vec![3, 3, 1]);
    }

    #[test]
    fn oversized_chunk_goes_out_alone() {
        // The 150k chunk exceeds the token cap; it must not be merged or dropped.
        assert_eq!(plan_subbatches(&[10_000, 150_000, 10_000], 250, 100_000), vec![1, 1, 1]);
    }

    #[test]
    fn empty_input_yields_no_subbatches() {
        assert!(plan_subbatches(&[], 250, 100_000).is_empty());
    }

    #[test]
    fn sizes_sum_to_len() {
        let sizes = plan_subbatches(&[500usize; 333], 250, 100_000);
        assert_eq!(sizes.iter().sum::<usize>(), 333);
    }
}

async fn post_json<I: Serialize + Sync, O: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &I,
) -> Result<O> {
    let resp = client
        .post(url)
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    decode_response(resp, url).await
}

async fn post_empty<O: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<O> {
    let resp = client
        .post(url)
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    decode_response(resp, url).await
}

async fn put_json<I: Serialize + Sync, O: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &I,
) -> Result<O> {
    let resp = client
        .put(url)
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .with_context(|| format!("PUT {url}"))?;
    decode_response(resp, url).await
}

async fn decode_response<O: for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
    url: &str,
) -> Result<O> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("{status} from {url}: {}", redact_token_like(&body)));
    }
    resp.json::<O>()
        .await
        .with_context(|| format!("parse response from {url}"))
}

/// Strip long base64-y fragments from error bodies so a bearer that ends up in
/// an error envelope is not echoed to logs (FR-019).
fn redact_token_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if word.len() > 40
            && word
                .chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '=')
        {
            out.push_str("[redacted]");
        } else {
            out.push_str(word);
        }
        out.push(' ');
    }
    out.trim_end().to_owned()
}

#[derive(Debug, Serialize)]
struct StartIngestRunRequest {
    ingest_cli_version: String,
    embedding_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StartIngestRunResponse {
    ingest_run_id: Uuid,
    #[allow(dead_code)]
    source_version_id: Uuid,
    #[allow(dead_code)]
    source_version_revision: i32,
}

#[derive(Debug, Serialize, Clone)]
struct UploadDocumentsRequest {
    documents: Vec<DocumentUpload>,
    batch_index: usize,
    batch_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    embedding_model: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
struct DocumentUpload {
    path: String,
    kind: DocumentKind,
    content_hash: String,
    source_url: Option<String>,
    published_url: Option<String>,
    language: Option<String>,
    source_modified_at: Option<OffsetDateTime>,
    frontmatter: Option<serde_json::Value>,
    provenance: Provenance,
    char_count: i32,
    token_count: i32,
    chunks: Vec<ChunkUpload>,
    /// Detected package membership (rust/npm) for this document, if any.
    package: Option<mn_core::types::PackageRef>,
}

#[derive(Debug, Serialize, Clone)]
struct ChunkUpload {
    chunk_index: i32,
    total_chunks: i32,
    content: String,
    content_hash: String,
    heading_path: Vec<String>,
    symbol_path: Vec<mn_core::types::SymbolSegment>,
    start_byte: i32,
    end_byte: i32,
    token_count: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    embedding: Option<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct UploadDocumentsResponse {
    accepted: usize,
    carried: usize,
    #[allow(dead_code)]
    #[serde(default)]
    conflicts: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct FinalizeResult {
    #[allow(dead_code)]
    source_version_id: Uuid,
    revision: i32,
    #[allow(dead_code)]
    is_active: bool,
    #[serde(default)]
    demoted_revision: Option<i32>,
}

/// Detect package membership for a single file.
///
/// `.compact` files are detected from their contents (a single top-level
/// `module <Name>`); all other files walk up to the source root for the nearest
/// `Cargo.toml` / `package.json`.
fn detect_package_ref(
    source_root: &std::path::Path,
    rel_path: &std::path::Path,
    content: &str,
) -> Option<mn_core::types::PackageRef> {
    if rel_path.extension().and_then(|e| e.to_str()) == Some("compact") {
        return mn_content::detect_compact_package(content);
    }
    let abs = source_root.join(rel_path);
    mn_content::package::detect(&abs, source_root).map(|p| mn_core::types::PackageRef {
        kind: p.kind,
        name: p.name,
        manifest_path: Some(p.manifest_path.display().to_string()),
    })
}

#[derive(Debug, Serialize)]
struct DryRunOutput<'a> {
    action: &'a str,
    source_slug: &'a str,
    documents: usize,
    chunks: usize,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct SuccessOutput<'a> {
    action: &'a str,
    source_slug: &'a str,
    revision: i32,
    demoted_revision: Option<i32>,
    documents_added: usize,
    documents_carried: usize,
}

fn format_dry_run(source_slug: &str, documents: usize, chunks: usize, json: bool) -> String {
    if json {
        let body = DryRunOutput {
            action: "ingest",
            source_slug,
            documents,
            chunks,
            dry_run: true,
        };
        serde_json::to_string(&body).unwrap_or_default()
    } else {
        format!(
            "ingest dry-run for `{source_slug}`: would post {documents} documents / {chunks} chunks",
        )
    }
}

fn format_success(
    source_slug: &str,
    revision: i32,
    demoted_revision: Option<i32>,
    added: usize,
    carried: usize,
    json: bool,
) -> String {
    if json {
        let body = SuccessOutput {
            action: "ingest",
            source_slug,
            revision,
            demoted_revision,
            documents_added: added,
            documents_carried: carried,
        };
        serde_json::to_string(&body).unwrap_or_default()
    } else if let Some(prev) = demoted_revision {
        format!(
            "ingested `{source_slug}` rev {revision} (was {prev}); +{added} new, {carried} carried"
        )
    } else {
        format!("ingested `{source_slug}` rev {revision} (first version); +{added} new")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_code_chunk_and_filter_flags() {
        use clap::Parser as _;
        // Args derives ClapArgs (not Parser); wrap in a minimal Parser for
        // testing so try_parse_from is available.
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            inner: Args,
        }
        let w = Wrap::try_parse_from([
            "ingest-run",
            "--source-slug",
            "s",
            "--code-chunk-tokens",
            "256",
            "--code-chunk-lines",
            "80",
            "--code-chunk-overlap",
            "15",
            "--include",
            "*.rs",
            "--exclude",
            "gen_*",
            "--no-respect-gitignore",
            "--disable-default-ignore-list",
            "--max-file-size",
            "1048576",
            "m.yaml",
        ])
        .unwrap();
        let args = w.inner;
        assert_eq!(args.code_chunk_tokens, 256);
        assert_eq!(args.code_chunk_lines, 80);
        assert_eq!(args.code_chunk_overlap, 15);
        assert_eq!(args.include, vec!["*.rs".to_string()]);
        assert_eq!(args.exclude, vec!["gen_*".to_string()]);
        assert!(args.no_respect_gitignore);
        assert!(args.disable_default_ignore_list);
        assert_eq!(args.max_file_size, 1_048_576);
    }

    #[test]
    fn dry_run_human_output() {
        let s = format_dry_run("docs", 3, 7, false);
        assert!(s.contains("docs"));
        assert!(s.contains("3 documents"));
        assert!(s.contains("7 chunks"));
    }

    #[test]
    fn dry_run_json_output() {
        let s = format_dry_run("docs", 3, 7, true);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["action"], "ingest");
        assert_eq!(v["documents"], 3);
        assert_eq!(v["chunks"], 7);
        assert_eq!(v["dry_run"], true);
    }

    #[test]
    fn success_human_output_first_version() {
        let s = format_success("docs", 1, None, 5, 0, false);
        assert!(s.contains("first version"));
        assert!(s.contains("+5 new"));
    }

    #[test]
    fn success_human_output_with_demote() {
        let s = format_success("docs", 2, Some(1), 3, 4, false);
        assert!(s.contains("rev 2"));
        assert!(s.contains("was 1"));
        assert!(s.contains("+3 new"));
        assert!(s.contains("4 carried"));
    }

    #[test]
    fn redacts_long_alnum_blobs() {
        let body = "verify failed token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let redacted = redact_token_like(body);
        assert!(!redacted.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(redacted.contains("[redacted]"));
    }

    #[test]
    fn attach_embeddings_distributes_in_order() {
        let mut docs = vec![
            DocumentUpload {
                path: "a".into(),
                kind: DocumentKind::Markdown,
                content_hash: "h".into(),
                source_url: None,
                published_url: None,
                language: None,
                source_modified_at: None,
                frontmatter: None,
                provenance: Provenance::default(),
                char_count: 0,
                token_count: 0,
                package: None,
                chunks: vec![mk_chunk(0), mk_chunk(1)],
            },
            DocumentUpload {
                path: "b".into(),
                kind: DocumentKind::Markdown,
                content_hash: "h".into(),
                source_url: None,
                published_url: None,
                language: None,
                source_modified_at: None,
                frontmatter: None,
                provenance: Provenance::default(),
                char_count: 0,
                token_count: 0,
                package: None,
                chunks: vec![mk_chunk(0)],
            },
        ];
        let vectors = vec![vec![1.0_f32], vec![2.0], vec![3.0]];
        attach_embeddings(&mut docs, vectors).unwrap();
        assert_eq!(docs[0].chunks[0].embedding, Some(vec![1.0]));
        assert_eq!(docs[0].chunks[1].embedding, Some(vec![2.0]));
        assert_eq!(docs[1].chunks[0].embedding, Some(vec![3.0]));
    }

    #[test]
    fn attach_embeddings_rejects_count_mismatch() {
        let mut docs = vec![DocumentUpload {
            path: "a".into(),
            kind: DocumentKind::Markdown,
            content_hash: "h".into(),
            source_url: None,
            published_url: None,
            language: None,
            source_modified_at: None,
            frontmatter: None,
            provenance: Provenance::default(),
            char_count: 0,
            token_count: 0,
            package: None,
            chunks: vec![mk_chunk(0)],
        }];
        assert!(attach_embeddings(&mut docs, vec![]).is_err());
    }

    fn mk_chunk(idx: i32) -> ChunkUpload {
        ChunkUpload {
            chunk_index: idx,
            total_chunks: 2,
            content: format!("c{idx}"),
            content_hash: "c".into(),
            heading_path: vec![],
            symbol_path: vec![],
            start_byte: 0,
            end_byte: 0,
            token_count: 0,
            embedding: None,
        }
    }

    #[test]
    fn upload_error_preserves_server_body() {
        let original = anyhow!(
            "422 Unprocessable Entity from http://x/documents: \
             {{\"error\":{{\"code\":\"invalid_request\",\"message\":\"unknown field embeding\"}}}}"
        );
        let translated = translate_upload_error(original, 8, 11, Uuid::nil());
        let shown = format!("{translated:#}");
        assert!(shown.contains("422"), "must keep server status: {shown}");
        assert!(shown.contains("invalid_request"), "must keep server body: {shown}");
        assert!(shown.contains("batch 8/11"), "must add batch context: {shown}");
    }

    #[test]
    fn default_batch_size_is_25_and_embedding_model_defaults_to_auto() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            inner: Args,
        }
        let w = Wrap::try_parse_from(["ingest-run", "--source-slug", "s", "m.yaml"]).unwrap();
        assert_eq!(w.inner.batch_size, 25);
        // The corpus wire id is resolved from /v1/models/active at runtime; the
        // clap default is the "auto" sentinel, not a hardcoded model name.
        assert_eq!(w.inner.embedding_model, DEFAULT_EMBEDDING_MODEL);
    }

    #[test]
    fn unsafe_no_global_limit_flag_parses_and_defaults_false() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            inner: Args,
        }
        // Absent → false (default; the global cap counts this ingest).
        let off = Wrap::try_parse_from(["ingest-run", "--source-slug", "s", "m.yaml"]).unwrap();
        assert!(!off.inner.unsafe_no_global_limit);
        // Present → true (admin-only opt-out; server still checks the role).
        let on = Wrap::try_parse_from([
            "ingest-run",
            "--source-slug",
            "s",
            "--unsafe-no-global-limit",
            "m.yaml",
        ])
        .unwrap();
        assert!(on.inner.unsafe_no_global_limit);
    }

    #[test]
    fn voyage_timeout_secs_flag_parses_and_defaults_none() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            inner: Args,
        }
        // Absent → None (resolver falls back to env/config/default).
        let off = Wrap::try_parse_from(["ingest-run", "--source-slug", "s", "m.yaml"]).unwrap();
        assert_eq!(off.inner.voyage_timeout_secs, None);
        // Present → parsed Some(secs).
        let on = Wrap::try_parse_from([
            "ingest-run",
            "--source-slug",
            "s",
            "--voyage-timeout-secs",
            "180",
            "m.yaml",
        ])
        .unwrap();
        assert_eq!(on.inner.voyage_timeout_secs, Some(180));
    }

    #[test]
    fn embedding_model_override_parses() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            inner: Args,
        }
        let w = Wrap::try_parse_from([
            "ingest-run",
            "--source-slug",
            "s",
            "--embedding-model",
            "voyage-code-3@2",
            "m.yaml",
        ])
        .unwrap();
        assert_eq!(w.inner.embedding_model, "voyage-code-3@2");
    }

    #[test]
    fn chunk_upload_skips_embedding_when_none() {
        let c = ChunkUpload {
            chunk_index: 0,
            total_chunks: 1,
            content: "x".into(),
            content_hash: "c".into(),
            heading_path: vec![],
            symbol_path: vec![],
            start_byte: 0,
            end_byte: 1,
            token_count: 0,
            embedding: None,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("embedding"), "None embedding must be omitted: {s}");
    }

    #[test]
    fn embedded_request_serializes_vectors_and_model() {
        // The CLI embeds every chunk via Voyage and the upload body must carry
        // the 1024-dim vector on each chunk plus the batch-level
        // `embedding_model` (the resolved corpus wire id).
        let body = UploadDocumentsRequest {
            documents: vec![DocumentUpload {
                path: "a".into(),
                kind: DocumentKind::Markdown,
                content_hash: "h".into(),
                source_url: None,
                published_url: None,
                language: None,
                source_modified_at: None,
                frontmatter: None,
                provenance: Provenance::default(),
                char_count: 0,
                token_count: 0,
                package: None,
                chunks: vec![ChunkUpload {
                    chunk_index: 0,
                    total_chunks: 1,
                    content: "x".into(),
                    content_hash: "c".into(),
                    heading_path: vec![],
                    symbol_path: vec![],
                    start_byte: 0,
                    end_byte: 1,
                    token_count: 0,
                    embedding: Some(vec![0.5_f32; 1024]),
                }],
            }],
            batch_index: 0,
            batch_count: 1,
            embedding_model: Some("voyage-code-3@1".to_owned()),
        };
        let v: serde_json::Value = serde_json::to_value(&body).unwrap();
        assert_eq!(v["embedding_model"], "voyage-code-3@1");
        let emb = v["documents"][0]["chunks"][0]["embedding"]
            .as_array()
            .expect("embedding present on the wire");
        assert_eq!(emb.len(), 1024);
    }
}

#[cfg(test)]
mod compact_package_routing_tests {
    use super::detect_package_ref;
    use std::path::Path;

    #[test]
    fn compact_file_routes_to_module_detection() {
        let root = tempfile::tempdir().unwrap();
        let body = "module M {\n  export ledger b: Field;\n}\n";
        let pkg = detect_package_ref(root.path(), Path::new("src/Token.compact"), body)
            .expect("module M should be detected");
        assert_eq!(pkg.kind, "compact");
        assert_eq!(pkg.name, "M");
    }

    #[test]
    fn non_compact_file_ignores_content() {
        let root = tempfile::tempdir().unwrap();
        // No Cargo.toml/package.json anywhere → None, regardless of content.
        let pkg = detect_package_ref(root.path(), Path::new("src/lib.rs"), "module M {}");
        assert!(pkg.is_none());
    }
}

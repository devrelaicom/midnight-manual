//! `mnm ingest <manifest>` — admin command that runs an end-to-end ingest
//! against the cloud server (Story 10).
//!
//! Flow:
//!
//! 1. Read the manifest (`hierarchy.yaml`) and validate it.
//!
//! 2. Walk the source root, parse frontmatter, run the Markdown chunker
//!    (via the orchestrator in [`mnm_content::ingest`]).
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
//!    All chunks get GENERAL contextualized vectors (voyage-context-3,
//!    per-document context groups); chunks of Code-kind documents additionally
//!    get flat voyage-code-3 vectors unless opted out (manifest
//!    `code_embeddings: false` or `--no-code-embeddings`, D9).
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
use mnm_content::ingest::{PlanBuilder, PriorState, WalkContext, Walker};
use mnm_content::manifest::Manifest;
use mnm_core::auth_file::AuthFile;
use mnm_core::provenance::Provenance;
use mnm_core::types::{DocumentKind, SourceKind};
use mnm_embedding::client::{EmbedSource, GeneralEmbedSource};
use mnm_embedding::contextualized::ContextualizedVoyageEmbedder;
use mnm_embedding::voyage::{InputType, VoyageEmbedder};
use mnm_telemetry::events::{Component, EventPayload, Outcome};
use mnm_telemetry::{Event, TelemetryClient};
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

// Rough upload-size estimate constants. Deliberately over-estimate so batches
// stay under the byte target; the retry-split covers any under-estimate.
const EST_EMBED_DIM: usize = 1024; // voyage-code-3 default (Matryoshka); rough
const EST_BYTES_PER_EMBED_FLOAT: usize = 12; // JSON float incl. comma
const EST_PER_CHUNK_OVERHEAD: usize = 256; // hashes, paths, indices, JSON keys/braces
const EST_PER_DOC_OVERHEAD: usize = 512; // path, kind, frontmatter, JSON keys

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

    /// Upper bound on documents per upload batch (default: 25). Batches are
    /// *additionally* bounded by estimated size to ~85% of the server's body
    /// limit, and any batch that still 413s is auto-split and retried, so this
    /// rarely needs changing — lower it only to cap peak memory per request.
    #[arg(long, default_value_t = 25)]
    pub batch_size: usize,

    /// Override the per-request timeout (seconds) for BYOK Voyage embedding
    /// calls. Precedence: this flag > `VOYAGE_TIMEOUT_SECS` env > config
    /// (default 120s). Raise it if large batches time out before Voyage
    /// finishes. The server-proxy embed path is not tuned by this flag; it
    /// uses the same 120s default.
    #[arg(long)]
    pub voyage_timeout_secs: Option<u64>,

    /// Chunk budget in tokens, all document kinds (markdown, code, plaintext).
    /// Greedy coalescing packs sibling units up to 90% of this.
    #[arg(long = "chunk-tokens", default_value_t = 1024)]
    pub chunk_tokens: u32,

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
    #[arg(long, default_value_t = mnm_content::chunk::DEFAULT_MAX_FILE_BYTES)]
    pub max_file_size: u64,

    /// Admin-only: exempt THIS ingest's server-side embedding from the
    /// site-wide token cap. Ignored for BYOK/local embedding. The server
    /// enforces the admin-role check — a non-admin caller setting this is still
    /// counted against the global cap.
    #[arg(long)]
    pub unsafe_no_global_limit: bool,

    /// Disable voyage-code-3 code embeddings for this run (overrides the
    /// manifest's `code_embeddings` option). Code files still get general
    /// contextualized embeddings.
    #[arg(long)]
    pub no_code_embeddings: bool,
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
    let env = mnm_core::config::StdEnv;
    let auth_path = mnm_core::paths::auth_file_path(&env)
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
/// via [`mnm_embedding::client::Embedded`]). The migration driver uses
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
    /// Per-document conflicts the server reported across every upload batch:
    /// documents that were NOT inserted into the finalized version. A non-empty
    /// list means documents were silently dropped, so it is warn-logged and
    /// surfaced in both the human summary and `--json` output.
    pub conflicts: Vec<mnm_core::ingest::UploadConflict>,
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

    // Dual embeddings opt-out (D9): the CLI flag wins over the manifest's
    // `code_embeddings` option (default true). When disabled, code-kind
    // documents still get general contextualized embeddings — only the extra
    // voyage-code-3 vectors are skipped.
    let code_embeddings_enabled = !args.no_code_embeddings && manifest.code_embeddings;

    let source_root = args.source_root.clone().unwrap_or_else(|| {
        manifest_path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });

    // Ingest tuning knobs. `max_file_bytes` is enforced by the walker below
    // (EC-52, "skipped by callers"); `max_tokens` drives the chunkers.
    let chunker_config = mnm_content::chunk::ChunkerConfig {
        max_tokens: args.chunk_tokens,
        max_file_bytes: args.max_file_size,
    };

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

    let walker = Walker::new(manifest, source_root.clone())
        .with_max_file_bytes(chunker_config.max_file_bytes);
    let outcome = walker.walk().context("walk source tree")?;
    for skip in &outcome.skipped {
        tracing::warn!(
            path = %skip.rel_path.display(),
            reason = %skip.reason,
            "skipping file during ingest walk",
        );
    }
    let walked_docs = outcome.documents;

    reporter.phase_done(
        "walk",
        serde_json::json!({"files": walked_docs.len(), "skipped": outcome.skipped.len()}),
    );

    // ── Phase: chunk ─────────────────────────────────────────────────────────
    reporter.phase("chunk", serde_json::json!({}));

    let revision = args
        .revision
        .clone()
        .unwrap_or_else(|| super::infer_revision(&source_root));

    let mut builder =
        PlanBuilder::new(&args.source_slug, SourceKind::DocsSite, &revision, PriorState::default())
            .with_chunker_config(chunker_config);
    for doc in &walked_docs {
        let extracted = if doc.resolved.no_extract {
            Provenance::default()
        } else {
            build_extracted(&source_root, &doc.rel_path, &doc.content, doc.resolved.kind)
        };
        let ctx = WalkContext {
            path: doc.rel_path.clone(),
            kind: doc.resolved.kind,
            content: &doc.content,
            split: &doc.split,
            resolved: &doc.resolved,
            extracted,
            source_modified_at: doc.source_modified_at,
            package: detect_package_ref(&source_root, &doc.rel_path, &doc.content),
        };
        builder
            .add_walked_document(&ctx)
            .with_context(|| format!("plan add {}", doc.rel_path.display()))?;
    }
    let plan = builder.finalize();

    let docs_with_language_targets = plan
        .new_documents
        .iter()
        .filter(|d| !d.provenance.language_targets.is_empty())
        .count();
    let docs_with_sdk_dependencies = plan
        .new_documents
        .iter()
        .filter(|d| !d.provenance.sdk_dependencies.is_empty())
        .count();

    reporter.phase_done(
        "chunk",
        serde_json::json!({
            "documents": plan.stats.documents_added,
            "chunks": plan.stats.chunks_emitted,
            "docs_with_language_targets": docs_with_language_targets,
            "docs_with_sdk_dependencies": docs_with_sdk_dependencies,
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
            conflicts: Vec::new(),
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
    // CLI-side now (the old local-model and server-side-embed branches are
    // gone), so the server never has to load an embedding model.
    let bearer = resolve_admin_bearer_str(&token);
    let env = mnm_core::config::StdEnv;
    let (cfg, _) = mnm_core::config::Config::discover(config_path, &env).unwrap_or_default();
    let voyage_key = mnm_core::config::resolve_voyage_api_key(voyage_api_key, &cfg.models, &env);
    let voyage_timeout_secs =
        mnm_core::config::resolve_voyage_timeout_secs(args.voyage_timeout_secs, &cfg.models, &env);

    // Resolve the corpus wire ids FIRST (before building the embedders) so the
    // embedder identities that COMPUTE the vectors are derived from the SAME
    // active-model response that LABELS them (cross-element drift fix). One
    // `GET /v1/models/active` round-trip covers the GENERAL wire id (when the
    // sentinel "auto" is present — an explicit --embedding-model override is
    // honoured directly), the CODE wire id (when code embeddings are enabled),
    // and the {name, dim, dtype} the BYOK embedders are built from.
    let active = if args.embedding_model == DEFAULT_EMBEDDING_MODEL || code_embeddings_enabled {
        Some(
            crate::commands::models::fetch_active(server_url)
                .await
                .context("resolve active corpus model")?,
        )
    } else {
        None
    };
    let embedding_model = active
        .as_ref()
        .filter(|_| args.embedding_model == DEFAULT_EMBEDDING_MODEL)
        .map_or_else(|| args.embedding_model.clone(), |a| format!("{}@{}", a.name, a.revision));
    // Code wire id: prefer the server's active code model; fall back to the
    // configured name at revision 1 for servers that predate dual embeddings.
    let code_embedding_model = code_embeddings_enabled.then(|| {
        active.as_ref().and_then(|a| a.code.as_ref()).map_or_else(
            || format!("{}@1", cfg.models.code_embedding),
            |c| format!("{}@{}", c.name, c.revision),
        )
    });

    // Derive the BYOK embedder identities from the active fetch (the authority),
    // with config as a logged fallback. The general identity follows the run's
    // chosen embedding_model: when "auto" it's the active model; under an
    // explicit --embedding-model override (e.g. `models migrate`) the bare name
    // is parsed from that wire id so the embedder matches the targeted model.
    let general_id =
        derive_general_ingest_identity(&args.embedding_model, active.as_ref(), &cfg.models);
    let code_id = derive_code_ingest_identity(active.as_ref(), &cfg.models);
    let byok = voyage_key.as_deref().map(|key| ByokEmbedders {
        general: ContextualizedVoyageEmbedder::new(
            key,
            &general_id.name,
            general_id.dim,
            &general_id.dtype,
        )
        .with_timeout_secs(voyage_timeout_secs),
        code: VoyageEmbedder::new(key, &code_id.name, code_id.dim, &code_id.dtype)
            .with_timeout_secs(voyage_timeout_secs),
    });
    reporter.phase(
        "embedder_resolved",
        serde_json::json!({"mode": if byok.is_some() { "byok" } else { "server" }}),
    );

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
            code_embedding_model,
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
                    code_embedding: None,
                })
                .collect(),
            package: d.package.clone(),
        })
        .collect();

    let batch_size = args.batch_size.max(1);
    // Bound each batch by both a document-count ceiling and ~85% of the server's
    // body limit (estimated). The 85% leaves headroom for the rough estimate's
    // slack; anything that still 413s is auto-split by the upload helper.
    let byte_target = mnm_core::limits::MAX_INGEST_BODY_BYTES * 85 / 100;
    let batches = pack_upload_batches(docs, batch_size, byte_target);
    let batch_count = batches.len();
    let upload_url = format!(
        "{server_url}/v1/admin/sources/{slug}/ingest-runs/{id}/documents",
        slug = url_encode(&args.source_slug),
        id = start.ingest_run_id,
    );

    let mut accepted = 0usize;
    let mut carried = 0usize;
    // Per-document conflicts accumulated across every upload batch (including the
    // 413 split-retry path, which concatenates its halves' conflicts). A
    // conflicted document was NOT inserted, so this is the only signal the
    // operator gets that documents were silently dropped.
    let mut conflicts: Vec<mnm_core::ingest::UploadConflict> = Vec::new();
    // Sum of VoyageAI tokens consumed across every embed call this run. Surfaced
    // on RunStats so the model-migration driver can budget at source boundaries.
    let mut total_tokens = 0u64;

    // Build the embed sources once: BYOK when a Voyage key resolved, else proxy
    // through the server's /v1/embeddings (which holds the platform key). The
    // admin-only `--unsafe-no-global-limit` opt-out only applies on the
    // server-proxy path; the server still enforces the admin-role check, so a
    // non-admin caller setting it has no effect. It is meaningless for BYOK
    // (Voyage has no such cap), so the BYOK branch ignores it. Both source enums
    // are `Copy`, so the per-batch loop reuses them directly.
    let general_src = byok.as_ref().map_or(
        GeneralEmbedSource::Server {
            base_url: server_url,
            bearer: bearer.as_deref(),
            no_global_limit: args.unsafe_no_global_limit,
        },
        |b| GeneralEmbedSource::Byok(&b.general),
    );
    let code_src = code_embeddings_enabled.then(|| {
        byok.as_ref().map_or(
            EmbedSource::Server {
                base_url: server_url,
                bearer: bearer.as_deref(),
                no_global_limit: args.unsafe_no_global_limit,
            },
            |b| EmbedSource::Byok(&b.code),
        )
    });

    for (i, batch) in batches.into_iter().enumerate() {
        let mut batch_docs = batch;
        // Embedding is the slow per-batch step; surface it as its own phase so
        // progress consumers don't appear to hang on "uploading".
        reporter.batch(i + 1, batch_count, "embedding documents");
        match embed_batch(general_src, code_src, &mut batch_docs).await {
            Ok(tokens) => total_tokens = total_tokens.saturating_add(tokens),
            Err(e) => {
                abort_run(&client, server_url, &args.source_slug, start.ingest_run_id, &token)
                    .await;
                return Err(e.context(format!("embed batch {}/{batch_count}", i + 1)));
            }
        }
        reporter.batch(i + 1, batch_count, "uploading documents");
        // Embeddings are already attached above, so the split path is a pure
        // upload concern: a 413 splits the batch and PUTs each half once.
        let result = upload_documents_with_split(
            &client,
            &upload_url,
            &token,
            &embedding_model,
            batch_docs,
            i,
            batch_count,
        )
        .await;
        match result {
            Ok(r) => {
                accepted += r.accepted;
                carried += r.carried;
                conflicts.extend(r.conflicts);
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

    // A conflicted document was NOT inserted into the finalized version: log each
    // one so the silent data loss is observable. The count is also surfaced in the
    // summary / `--json` output below.
    if !conflicts.is_empty() {
        tracing::warn!(
            count = conflicts.len(),
            source_slug = %args.source_slug,
            "ingest finalized with document conflicts — these documents were NOT inserted",
        );
        for c in &conflicts {
            tracing::warn!(path = %c.path, reason = %c.reason, "ingest document conflict");
        }
    }

    let conflict_count = conflicts.len();
    let stats = RunStats {
        added: accepted.saturating_sub(carried),
        carried,
        deleted: 0,
        batch_count: u32::try_from(batch_count).unwrap_or(u32::MAX),
        failed_batch_index: None,
        total_tokens,
        conflicts,
    };
    println!(
        "{}",
        format_success(
            &SuccessOutput {
                action: "ingest",
                source_slug: &args.source_slug,
                revision: finalize.revision,
                demoted_revision: finalize.demoted_revision,
                documents_added: stats.added,
                documents_carried: stats.carried,
                conflict_count,
                docs_with_language_targets,
                docs_with_sdk_dependencies,
            },
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
        let mib = mnm_core::limits::MAX_INGEST_BODY_BYTES / (1024 * 1024);
        return e.context(format!(
            "batch {batch} still exceeded the server's {mib} MiB body limit after \
             automatic splitting; a single document's content + embeddings is likely \
             too large. Aborted run {run_id}."
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

/// The two BYOK Voyage embedders an ingest run holds when a key resolves
/// (dual embeddings, D1): the GENERAL contextualized embedder
/// (voyage-context-3) for every chunk, and the flat CODE embedder
/// (voyage-code-3) for chunks of Code-kind documents.
struct ByokEmbedders {
    general: ContextualizedVoyageEmbedder,
    code: VoyageEmbedder,
}

/// Derive the GENERAL embedder identity for an ingest run (cross-element drift
/// fix): the `{name, dim, dtype}` the contextualized embedder is built from must
/// match the wire id the run is labelled with.
///
/// Under an explicit `--embedding-model` override (e.g. `models migrate` pins a
/// target wire id) the bare name is parsed from that wire id so the embedder
/// targets exactly that model. Under the `"auto"` sentinel the active model's
/// `{name, dim, dtype}` are the authority. Local config is only a logged
/// fallback (via [`mnm_core::embedder_identity::derive`]) when the active fetch
/// was unavailable.
fn derive_general_ingest_identity(
    embedding_model: &str,
    active: Option<&crate::commands::models::ActiveModelResponse>,
    models: &mnm_core::config::ModelsConfig,
) -> mnm_core::embedder_identity::EmbedderIdentity {
    use mnm_core::embedder_identity::{derive, ActiveModelIdentity, FallbackIdentity};
    // For an explicit override the embedder must match the targeted model's
    // bare name; dim/dtype still come from the active model (the corpus's
    // encoding) when available, else config.
    let explicit_name = (embedding_model != DEFAULT_EMBEDDING_MODEL).then(|| {
        embedding_model
            .split_once('@')
            .map_or(embedding_model, |(n, _)| n)
    });
    let active_id = active.map(|a| ActiveModelIdentity {
        name: explicit_name.map_or_else(|| a.name.clone(), str::to_owned),
        dim: u32::try_from(a.dim).unwrap_or(models.voyage_output_dimension),
        dtype: a.dtype.clone(),
    });
    derive(
        "general",
        active_id.as_ref(),
        &FallbackIdentity {
            name: explicit_name.unwrap_or(&models.embedding),
            dim: models.voyage_output_dimension,
            dtype: &models.voyage_output_dtype,
        },
    )
}

/// Derive the CODE embedder identity for an ingest run from the active model's
/// `code` half (the authority), with local config as a logged fallback.
fn derive_code_ingest_identity(
    active: Option<&crate::commands::models::ActiveModelResponse>,
    models: &mnm_core::config::ModelsConfig,
) -> mnm_core::embedder_identity::EmbedderIdentity {
    use mnm_core::embedder_identity::{derive, ActiveModelIdentity, FallbackIdentity};
    let active_id = active
        .and_then(|a| a.code.as_ref())
        .map(|c| ActiveModelIdentity {
            name: c.name.clone(),
            dim: u32::try_from(c.dim).unwrap_or(models.voyage_output_dimension),
            dtype: c.dtype.clone(),
        });
    derive(
        "code",
        active_id.as_ref(),
        &FallbackIdentity {
            name: &models.code_embedding,
            dim: models.voyage_output_dimension,
            dtype: &models.voyage_output_dtype,
        },
    )
}

/// Embed every chunk of `docs` in place: general contextualized vectors for
/// all chunks (per-document context groups, spec §6), plus flat voyage-code-3
/// vectors for chunks of Code-kind documents when `code` is supplied. Returns
/// the total Voyage tokens consumed across both models.
///
/// General path: each document's chunks are partitioned into context groups
/// ([`mnm_content::context_group::balanced_groups`]) and the groups packed into
/// Voyage requests by [`plan_group_batches`]. Code path: Code-kind chunks are
/// flat-embedded in sub-requests bounded by [`plan_subbatches`] (same limits
/// as before). Input order is preserved on both paths; vectors are distributed
/// back positionally.
///
/// # Errors
///
/// Errors if any Voyage call fails or a returned vector count does not match
/// the corresponding chunk count.
async fn embed_batch(
    general: GeneralEmbedSource<'_>,
    code: Option<EmbedSource<'_>>,
    docs: &mut [DocumentUpload],
) -> Result<u64> {
    let mut tokens = 0u64;

    // ── General: per-document context groups, packed into Voyage requests ──
    // Each entry: (texts, token_total) for one context group. `token_count` is
    // a non-negative i32; the `0` fallback only affects budgeting — never
    // vector alignment, which is positional.
    let mut groups: Vec<(Vec<String>, usize)> = Vec::new();
    for d in docs.iter() {
        let counts: Vec<u32> = d
            .chunks
            .iter()
            .map(|c| u32::try_from(c.token_count).unwrap_or(0))
            .collect();
        for r in mnm_content::context_group::balanced_groups(
            &counts,
            mnm_content::context_group::context_group_limit(),
        ) {
            let texts: Vec<String> = d.chunks[r.clone()]
                .iter()
                .map(|c| c.content.clone())
                .collect();
            let total: usize = counts[r].iter().map(|&t| t as usize).sum();
            groups.push((texts, total));
        }
    }
    if !groups.is_empty() {
        let plan = plan_group_batches(&groups);
        let mut general_vectors: Vec<Vec<f32>> = Vec::new();
        let mut cursor = groups.into_iter();
        for take in plan {
            let req_groups: Vec<Vec<String>> =
                cursor.by_ref().take(take).map(|(texts, _)| texts).collect();
            let out = mnm_embedding::client::embed_general_groups(req_groups, general)
                .await
                .map_err(|e| anyhow!("embed context groups via Voyage: {e}"))?;
            tokens = tokens.saturating_add(out.total_tokens);
            general_vectors.extend(out.groups.into_iter().flatten());
        }
        attach_embeddings(docs, general_vectors)?;
    }

    // ── Code: flat embed for Code-kind documents' chunks ──
    if let Some(code_src) = code {
        let mut code_texts: Vec<(String, usize)> = Vec::new();
        for d in docs.iter() {
            if d.kind == DocumentKind::Code {
                for c in &d.chunks {
                    code_texts
                        .push((c.content.clone(), usize::try_from(c.token_count).unwrap_or(0)));
                }
            }
        }
        if !code_texts.is_empty() {
            let counts: Vec<usize> = code_texts.iter().map(|(_, t)| *t).collect();
            let plan = plan_subbatches(
                &counts,
                VOYAGE_MAX_TEXTS_PER_REQUEST,
                VOYAGE_MAX_TOKENS_PER_REQUEST,
            );
            let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(code_texts.len());
            let mut chunks = code_texts.into_iter().map(|(s, _)| s);
            for size in plan {
                let sub: Vec<String> = chunks.by_ref().take(size).collect();
                let out = mnm_embedding::client::embed_code(sub, InputType::Document, code_src)
                    .await
                    .map_err(|e| anyhow!("embed code chunks via Voyage: {e}"))?;
                tokens = tokens.saturating_add(out.total_tokens);
                vectors.extend(out.vectors);
            }
            attach_code_embeddings(docs, vectors)?;
        }
    }
    Ok(tokens)
}

/// Pack context groups into Voyage requests bounded by ≤1 000 input groups,
/// ≤100 K summed tokens (headroom under Voyage's ~120 K hard limit), and
/// ≤16 K total chunks per request. Returns group-counts per request, summing
/// to `groups.len()`. A single group that alone exceeds a bound still goes
/// out as its own request (never dropped).
fn plan_group_batches(groups: &[(Vec<String>, usize)]) -> Vec<usize> {
    const MAX_INPUTS: usize = 1_000;
    const MAX_TOKENS: usize = 100_000; // headroom under Voyage's 120K
    const MAX_CHUNKS: usize = 16_000;
    let mut sizes = Vec::new();
    let (mut n, mut toks, mut chunks) = (0usize, 0usize, 0usize);
    for (texts, total) in groups {
        let over = n > 0
            && (n >= MAX_INPUTS
                || toks.saturating_add(*total) > MAX_TOKENS
                || chunks.saturating_add(texts.len()) > MAX_CHUNKS);
        if over {
            sizes.push(n);
            n = 0;
            toks = 0;
            chunks = 0;
        }
        n += 1;
        toks = toks.saturating_add(*total);
        chunks = chunks.saturating_add(texts.len());
    }
    if n > 0 {
        sizes.push(n);
    }
    sizes
}

/// Distribute one code vector per Code-kind chunk, in document-then-chunk
/// order. Non-code documents are skipped (their `code_embedding` stays `None`).
///
/// # Errors
///
/// Errors if `vectors.len()` does not equal the total Code-kind chunk count.
fn attach_code_embeddings(docs: &mut [DocumentUpload], vectors: Vec<Vec<f32>>) -> Result<()> {
    let total: usize = docs
        .iter()
        .filter(|d| d.kind == DocumentKind::Code)
        .map(|d| d.chunks.len())
        .sum();
    if vectors.len() != total {
        return Err(anyhow!("code embedder returned {} vectors for {total} chunks", vectors.len()));
    }
    let mut it = vectors.into_iter();
    for d in docs.iter_mut() {
        if d.kind != DocumentKind::Code {
            continue;
        }
        for c in &mut d.chunks {
            c.code_embedding = it.next();
        }
    }
    Ok(())
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

#[cfg(test)]
mod plan_group_batches_tests {
    use super::plan_group_batches;

    /// Build `n` groups, each with `texts` empty strings and `tokens` summed
    /// tokens. Only the shape matters for the packer.
    fn mk_groups(n: usize, texts: usize, tokens: usize) -> Vec<(Vec<String>, usize)> {
        (0..n)
            .map(|_| (vec![String::new(); texts], tokens))
            .collect()
    }

    #[test]
    fn splits_on_group_cap() {
        // 1 001 tiny groups, input cap 1 000 -> 1 000 + 1.
        assert_eq!(plan_group_batches(&mk_groups(1_001, 1, 1)), vec![1_000, 1]);
    }

    #[test]
    fn splits_on_token_cap() {
        // 30k-token groups: three fit (90k); the fourth (120k) starts a new batch.
        assert_eq!(plan_group_batches(&mk_groups(7, 1, 30_000)), vec![3, 3, 1]);
    }

    #[test]
    fn splits_on_chunk_cap() {
        // 9k-chunk groups: two would be 18k chunks > 16k, so one group per batch.
        assert_eq!(plan_group_batches(&mk_groups(3, 9_000, 10)), vec![1, 1, 1]);
    }

    #[test]
    fn oversized_group_goes_out_alone() {
        let groups = vec![
            (vec![String::new()], 10_000),
            (vec![String::new()], 150_000), // alone exceeds the token cap
            (vec![String::new()], 10_000),
        ];
        assert_eq!(plan_group_batches(&groups), vec![1, 1, 1]);
    }

    #[test]
    fn empty_input_yields_no_batches() {
        assert!(plan_group_batches(&[]).is_empty());
    }

    #[test]
    fn sizes_sum_to_group_count() {
        let sizes = plan_group_batches(&mk_groups(2_345, 2, 500));
        assert_eq!(sizes.iter().sum::<usize>(), 2_345);
    }
}

/// Rough estimate of `doc`'s serialized JSON upload size in bytes, INCLUDING the
/// embedding vectors attached per chunk before upload (they are `None` at plan
/// time but dominate the real payload). Code-kind chunks count the vector cost
/// twice — they may also carry a `code_embedding` (dual embeddings, D1); when
/// code embeddings are opted out this merely over-estimates, which the packer
/// tolerates by design. Intentionally approximate.
fn estimated_upload_bytes(doc: &DocumentUpload) -> usize {
    let vectors_per_chunk = if doc.kind == DocumentKind::Code { 2 } else { 1 };
    EST_PER_DOC_OVERHEAD
        + doc.path.len()
        + doc
            .chunks
            .iter()
            .map(|c| {
                c.content.len()
                    + EST_PER_CHUNK_OVERHEAD
                    + vectors_per_chunk * EST_EMBED_DIM * EST_BYTES_PER_EMBED_FLOAT
            })
            .sum::<usize>()
}

/// Greedily pack documents into upload batches bounded by BOTH a document-count
/// ceiling (`max_docs`) and an estimated byte budget (`byte_target`). Always
/// emits at least one document per batch — a single document larger than the
/// target goes alone (and relies on the server's headroom / retry-split).
fn pack_upload_batches(
    docs: Vec<DocumentUpload>,
    max_docs: usize,
    byte_target: usize,
) -> Vec<Vec<DocumentUpload>> {
    let mut out = Vec::new();
    let mut cur: Vec<DocumentUpload> = Vec::new();
    let mut cur_bytes = 0usize;
    for doc in docs {
        let bytes = estimated_upload_bytes(&doc);
        if !cur.is_empty() && (cur.len() >= max_docs || cur_bytes + bytes > byte_target) {
            out.push(std::mem::take(&mut cur));
            cur_bytes = 0;
        }
        cur_bytes += bytes;
        cur.push(doc);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// PUT one already-embedded batch. On HTTP 413 (payload too large), split the
/// documents into two approximate halves and PUT each ONCE (single-level, no
/// recursion — a half that still 413s propagates its error). `batch_index` /
/// `batch_count` are informational (server logs only).
async fn upload_documents_with_split(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    embedding_model: &str,
    documents: Vec<DocumentUpload>,
    batch_index: usize,
    batch_count: usize,
) -> Result<UploadDocumentsResponse> {
    let body = UploadDocumentsRequest {
        documents,
        batch_index: Some(batch_index),
        batch_count: Some(batch_count),
        embedding_model: Some(embedding_model.to_owned()),
    };
    match put_json::<_, UploadDocumentsResponse>(client, url, token, &body).await {
        Ok(r) => Ok(r),
        Err(e) if is_payload_too_large(&e) && body.documents.len() > 1 => {
            let docs = body.documents; // recover & split (put_json only borrowed `body`)
            let mid = docs.len() / 2; // >= 1 because len > 1
            let mut it = docs.into_iter();
            let first: Vec<DocumentUpload> = it.by_ref().take(mid).collect();
            let second: Vec<DocumentUpload> = it.collect();
            let r1 = put_documents_once(
                client,
                url,
                token,
                embedding_model,
                first,
                batch_index,
                batch_count,
            )
            .await?;
            let r2 = put_documents_once(
                client,
                url,
                token,
                embedding_model,
                second,
                batch_index,
                batch_count,
            )
            .await?;
            Ok(merge_split_responses(r1, r2))
        }
        Err(e) => Err(e),
    }
}

/// Merge the two half-batch responses produced by the 413 split-retry path:
/// sum `accepted`/`carried` and concatenate `conflicts` from both halves
/// exactly once (first half then second). Pure so the merge — the one spot a
/// future refactor could double-count or drop conflicts — is unit-testable
/// without an HTTP round-trip.
fn merge_split_responses(
    first: UploadDocumentsResponse,
    second: UploadDocumentsResponse,
) -> UploadDocumentsResponse {
    let mut conflicts = first.conflicts;
    conflicts.extend(second.conflicts);
    UploadDocumentsResponse {
        accepted: first.accepted + second.accepted,
        carried: first.carried + second.carried,
        conflicts,
    }
}

/// Build the upload request and PUT it exactly once (no splitting).
async fn put_documents_once(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    embedding_model: &str,
    documents: Vec<DocumentUpload>,
    batch_index: usize,
    batch_count: usize,
) -> Result<UploadDocumentsResponse> {
    let body = UploadDocumentsRequest {
        documents,
        batch_index: Some(batch_index),
        batch_count: Some(batch_count),
        embedding_model: Some(embedding_model.to_owned()),
    };
    put_json(client, url, token, &body).await
}

fn is_payload_too_large(e: &anyhow::Error) -> bool {
    e.to_string().contains("413")
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
    /// Code-embedding wire id (`name@revision`) when this run uploads
    /// voyage-code-3 vectors (dual embeddings, D1); omitted when code
    /// embeddings are opted out, which records
    /// `code_embedding_model_id = NULL` on the source_version.
    #[serde(skip_serializing_if = "Option::is_none")]
    code_embedding_model: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_count: Option<usize>,
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
    package: Option<mnm_core::types::PackageRef>,
}

#[derive(Debug, Serialize, Clone)]
struct ChunkUpload {
    chunk_index: i32,
    total_chunks: i32,
    content: String,
    content_hash: String,
    heading_path: Vec<String>,
    symbol_path: Vec<mnm_core::types::SymbolSegment>,
    start_byte: i32,
    end_byte: i32,
    token_count: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    embedding: Option<Vec<f32>>,
    /// Flat voyage-code-3 vector, only for chunks of Code-kind documents when
    /// code embeddings are enabled (dual embeddings, D1).
    #[serde(skip_serializing_if = "Option::is_none")]
    code_embedding: Option<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct UploadDocumentsResponse {
    accepted: usize,
    carried: usize,
    /// Per-document conflicts — documents the server did NOT insert. Surfaced
    /// (warn-logged + counted) at run end so partial-failure uploads aren't
    /// reported as fully successful.
    #[serde(default)]
    conflicts: Vec<mnm_core::ingest::UploadConflict>,
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
) -> Option<mnm_core::types::PackageRef> {
    if rel_path.extension().and_then(|e| e.to_str()) == Some("compact") {
        return mnm_content::detect_compact_package(content);
    }
    let abs = source_root.join(rel_path);
    mnm_content::package::detect(&abs, source_root).map(|p| mnm_core::types::PackageRef {
        kind: p.kind,
        name: p.name,
        version: p.version,
        manifest_path: Some(p.manifest_path.display().to_string()),
    })
}

/// Machine-extract version provenance for one walked file (spec §1.1): code
/// documents only — pragma constraints for `.compact`, allowlisted manifest
/// dependencies for files in an npm/cargo package. Prose gets nothing.
fn build_extracted(
    source_root: &std::path::Path,
    rel_path: &std::path::Path,
    content: &str,
    kind: mnm_core::types::DocumentKind,
) -> mnm_core::provenance::Provenance {
    use mnm_core::provenance::{LanguageTarget, Provenance};
    if kind != mnm_core::types::DocumentKind::Code {
        return Provenance::default();
    }
    let mut out = Provenance::default();
    if rel_path.extension().and_then(|e| e.to_str()) == Some("compact") {
        if let Some(expr) = mnm_content::detect_language_version(content) {
            out.language_targets = vec![LanguageTarget {
                name: "compact".into(),
                version_constraint: Some(expr),
            }];
        }
    }
    let abs = source_root.join(rel_path);
    if let Some(pkg) = mnm_content::package::detect(&abs, source_root) {
        let manifest_abs = source_root.join(&pkg.manifest_path);
        out.sdk_dependencies =
            mnm_content::extract::extract_manifest_deps(&manifest_abs, source_root);
    }
    out
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
    /// Documents the server reported as conflicts (NOT inserted). Always present
    /// in `--json` output so consumers can detect partial-failure uploads.
    conflict_count: usize,
    docs_with_language_targets: usize,
    docs_with_sdk_dependencies: usize,
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

fn format_success(out: &SuccessOutput<'_>, json: bool) -> String {
    if json {
        return serde_json::to_string(out).unwrap_or_default();
    }
    let (slug, rev, added, carried) =
        (out.source_slug, out.revision, out.documents_added, out.documents_carried);
    // Conflicts are silent data loss; only render the clause when there are any,
    // so the clean path stays terse, but a non-zero count is always visible.
    let conflicts = if out.conflict_count > 0 {
        format!(", {} conflicts", out.conflict_count)
    } else {
        String::new()
    };
    if let Some(prev) = out.demoted_revision {
        format!(
            "ingested `{slug}` rev {rev} (was {prev}); +{added} new, {carried} carried{conflicts}"
        )
    } else {
        format!("ingested `{slug}` rev {rev} (first version); +{added} new{conflicts}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::models::{ActiveCode, ActiveModelResponse};

    /// An active-model response whose `{name, dim, dtype}` deliberately differ
    /// from `ModelsConfig`'s defaults, so a test can prove the embedder identity
    /// is taken from the active response and NOT from config.
    fn divergent_active() -> ActiveModelResponse {
        ActiveModelResponse {
            name: "voyage-context-3".to_owned(),
            revision: 1,
            dim: 512,
            provider: "voyageai".to_owned(),
            dtype: "int8".to_owned(),
            code: Some(ActiveCode {
                name: "voyage-code-3".to_owned(),
                revision: 2,
                dim: 256,
                provider: "voyageai".to_owned(),
                dtype: "uint8".to_owned(),
            }),
        }
    }

    /// THE cross-element drift property (ingest side): when the active model is
    /// available, the GENERAL and CODE embedders are built from the active
    /// response's `{name, dim, dtype}`, NOT from the (divergent) local config.
    /// `auto` means "follow the corpus", so the general name is the active name.
    #[test]
    fn ingest_identities_come_from_active_not_config() {
        let active = divergent_active();
        // Config defaults are voyage-context-3 / voyage-code-3 / 1024 / "float".
        let models = mnm_core::config::ModelsConfig::default();

        let general =
            derive_general_ingest_identity(DEFAULT_EMBEDDING_MODEL, Some(&active), &models);
        assert_eq!(general.name, "voyage-context-3");
        assert_eq!(general.dim, 512, "dim must come from the active response, not config 1024");
        assert_eq!(general.dtype, "int8", "dtype must come from the active response, not config");

        let code = derive_code_ingest_identity(Some(&active), &models);
        assert_eq!(code.name, "voyage-code-3");
        assert_eq!(code.dim, 256, "code dim must come from the active `code` half");
        assert_eq!(code.dtype, "uint8", "code dtype must come from the active `code` half");
    }

    /// An explicit `--embedding-model` override (e.g. `models migrate`) drives
    /// the GENERAL embedder's name to the targeted model's bare name, so the
    /// embedder matches the wire id the run is labelled with.
    #[test]
    fn ingest_general_identity_honours_explicit_override() {
        let active = divergent_active();
        let models = mnm_core::config::ModelsConfig::default();
        let general = derive_general_ingest_identity("voyage-context-4@3", Some(&active), &models);
        // Name parsed from the override wire id; dim/dtype still from the corpus.
        assert_eq!(general.name, "voyage-context-4");
        assert_eq!(general.dim, 512);
        assert_eq!(general.dtype, "int8");
    }

    /// Offline path: with no active-model response, the identities fall back to
    /// local config (logged). This preserves offline behavior.
    #[test]
    fn ingest_identities_fall_back_to_config_when_active_absent() {
        let models = mnm_core::config::ModelsConfig::default();
        let general = derive_general_ingest_identity(DEFAULT_EMBEDDING_MODEL, None, &models);
        assert_eq!(general.name, models.embedding);
        assert_eq!(general.dim, models.voyage_output_dimension);
        assert_eq!(general.dtype, models.voyage_output_dtype);

        let code = derive_code_ingest_identity(None, &models);
        assert_eq!(code.name, models.code_embedding);
        assert_eq!(code.dim, models.voyage_output_dimension);
        assert_eq!(code.dtype, models.voyage_output_dtype);
    }

    #[test]
    fn parses_chunk_and_filter_flags() {
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
            "--chunk-tokens",
            "256",
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
        assert_eq!(args.chunk_tokens, 256);
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
        let s = format_success(
            &SuccessOutput {
                action: "ingest",
                source_slug: "docs",
                revision: 1,
                demoted_revision: None,
                documents_added: 5,
                documents_carried: 0,
                conflict_count: 0,
                docs_with_language_targets: 0,
                docs_with_sdk_dependencies: 0,
            },
            false,
        );
        assert!(s.contains("first version"));
        assert!(s.contains("+5 new"));
        assert!(!s.contains("conflict"), "no conflict clause when count is zero");
    }

    #[test]
    fn success_human_output_with_demote() {
        let s = format_success(
            &SuccessOutput {
                action: "ingest",
                source_slug: "docs",
                revision: 2,
                demoted_revision: Some(1),
                documents_added: 3,
                documents_carried: 4,
                conflict_count: 0,
                docs_with_language_targets: 0,
                docs_with_sdk_dependencies: 0,
            },
            false,
        );
        assert!(s.contains("rev 2"));
        assert!(s.contains("was 1"));
        assert!(s.contains("+3 new"));
        assert!(s.contains("4 carried"));
    }

    /// A server response carrying conflicts deserializes into the shared
    /// `UploadConflict` type with a non-zero count — the CLI must not decode it
    /// as opaque-and-ignored, or the operator loses documents silently.
    #[test]
    fn upload_response_decodes_conflicts() {
        let body = serde_json::json!({
            "accepted": 2,
            "carried": 1,
            "conflicts": [
                { "path": "a/dup.md", "reason": "duplicate path in this batch" },
                { "path": "b/bad.md", "reason": "insert failed: boom" },
            ],
        });
        let resp: UploadDocumentsResponse = serde_json::from_value(body).unwrap();
        assert_eq!(resp.accepted, 2);
        assert_eq!(resp.carried, 1);
        assert_eq!(resp.conflicts.len(), 2, "conflicts must be a non-zero count, not dropped");
        assert_eq!(resp.conflicts[0].path, "a/dup.md");
        assert_eq!(resp.conflicts[0].reason, "duplicate path in this batch");
    }

    /// A response with no `conflicts` field (or an empty list) yields an empty
    /// vec — the clean path stays terse.
    #[test]
    fn upload_response_defaults_conflicts_to_empty() {
        let resp: UploadDocumentsResponse =
            serde_json::from_value(serde_json::json!({ "accepted": 5, "carried": 0 })).unwrap();
        assert!(resp.conflicts.is_empty());
    }

    /// The 413 split-retry merge (`merge_split_responses`) sums `accepted` /
    /// `carried` and concatenates each half's `conflicts` exactly once, in
    /// order. This is the one spot a future refactor could double-count or drop
    /// conflicts, so it is exercised directly (not via the hand-mirrored loop).
    #[test]
    fn merge_split_responses_sums_and_concatenates_conflicts_once() {
        let first: UploadDocumentsResponse = serde_json::from_value(serde_json::json!({
            "accepted": 3,
            "carried": 1,
            "conflicts": [{ "path": "a/dup.md", "reason": "duplicate path in this batch" }],
        }))
        .unwrap();
        let second: UploadDocumentsResponse = serde_json::from_value(serde_json::json!({
            "accepted": 2,
            "carried": 4,
            "conflicts": [
                { "path": "b/bad.md", "reason": "insert failed: boom" },
                { "path": "c/dupe.md", "reason": "duplicate path in this batch" },
            ],
        }))
        .unwrap();

        let merged = merge_split_responses(first, second);

        assert_eq!(merged.accepted, 5, "accepted is the sum of both halves");
        assert_eq!(merged.carried, 5, "carried is the sum of both halves");
        assert_eq!(
            merged.conflicts.len(),
            3,
            "conflicts concatenated exactly once — not dropped, not doubled"
        );
        // Order is preserved: first half's conflicts precede the second half's.
        assert_eq!(merged.conflicts[0].path, "a/dup.md");
        assert_eq!(merged.conflicts[1].path, "b/bad.md");
        assert_eq!(merged.conflicts[2].path, "c/dupe.md");
    }

    /// A 413 split where one half reports no conflicts must carry the other
    /// half's conflicts through unchanged (the clean half does not mask the
    /// dirty half, and the empty list adds nothing).
    #[test]
    fn merge_split_responses_one_clean_half() {
        let clean: UploadDocumentsResponse = serde_json::from_value(serde_json::json!({
            "accepted": 4,
            "carried": 0,
        }))
        .unwrap();
        let dirty: UploadDocumentsResponse = serde_json::from_value(serde_json::json!({
            "accepted": 1,
            "carried": 0,
            "conflicts": [{ "path": "b/bad.md", "reason": "insert failed: boom" }],
        }))
        .unwrap();

        let merged = merge_split_responses(clean, dirty);
        assert_eq!(merged.accepted, 5);
        assert_eq!(merged.carried, 0);
        assert_eq!(merged.conflicts.len(), 1);
        assert_eq!(merged.conflicts[0].path, "b/bad.md");
    }

    /// Conflicts accumulated across batches (as `run_inner` does, and as the 413
    /// split-retry path concatenates its halves) surface a non-zero count in
    /// `RunStats` and in the rendered success summary.
    #[test]
    fn conflicts_surface_in_stats_and_summary() {
        let batch_a: UploadDocumentsResponse = serde_json::from_value(serde_json::json!({
            "accepted": 1,
            "carried": 0,
            "conflicts": [{ "path": "a/dup.md", "reason": "duplicate path in this batch" }],
        }))
        .unwrap();
        let batch_b: UploadDocumentsResponse = serde_json::from_value(serde_json::json!({
            "accepted": 1,
            "carried": 1,
            "conflicts": [{ "path": "b/bad.md", "reason": "insert failed: boom" }],
        }))
        .unwrap();

        // Accumulate via the real merge helper (same code the 413 split-retry
        // path uses) rather than hand-mirroring the `extend` loop.
        let merged = merge_split_responses(batch_a, batch_b);

        let stats = RunStats {
            added: 1,
            carried: 1,
            deleted: 0,
            batch_count: 2,
            failed_batch_index: None,
            total_tokens: 0,
            conflicts: merged.conflicts,
        };
        assert_eq!(stats.conflicts.len(), 2, "RunStats carries a non-zero conflict count");

        let s = format_success(
            &SuccessOutput {
                action: "ingest",
                source_slug: "docs",
                revision: 2,
                demoted_revision: Some(1),
                documents_added: stats.added,
                documents_carried: stats.carried,
                conflict_count: stats.conflicts.len(),
                docs_with_language_targets: 0,
                docs_with_sdk_dependencies: 0,
            },
            false,
        );
        assert!(s.contains("2 conflicts"), "human summary surfaces the conflict count: {s}");
    }

    /// `--json` output always carries `conflict_count` so machine consumers can
    /// detect partial-failure uploads.
    #[test]
    fn success_json_output_includes_conflict_count() {
        let s = format_success(
            &SuccessOutput {
                action: "ingest",
                source_slug: "docs",
                revision: 2,
                demoted_revision: Some(1),
                documents_added: 3,
                documents_carried: 4,
                conflict_count: 2,
                docs_with_language_targets: 0,
                docs_with_sdk_dependencies: 0,
            },
            true,
        );
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["conflict_count"], 2);
    }

    /// The shared `UploadConflict` serializes to exactly `{path, reason}` — the
    /// server's wire shape must be byte-identical to what the CLI decodes.
    #[test]
    fn upload_conflict_wire_shape_is_path_reason() {
        let c = mnm_core::ingest::UploadConflict {
            path: "a/dup.md".to_owned(),
            reason: "duplicate path in this batch".to_owned(),
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, r#"{"path":"a/dup.md","reason":"duplicate path in this batch"}"#);
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
    fn no_code_embeddings_flag_parses_and_defaults_false() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            inner: Args,
        }
        // Absent → false (code embeddings follow the manifest, default on).
        let off = Wrap::try_parse_from(["ingest-run", "--source-slug", "s", "m.yaml"]).unwrap();
        assert!(!off.inner.no_code_embeddings);
        // Present → true (overrides the manifest's `code_embeddings: true`).
        let on = Wrap::try_parse_from([
            "ingest-run",
            "--source-slug",
            "s",
            "--no-code-embeddings",
            "m.yaml",
        ])
        .unwrap();
        assert!(on.inner.no_code_embeddings);
    }

    #[test]
    fn attach_code_embeddings_targets_code_docs_only() {
        let mut code_a = mk_doc(2, 1);
        code_a.kind = DocumentKind::Code;
        let markdown = mk_doc(1, 1);
        let mut code_b = mk_doc(1, 1);
        code_b.kind = DocumentKind::Code;
        let mut docs = vec![code_a, markdown, code_b];
        // 3 code chunks total (2 + 1); the markdown chunk gets nothing.
        let vectors = vec![vec![1.0_f32], vec![2.0], vec![3.0]];
        attach_code_embeddings(&mut docs, vectors).unwrap();
        assert_eq!(docs[0].chunks[0].code_embedding, Some(vec![1.0]));
        assert_eq!(docs[0].chunks[1].code_embedding, Some(vec![2.0]));
        assert_eq!(docs[1].chunks[0].code_embedding, None);
        assert_eq!(docs[2].chunks[0].code_embedding, Some(vec![3.0]));
    }

    #[test]
    fn attach_code_embeddings_rejects_count_mismatch() {
        let mut doc = mk_doc(2, 1);
        doc.kind = DocumentKind::Code;
        let mut docs = vec![doc];
        // 2 code chunks but only 1 vector → error, nothing partially attached.
        assert!(attach_code_embeddings(&mut docs, vec![vec![1.0_f32]]).is_err());
    }

    #[test]
    fn chunk_upload_skips_code_embedding_when_none() {
        let c = mk_chunk(0);
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("code_embedding"), "None code_embedding must be omitted: {s}");
    }

    #[test]
    fn chunk_upload_serializes_code_embedding_when_present() {
        let mut c = mk_chunk(0);
        c.code_embedding = Some(vec![0.25_f32; 4]);
        let v: serde_json::Value = serde_json::to_value(&c).unwrap();
        assert_eq!(v["code_embedding"].as_array().map(Vec::len), Some(4));
    }

    #[test]
    fn start_run_request_carries_optional_code_embedding_model() {
        let without = StartIngestRunRequest {
            ingest_cli_version: "1.0.0".into(),
            embedding_model: "voyage-context-3@1".into(),
            code_embedding_model: None,
            note: None,
        };
        let s = serde_json::to_string(&without).unwrap();
        assert!(!s.contains("code_embedding_model"), "None must be omitted: {s}");

        let with = StartIngestRunRequest {
            code_embedding_model: Some("voyage-code-3@1".into()),
            ..without
        };
        let v: serde_json::Value = serde_json::to_value(&with).unwrap();
        assert_eq!(v["code_embedding_model"], "voyage-code-3@1");
    }

    #[test]
    fn estimated_upload_bytes_counts_both_vectors_for_code_docs() {
        // Code-kind chunks carry `embedding` + `code_embedding`, so the
        // (deliberately conservative) estimate doubles the vector cost.
        let md = mk_doc(2, 100);
        let mut code = mk_doc(2, 100);
        code.kind = DocumentKind::Code;
        assert_eq!(
            estimated_upload_bytes(&code) - estimated_upload_bytes(&md),
            2 * EST_EMBED_DIM * EST_BYTES_PER_EMBED_FLOAT,
        );
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
            code_embedding: None,
        }
    }

    /// Build a `DocumentUpload` with `n` chunks each holding `content_len` bytes
    /// of content. Path defaults to `"p"`; only the size-relevant fields matter
    /// for the estimate/packer tests.
    fn mk_doc(n_chunks: usize, content_len: usize) -> DocumentUpload {
        let chunks = (0..n_chunks)
            .map(|i| ChunkUpload {
                chunk_index: i32::try_from(i).unwrap_or(i32::MAX),
                total_chunks: i32::try_from(n_chunks).unwrap_or(i32::MAX),
                content: "x".repeat(content_len),
                content_hash: "c".into(),
                heading_path: vec![],
                symbol_path: vec![],
                start_byte: 0,
                end_byte: 0,
                token_count: 0,
                embedding: None,
                code_embedding: None,
            })
            .collect();
        DocumentUpload {
            path: "p".into(),
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
            chunks,
        }
    }

    #[test]
    fn estimated_upload_bytes_empty_chunks_is_doc_overhead_plus_path() {
        let doc = mk_doc(0, 0);
        assert_eq!(estimated_upload_bytes(&doc), EST_PER_DOC_OVERHEAD + doc.path.len());
    }

    #[test]
    fn estimated_upload_bytes_grows_with_chunk_count() {
        let one = estimated_upload_bytes(&mk_doc(1, 100));
        let two = estimated_upload_bytes(&mk_doc(2, 100));
        let three = estimated_upload_bytes(&mk_doc(3, 100));
        assert!(two > one, "two chunks must estimate larger than one");
        assert!(three > two, "three chunks must estimate larger than two");
        // Each extra chunk adds exactly the per-chunk overhead + embedding +
        // content bytes.
        let per_chunk = 100 + EST_PER_CHUNK_OVERHEAD + EST_EMBED_DIM * EST_BYTES_PER_EMBED_FLOAT;
        assert_eq!(two - one, per_chunk);
        assert_eq!(three - two, per_chunk);
    }

    #[test]
    fn estimated_upload_bytes_grows_with_content_length() {
        let short = estimated_upload_bytes(&mk_doc(2, 10));
        let long = estimated_upload_bytes(&mk_doc(2, 1000));
        assert!(long > short, "longer content must estimate larger");
        // Two chunks, so 990 extra bytes per chunk → 1980 total.
        assert_eq!(long - short, 2 * (1000 - 10));
    }

    #[test]
    fn pack_upload_batches_respects_max_docs() {
        // Tiny docs so the byte target never bites; only the doc cap should.
        let docs: Vec<_> = (0..7).map(|_| mk_doc(0, 0)).collect();
        let batches = pack_upload_batches(docs, 3, usize::MAX);
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), vec![3, 3, 1]);
    }

    #[test]
    fn pack_upload_batches_respects_byte_target() {
        // Each doc estimates ~ EST_PER_DOC_OVERHEAD + 1 chunk's bytes; pick a
        // target that fits exactly two docs per batch.
        let doc = mk_doc(1, 0);
        let per_doc = estimated_upload_bytes(&doc);
        let target = per_doc * 2; // exactly two fit; the third must start a new batch
        let docs: Vec<_> = (0..5).map(|_| mk_doc(1, 0)).collect();
        let batches = pack_upload_batches(docs, usize::MAX, target);
        // No multi-doc batch's summed estimate may exceed the target.
        for b in &batches {
            let summed: usize = b.iter().map(estimated_upload_bytes).sum();
            if b.len() > 1 {
                assert!(summed <= target, "multi-doc batch {summed} exceeds target {target}");
            }
            assert!(!b.is_empty(), "no empty batches");
        }
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 2, 1]);
    }

    #[test]
    fn pack_upload_batches_oversized_doc_goes_alone() {
        // A single doc whose estimate alone exceeds the target must still ship,
        // in its own batch.
        let big = mk_doc(50, 1000);
        let big_bytes = estimated_upload_bytes(&big);
        let tiny_target = big_bytes / 2; // big alone exceeds the target
        let docs = vec![mk_doc(0, 0), big, mk_doc(0, 0)];
        let batches = pack_upload_batches(docs, usize::MAX, tiny_target);
        // The big doc is over target, so it neither merges forward nor pulls a
        // neighbour in: it occupies its own batch.
        assert!(
            batches
                .iter()
                .any(|b| b.len() == 1 && b[0].chunks.len() == 50),
            "oversized doc must occupy its own batch: {:?}",
            batches.iter().map(Vec::len).collect::<Vec<_>>()
        );
        for b in &batches {
            assert!(!b.is_empty(), "no empty batches");
        }
    }

    #[test]
    fn pack_upload_batches_never_emits_empty_batch() {
        assert!(pack_upload_batches(vec![], 3, 1024).is_empty(), "no docs → no batches");
        let batches = pack_upload_batches(vec![mk_doc(1, 0)], 3, 1024);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 1);
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
            code_embedding: None,
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
                    code_embedding: None,
                }],
            }],
            batch_index: Some(0),
            batch_count: Some(1),
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
    use super::{build_extracted, detect_package_ref};
    use std::path::Path;

    #[test]
    fn extracted_provenance_for_code_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"app","version":"1.0.0","dependencies":{"@midnight-ntwrk/midnight-js":"^1.4.0"}}"#,
        )
        .unwrap();
        std::fs::write(root.path().join("src/x.compact"), "pragma language_version >= 0.23;\n")
            .unwrap();
        std::fs::write(root.path().join("src/y.ts"), "export const x = 1;").unwrap();
        std::fs::write(root.path().join("README.md"), "# hi").unwrap();

        let compact = build_extracted(
            root.path(),
            Path::new("src/x.compact"),
            "pragma language_version >= 0.23;\n",
            mnm_core::types::DocumentKind::Code,
        );
        assert_eq!(compact.language_targets[0].name, "compact");
        assert_eq!(compact.language_targets[0].version_constraint.as_deref(), Some(">=0.23"));

        let ts = build_extracted(
            root.path(),
            Path::new("src/y.ts"),
            "export const x = 1;",
            mnm_core::types::DocumentKind::Code,
        );
        assert_eq!(ts.sdk_dependencies.len(), 1);

        // prose: never extracted (spec §1)
        let md = build_extracted(
            root.path(),
            Path::new("README.md"),
            "# hi",
            mnm_core::types::DocumentKind::Markdown,
        );
        assert!(md.language_targets.is_empty() && md.sdk_dependencies.is_empty());
    }

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

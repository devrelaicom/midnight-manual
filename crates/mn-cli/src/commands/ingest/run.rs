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
//!    documents in batches of `--batch-size` (default 25) each.
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
use mn_telemetry::events::{Component, EventPayload, Outcome};
use mn_telemetry::{Event, TelemetryClient};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

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

    /// Embedding-model wire id (`name@revision`). Defaults to
    /// `bge-base-en-v1.5@1` to match the corpus's current model.
    #[arg(long, default_value = "bge-base-en-v1.5@1")]
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
    /// 413 responses from the server (local embedding inflates each batch).
    #[arg(long, default_value_t = 25)]
    pub batch_size: usize,

    /// Embed on the server instead of locally. Off by default: the CLI embeds
    /// chunks with its local model and uploads the vectors, so the server
    /// never has to load the model. Use this when the local model is
    /// unavailable or you want the server to embed.
    #[arg(long)]
    pub enable_server_embedding: bool,

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
}

/// Dispatch.
///
/// # Errors
///
/// Returns `anyhow::Error` if the manifest cannot be read, the source tree
/// walk fails, the auth.toml cannot be loaded, or any of the HTTP round-trips
/// fail.
pub async fn run(
    args: Args,
    server_flag: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server_flag);
    let env = mn_core::config::StdEnv;
    let auth_path = mn_core::paths::auth_file_path(&env)
        .ok_or_else(|| anyhow!("could not resolve auth.toml path (set XDG_CONFIG_HOME or HOME)"))?;
    run_with_paths(args, &server_url, &auth_path, telemetry, cli_version, json).await
}

/// Path-explicit driver, exposed for integration tests.
///
/// # Errors
///
/// Returns the same errors as [`run`].
pub async fn run_with_paths(
    args: Args,
    server_url: &str,
    auth_path: &Path,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    let started = Instant::now();
    let outcome = run_inner(&args, server_url, auth_path, json).await;

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

    outcome.map(|_| ())
}

struct RunStats {
    added: usize,
    carried: usize,
    deleted: usize,
    batch_count: u32,
    failed_batch_index: Option<u32>,
}

#[allow(clippy::too_many_lines)]
async fn run_inner(
    args: &Args,
    server_url: &str,
    auth_path: &Path,
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
            package: detect_package_ref(&source_root, &doc.rel_path),
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

    // Load the local embedder up front (unless the server will embed). Doing
    // this before we create a server-side run means a missing model fails
    // fast without leaving an orphaned `building` source_version.
    let embedder = if args.enable_server_embedding {
        None
    } else {
        validate_local_embedding_model(&args.embedding_model)?;
        reporter.phase("load_embedder", serde_json::json!({"model": args.embedding_model}));
        let emb = load_local_embedder().await?;
        reporter.phase_done("load_embedder", serde_json::json!({"model": args.embedding_model}));
        Some(emb)
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
            embedding_model: args.embedding_model.clone(),
            note: args.note.clone(),
        },
    )
    .await
    .map_err(|e| translate_start_error(e, &args.embedding_model))
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

    for (i, batch) in docs.chunks(batch_size).enumerate() {
        reporter.batch(i + 1, batch_count, "uploading documents");
        let mut batch_docs = batch.to_vec();
        if let Some(emb) = &embedder {
            if let Err(e) = embed_batch(emb, &mut batch_docs).await {
                abort_run(&client, server_url, &args.source_slug, start.ingest_run_id, &token)
                    .await;
                return Err(e.context(format!("embed batch {}/{batch_count}", i + 1)));
            }
        }
        let body = UploadDocumentsRequest {
            documents: batch_docs,
            batch_index: i,
            batch_count,
            embedding_model: embedder.as_ref().map(|_| args.embedding_model.clone()),
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
fn translate_upload_error(e: anyhow::Error, batch: usize, of: usize, run_id: Uuid) -> anyhow::Error {
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
        for c in d.chunks.iter_mut() {
            c.embedding = it.next();
        }
    }
    Ok(())
}

/// Local embedding can only produce `bge-base-en-v1.5` vectors. Reject any
/// other `--embedding-model` with a pointer to `--enable-server-embedding`.
///
/// # Errors
///
/// Errors if the wire id fails to parse or names a different model.
fn validate_local_embedding_model(model_wire: &str) -> Result<()> {
    use std::str::FromStr as _;
    let id = mn_core::model_id::EmbeddingModelId::from_str(model_wire)
        .map_err(|e| anyhow!("invalid --embedding-model `{model_wire}`: {e}"))?;
    if id.name != mn_embedding::embedder::MODEL_NAME {
        return Err(anyhow!(
            "local embedding only supports `{}@…`; got `{model_wire}`. \
             Pass --enable-server-embedding to ingest with a server-side model.",
            mn_embedding::embedder::MODEL_NAME
        ));
    }
    Ok(())
}

/// Load the process-wide local embedder, mapping failures to actionable advice.
///
/// # Errors
///
/// Errors if the model cache dir cannot be resolved or the model fails to load.
async fn load_local_embedder() -> Result<mn_embedding::Embedder> {
    let env = mn_embedding::cache::StdEnv;
    let cache_dir = mn_embedding::cache::resolve(&env)
        .context("could not resolve model cache dir (set MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME)")?;
    mn_embedding::embedder::global(cache_dir).await.map_err(|e| {
        anyhow!("could not load local embedder ({e}). Run `mnm models pull`, or pass --enable-server-embedding.")
    })
}

/// Embed every chunk of `docs` in place using the local embedder.
///
/// # Errors
///
/// Errors if the embedder call fails or returns the wrong vector count.
async fn embed_batch(emb: &mn_embedding::Embedder, docs: &mut [DocumentUpload]) -> Result<()> {
    let texts: Vec<String> = docs
        .iter()
        .flat_map(|d| d.chunks.iter().map(|c| c.content.clone()))
        .collect();
    if texts.is_empty() {
        return Ok(());
    }
    let vectors = emb.embed_blocking(texts, None).await.context("local embedding")?;
    attach_embeddings(docs, vectors)
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

/// Detect package membership for a single file by walking up to the source root
/// and looking for the nearest `Cargo.toml` or `package.json` manifest.
///
/// Returns `None` for files that are not enclosed by any known manifest.
fn detect_package_ref(
    source_root: &std::path::Path,
    rel_path: &std::path::Path,
) -> Option<mn_core::types::PackageRef> {
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
                path: "a".into(), kind: DocumentKind::Markdown, content_hash: "h".into(),
                source_url: None, published_url: None, language: None, source_modified_at: None,
                frontmatter: None, provenance: Provenance::default(), char_count: 0, token_count: 0,
                package: None,
                chunks: vec![ mk_chunk(0), mk_chunk(1) ],
            },
            DocumentUpload {
                path: "b".into(), kind: DocumentKind::Markdown, content_hash: "h".into(),
                source_url: None, published_url: None, language: None, source_modified_at: None,
                frontmatter: None, provenance: Provenance::default(), char_count: 0, token_count: 0,
                package: None,
                chunks: vec![ mk_chunk(0) ],
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
            path: "a".into(), kind: DocumentKind::Markdown, content_hash: "h".into(),
            source_url: None, published_url: None, language: None, source_modified_at: None,
            frontmatter: None, provenance: Provenance::default(), char_count: 0, token_count: 0,
            package: None, chunks: vec![mk_chunk(0)],
        }];
        assert!(attach_embeddings(&mut docs, vec![]).is_err());
    }

    #[test]
    fn local_model_must_be_bge_base() {
        assert!(validate_local_embedding_model("bge-base-en-v1.5@1").is_ok());
        let err = validate_local_embedding_model("some-other-model@1").unwrap_err();
        assert!(err.to_string().contains("--enable-server-embedding"), "{err}");
    }

    fn mk_chunk(idx: i32) -> ChunkUpload {
        ChunkUpload {
            chunk_index: idx, total_chunks: 2, content: format!("c{idx}"), content_hash: "c".into(),
            heading_path: vec![], symbol_path: vec![], start_byte: 0, end_byte: 0, token_count: 0,
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
    fn default_batch_size_is_25_and_server_embedding_defaults_off() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            inner: Args,
        }
        let w = Wrap::try_parse_from(["ingest-run", "--source-slug", "s", "m.yaml"]).unwrap();
        assert_eq!(w.inner.batch_size, 25);
        assert!(!w.inner.enable_server_embedding);
    }

    #[test]
    fn enable_server_embedding_flag_parses() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            inner: Args,
        }
        let w = Wrap::try_parse_from(
            ["ingest-run", "--source-slug", "s", "--enable-server-embedding", "m.yaml"],
        )
        .unwrap();
        assert!(w.inner.enable_server_embedding);
    }

    #[test]
    fn chunk_upload_skips_embedding_when_none() {
        let c = ChunkUpload {
            chunk_index: 0, total_chunks: 1, content: "x".into(), content_hash: "c".into(),
            heading_path: vec![], symbol_path: vec![], start_byte: 0, end_byte: 1, token_count: 0,
            embedding: None,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("embedding"), "None embedding must be omitted: {s}");
    }
}

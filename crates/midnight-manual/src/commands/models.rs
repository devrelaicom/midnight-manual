//! `mnm models <subcommand>` — local model management + corpus-side
//! introspection.
//!
//! - `mnm models pull` ensures the local model-cache directory exists. Both the
//!   embedder and the reranker are remote VoyageAI (BYOK or the cloud server's
//!   `/v1/embeddings` and inline rerank), so there are no model weights to
//!   download; the subcommand is kept as a no-op-friendly cache primer.
//! - `mnm models active` GETs `/v1/models/active` so callers can verify
//!   that the corpus's active model matches what their queries embed with.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mnm_telemetry::events::{Component, EventPayload, Outcome};
use mnm_telemetry::{Event, TelemetryClient};
use serde::{Deserialize, Serialize};

/// `mnm models <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: ModelsCmd,
}

/// `models` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum ModelsCmd {
    /// Ensure the local model-cache directory exists. Both the embedder and the
    /// reranker are remote VoyageAI, so nothing is downloaded.
    Pull(PullArgs),
    /// Show the corpus's currently active embedding model.
    Active(ActiveArgs),
    /// List sources that are still on an older embedding model (i.e. have not
    /// yet been re-ingested against the corpus's current active model).
    #[command(hide = true)]
    Status(StatusArgs),
    /// Re-ingest every source not yet on the target embedding model (admin).
    #[command(hide = true)]
    Migrate(MigrateArgs),
}

/// Args for `mnm models pull`.
#[derive(Debug, ClapArgs)]
pub struct PullArgs {
    /// Override the local model cache directory. Defaults to
    /// `$MIDNIGHT_MANUAL_MODEL_CACHE_DIR` > `$XDG_DATA_HOME/midnight-manual/models`
    /// > `$HOME/.local/share/midnight-manual/models`.
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
}

/// Args for `mnm models active`.
#[derive(Debug, ClapArgs)]
pub struct ActiveArgs;

/// Args for `mnm models status`.
///
/// No positional arguments — the active model wire id is fetched automatically
/// from `GET /v1/models/active`.
#[derive(Debug, ClapArgs)]
pub struct StatusArgs;

/// Args for `mnm models migrate`.
#[derive(Debug, ClapArgs)]
pub struct MigrateArgs {
    /// Target model wire id, e.g. "voyage-code-4@1". Defaults to the active model.
    #[arg(long)]
    pub to: Option<String>,
    /// Comma-separated source names to restrict the run.
    #[arg(long, value_delimiter = ',')]
    pub source: Vec<String>,
    /// Stop after this many documents (evaluated at source boundaries).
    #[arg(long)]
    pub max_docs: Option<u64>,
    /// Client-side session token budget (sums Voyage usage across server + BYOK).
    #[arg(long)]
    pub token_budget: Option<u64>,
    /// Manifest directory (defaults to manifests/midnight).
    #[arg(long, default_value = "manifests/midnight")]
    pub manifests_dir: std::path::PathBuf,
}

/// Dispatch.
///
/// `config_path` and `voyage_api_key` are only consumed by the `migrate` path
/// (the ingest pipeline needs them to choose between BYOK Voyage and the
/// server-proxy embed route); `pull` / `active` / `status` ignore them.
///
/// # Errors
///
/// Returns `anyhow::Error` when the cache dir cannot be resolved, the model
/// loader fails, or the cloud round-trip fails for the `active` path.
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
    match args.cmd {
        ModelsCmd::Pull(p) => run_pull(p, config_path, telemetry, cli_version, json).await,
        ModelsCmd::Active(_) => run_active(server_flag, json).await,
        ModelsCmd::Status(_) => run_status(server_flag, json).await,
        ModelsCmd::Migrate(m) => {
            run_migrate(m, server_flag, config_path, voyage_api_key, telemetry, cli_version, json)
                .await
        }
    }
}

async fn run_pull(
    args: PullArgs,
    config_path: Option<&Path>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    let started = Instant::now();
    // Config supplies the `[models].cache_dir` middle layer (flag > config > env).
    let cfg_env = mnm_core::config::StdEnv;
    let (cfg, _) = mnm_core::config::Config::discover(config_path, &cfg_env).unwrap_or_default();
    let cache_dir = resolve_cache_dir(args.cache_dir, cfg.models.cache_dir.as_deref())?;
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create model cache dir at {}", cache_dir.display()))?;

    // Both the embedder and the reranker are remote VoyageAI now, so there is
    // nothing to download — `pull` only primes the cache directory.
    let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    telemetry
        .emit(Event::new(
            Component::Cli,
            cli_version,
            EventPayload::PullModels {
                // Nothing is fetched locally: both models are remote VoyageAI.
                embedder_downloaded: false,
                reranker_downloaded: false,
                duration_ms,
                outcome: Outcome::Ok,
            },
        ))
        .await;

    println!("{}", format_pull_output(duration_ms, &cache_dir, json));
    Ok(())
}

async fn run_active(server_flag: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server_flag);
    let parsed = fetch_active(&server_url).await?;
    println!("{}", format_active_output(&parsed, json));
    Ok(())
}

async fn run_status(server_flag: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server_flag);
    // Determine the active wire id from the corpus.
    let active = fetch_active(&server_url).await?;
    let wire = format!("{}@{}", active.name, active.revision);
    // Require an admin token — this endpoint is admin-gated.
    let bearer = crate::commands::ratelimits::require_admin_token_from(&mnm_core::config::StdEnv)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let value = status_request(&client, &server_url, &wire, &bearer).await?;
    let sources = value["sources"]
        .as_array()
        .ok_or_else(|| anyhow!("unexpected response shape: missing `sources` array"))?;
    println!("{}", format_status_output(sources, &wire, json));
    Ok(())
}

// ── models migrate ───────────────────────────────────────────────────────────

/// One source the server reports as not-on-target. `origin_url` is the git URL
/// to clone for re-ingest; `None` means the source can't be cloned (it is
/// skipped before the budget loop ever sees it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRef {
    /// Source slug — equals the manifest `root.name` and the server source slug.
    pub slug: String,
    /// Git URL to clone for re-ingest, if the server supplied one.
    pub origin_url: Option<String>,
}

/// What one source's re-ingest produced, used to advance the session budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceOutcome {
    /// Documents added by this source's ingest.
    pub docs: u64,
    /// VoyageAI tokens consumed by this source's ingest.
    pub tokens: u64,
    /// Per-document conflicts the server reported for this source's ingest
    /// (documents NOT inserted). Surfaced so the migrate path's machine output
    /// is as observable as the single-source `ingest` path.
    pub conflicts: u64,
}

/// Outcome of a whole migration run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrateSummary {
    /// Slugs migrated (in order).
    pub migrated: Vec<String>,
    /// Total documents added across migrated sources.
    pub docs: u64,
    /// Total VoyageAI tokens consumed across migrated sources.
    pub tokens: u64,
    /// Total per-document conflicts (documents NOT inserted) across migrated
    /// sources. The per-source warn-logs already fire in the ingest pipeline;
    /// this surfaces the aggregate in the migrate `--json` output.
    pub conflicts: u64,
    /// Slugs not attempted — either budget/max-docs stopped before them, or a
    /// mid-source error halted the run (the failed source plus its untried
    /// tail). Empty when the whole list completed.
    pub remaining: Vec<String>,
}

/// Drive the migration loop over `sources`, calling `ingest_one` for each, with
/// budget checks at **source boundaries**.
///
/// Boundary semantics (overshoot allowed *within* a source):
///
/// - Before starting a source, if `token_budget` is set and `tokens >= budget`,
///   or `max_docs` is set and `docs >= max`, STOP without starting it. The
///   source that crosses the budget still completes (we cannot know its size
///   before ingesting); the *next* source is the one that is skipped.
/// - On `Ok(outcome)`: accrue `docs`/`tokens`, record the slug as migrated.
/// - On `Err`: this is the mid-source limit/429 case. The ingest pipeline has
///   already aborted (not finalized) the in-flight source, so we just log, push
///   the failed source plus every untried source after it into `remaining`, and
///   STOP.
///
/// The ingest is injected so this loop is unit-testable without git or HTTP.
/// `ingest_one` is an `AsyncFnMut` (stable since Rust 1.85) so its returned
/// future may borrow the `&SourceRef` argument — the `FnMut(&SourceRef) -> Fut`
/// shape cannot express that higher-ranked lifetime.
pub async fn drive_migration<F>(
    sources: &[SourceRef],
    max_docs: Option<u64>,
    token_budget: Option<u64>,
    mut ingest_one: F,
) -> MigrateSummary
where
    F: AsyncFnMut(&SourceRef) -> Result<SourceOutcome>,
{
    let mut summary = MigrateSummary::default();
    let total = sources.len();

    for (idx, src) in sources.iter().enumerate() {
        // Boundary budget check — evaluated *before* starting this source.
        if token_budget.is_some_and(|b| summary.tokens >= b)
            || max_docs.is_some_and(|m| summary.docs >= m)
        {
            summary
                .remaining
                .extend(sources[idx..].iter().map(|s| s.slug.clone()));
            return summary;
        }

        // Per-source liveness line to STDERR (multi-repo admin op). Unit tests
        // stub `ingest_one` and do not assert on stderr, so this is safe.
        eprintln!("[{}/{}] migrating {}", idx + 1, total, src.slug);

        match ingest_one(src).await {
            Ok(outcome) => {
                summary.docs = summary.docs.saturating_add(outcome.docs);
                summary.tokens = summary.tokens.saturating_add(outcome.tokens);
                summary.conflicts = summary.conflicts.saturating_add(outcome.conflicts);
                summary.migrated.push(src.slug.clone());
            }
            Err(e) => {
                // The pipeline already aborted (not promoted) the in-flight
                // source on a limit/429; we only stop and record what's left.
                tracing::warn!(
                    slug = %src.slug,
                    error = %format!("{e:#}"),
                    "models migrate: source ingest failed; aborting run (in-flight source was not finalized)"
                );
                summary
                    .remaining
                    .extend(sources[idx..].iter().map(|s| s.slug.clone()));
                return summary;
            }
        }
    }

    summary
}

/// `mnm models migrate` driver.
///
/// Resolves the target wire id (`--to`, else the active model), fetches the
/// provenance-ordered list of sources not yet on that target, filters by
/// `--source`, skips sources that cannot be re-ingested (no `origin_url`, no
/// manifest), then re-ingests the rest in order — cloning each `origin_url` and
/// running the Phase 6.3 ingest pipeline onto the target model — stopping at
/// source boundaries once `--max-docs` / `--token-budget` is reached.
///
/// # Errors
///
/// Returns `anyhow::Error` when the active model / source list cannot be
/// fetched, the admin bearer is missing, or a git clone / ingest fails
/// mid-source (which also aborts the in-flight source via the pipeline).
#[allow(clippy::too_many_arguments)]
pub async fn run_migrate(
    args: MigrateArgs,
    server_flag: Option<&str>,
    config_path: Option<&Path>,
    voyage_api_key: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server_flag);

    // Resolve the TARGET wire id: explicit --to, else the corpus active model.
    let target_wire = if let Some(to) = args.to.clone() {
        to
    } else {
        let active = fetch_active(&server_url).await?;
        format!("{}@{}", active.name, active.revision)
    };

    // Admin bearer (the not-on-target source list is admin-gated).
    let bearer = crate::commands::ratelimits::require_admin_token_from(&mnm_core::config::StdEnv)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;

    // Provenance-ordered list of sources not yet on the target.
    let value = status_request(&client, &server_url, &target_wire, &bearer).await?;
    let sources = parse_and_filter_sources(&value, &args.source)?;

    // Skip handling lives OUTSIDE the budget loop: a missing origin_url or a
    // missing manifest is a per-source SKIP, not a hard stop. We pre-filter so
    // the budget loop only ever sees ingestable sources.
    let mut ingestable: Vec<SourceRef> = Vec::with_capacity(sources.len());
    for s in sources {
        if s.origin_url.is_none() {
            tracing::warn!(slug = %s.slug, "models migrate: skipping — no origin_url to clone");
            continue;
        }
        let manifest = args.manifests_dir.join(format!("{}.yaml", s.slug));
        if !manifest.exists() {
            tracing::warn!(
                slug = %s.slug,
                manifest = %manifest.display(),
                "models migrate: skipping — no manifest"
            );
            continue;
        }
        ingestable.push(s);
    }

    // Resolve the auth path once for the per-source ingest calls.
    let auth_path = mnm_core::paths::auth_file_path(&mnm_core::config::StdEnv)
        .ok_or_else(|| anyhow!("could not resolve auth.toml path (set XDG_CONFIG_HOME or HOME)"))?;

    // Pre-flight banner to STDERR (visible even under `--json`). `mnm` defaults
    // to the PROD server and this op clones every not-on-target repo and spends
    // Voyage tokens, so surface the blast radius before the loop starts.
    eprintln!("migrating {} source(s) onto {target_wire} via {server_url}", ingestable.len());
    eprintln!("note: this clones each source repository and spends Voyage embedding tokens");

    // Drive the loop with the REAL per-source ingest closure. The `async`
    // closure confines the borrow of `src` to the future, which the
    // `AsyncFnMut` bound on `drive_migration` requires.
    let summary =
        drive_migration(&ingestable, args.max_docs, args.token_budget, async |src: &SourceRef| {
            ingest_source(
                src,
                &target_wire,
                &args.manifests_dir,
                &server_url,
                &auth_path,
                config_path,
                voyage_api_key,
                telemetry,
                cli_version,
                json,
            )
            .await
        })
        .await;

    println!("{}", format_migrate_output(&summary, &target_wire, json));
    Ok(())
}

/// Parse the server's `{"sources":[{"slug","origin_url"}...]}` envelope into an
/// ordered `Vec<SourceRef>` (preserving server = provenance order) and apply the
/// optional `--source` filter. Slugs requested via `--source` that aren't in the
/// not-on-target list are warned about and ignored.
///
/// # Errors
///
/// Returns `anyhow::Error` if the `sources` array is missing.
fn parse_and_filter_sources(
    value: &serde_json::Value,
    source_filter: &[String],
) -> Result<Vec<SourceRef>> {
    let raw = value["sources"]
        .as_array()
        .ok_or_else(|| anyhow!("unexpected response shape: missing `sources` array"))?;
    let mut sources: Vec<SourceRef> = raw
        .iter()
        .filter_map(|s| {
            s["slug"].as_str().map(|slug| SourceRef {
                slug: slug.to_owned(),
                origin_url: s["origin_url"].as_str().map(str::to_owned),
            })
        })
        .collect();

    if !source_filter.is_empty() {
        let wanted: BTreeSet<&str> = source_filter.iter().map(String::as_str).collect();
        let present: BTreeSet<&str> = sources.iter().map(|s| s.slug.as_str()).collect();
        for name in &wanted {
            if !present.contains(name) {
                tracing::warn!(source = %name, "models migrate: --source not in the not-on-target list; ignoring");
            }
        }
        sources.retain(|s| wanted.contains(s.slug.as_str()));
    }
    Ok(sources)
}

/// Re-ingest one source onto `target_wire`: clone `origin_url` into a tempdir
/// (auto-cleaned on drop), run the Phase 6.3 ingest pipeline against the cloned
/// tree, and map the resulting [`RunStats`] into a [`SourceOutcome`].
///
/// A clone or ingest failure returns `Err`, which stops the migration run —
/// for ingest failures the pipeline has already aborted (not finalized) the
/// in-flight source.
#[allow(clippy::too_many_arguments)]
async fn ingest_source(
    src: &SourceRef,
    target_wire: &str,
    manifests_dir: &Path,
    server_url: &str,
    auth_path: &Path,
    config_path: Option<&Path>,
    voyage_api_key: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<SourceOutcome> {
    // origin_url is guaranteed Some here (run_migrate pre-filters None away), but
    // handle it defensively rather than unwrap.
    let origin_url = src
        .origin_url
        .as_deref()
        .ok_or_else(|| anyhow!("source {} has no origin_url", src.slug))?;

    let tmp = tempfile::tempdir().context("create temp clone dir")?;
    let clone_dir = tmp.path().join(&src.slug);

    // Pass `origin_url` and the clone dir as discrete args (no shell string —
    // no injection surface). Capture output so git's diagnostic survives in
    // `--json`/piped contexts instead of vanishing onto the inherited terminal.
    let clone_out = std::process::Command::new("git")
        .args(["clone", "--depth", "1", origin_url])
        .arg(&clone_dir)
        .output()
        .with_context(|| format!("spawn `git clone {origin_url}`"))?;
    if !clone_out.status.success() {
        let stderr = String::from_utf8_lossy(&clone_out.stderr);
        return Err(anyhow!(
            "git clone of {origin_url} failed for source {}: {}",
            src.slug,
            stderr.trim()
        ));
    }

    let manifest = manifests_dir.join(format!("{}.yaml", src.slug));
    let ingest_args = crate::commands::ingest::run::Args {
        manifest,
        source_slug: src.slug.clone(),
        // None → the pipeline infers the revision via `git rev-parse` in the clone.
        revision: None,
        // Pin the TARGET wire id (NOT "auto") so the run records the new model.
        embedding_model: target_wire.to_owned(),
        note: Some(format!("models migrate → {target_wire}")),
        source_root: Some(clone_dir.clone()),
        dry_run: false,
        yes: true,
        source_base_url: None,
        batch_size: 25,
        // None → resolver falls back to VOYAGE_TIMEOUT_SECS env / config / default.
        voyage_timeout_secs: None,
        chunk_tokens: 1024,
        include: Vec::new(),
        exclude: Vec::new(),
        no_respect_gitignore: false,
        disable_default_ignore_list: false,
        max_file_size: 10 * 1024 * 1024,
        // Migrate does NOT expose the global-cap opt-out.
        unsafe_no_global_limit: false,
        // Follow the manifest's `code_embeddings` option (default on); migrate
        // does not expose the per-run opt-out flag.
        no_code_embeddings: false,
        // Migrate does not write a structured report file.
        report_file: None,
    };

    let stats = crate::commands::ingest::run::run_with_paths_stats(
        ingest_args,
        server_url,
        auth_path,
        config_path,
        voyage_api_key,
        telemetry,
        cli_version,
        json,
    )
    .await?;

    Ok(SourceOutcome {
        docs: stats.added as u64,
        tokens: stats.total_tokens,
        conflicts: stats.conflicts.len() as u64,
    })
}

#[derive(Debug, Serialize)]
struct MigrateOutput<'a> {
    action: &'a str,
    target_model: &'a str,
    migrated: &'a [String],
    documents: u64,
    spent_tokens: u64,
    /// Aggregate per-document conflicts (documents NOT inserted) across migrated
    /// sources. Always present so machine consumers can detect partial-failure
    /// migrations — parity with the single-source `ingest` path's `conflict_count`.
    conflicts: u64,
    remaining: &'a [String],
}

fn format_migrate_output(summary: &MigrateSummary, target_wire: &str, json: bool) -> String {
    if json {
        let body = MigrateOutput {
            action: "models.migrate",
            target_model: target_wire,
            migrated: &summary.migrated,
            documents: summary.docs,
            spent_tokens: summary.tokens,
            conflicts: summary.conflicts,
            remaining: &summary.remaining,
        };
        return serde_json::to_string(&body).unwrap_or_default();
    }
    let mut out = String::new();
    let conflict_clause = if summary.conflicts > 0 {
        format!(" / {} conflicts", summary.conflicts)
    } else {
        String::new()
    };
    writeln!(
        out,
        "migrated {} source(s) / {} docs / {} tokens{conflict_clause} onto {target_wire}",
        summary.migrated.len(),
        summary.docs,
        summary.tokens,
    )
    .ok();
    if summary.migrated.is_empty() {
        writeln!(out, "  (no sources migrated)").ok();
    } else {
        for slug in &summary.migrated {
            writeln!(out, "  + {slug}").ok();
        }
    }
    if summary.remaining.is_empty() {
        write!(out, "all targeted sources are on {target_wire}").ok();
    } else {
        writeln!(out, "remaining (not attempted):").ok();
        for slug in &summary.remaining {
            writeln!(out, "  - {slug}").ok();
        }
    }
    out.trim_end_matches('\n').to_owned()
}

/// `GET /v1/admin/sources?not_model=<wire>` — returns the server's
/// `{"sources":[...]}` envelope.  Exposed `pub` for integration tests.
///
/// # Errors
///
/// Returns `anyhow::Error` on transport failure or non-2xx responses.
pub async fn status_request(
    client: &reqwest::Client,
    server_url: &str,
    wire: &str,
    bearer: &str,
) -> Result<serde_json::Value> {
    let url = format!("{server_url}/v1/admin/sources?not_model={wire}");
    let resp = client
        .get(&url)
        .bearer_auth(bearer)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    crate::commands::ratelimits::decode_response(resp, "sources not on model").await
}

fn format_status_output(sources: &[serde_json::Value], wire: &str, json: bool) -> String {
    if json {
        let body = serde_json::json!({
            "action": "models.status",
            "active_model": wire,
            "sources_pending_reingest": sources,
        });
        return serde_json::to_string(&body).unwrap_or_default();
    }
    if sources.is_empty() {
        return format!("all sources are on {wire}");
    }
    let mut out = String::new();
    writeln!(out, "sources not yet on {wire}:").ok();
    for s in sources {
        let slug = s["slug"].as_str().unwrap_or("?");
        let url = s["origin_url"].as_str().unwrap_or("(no url)");
        writeln!(out, "  {slug:<30}  {url}").ok();
    }
    // Trim the trailing newline so println! adds exactly one.
    out.trim_end_matches('\n').to_owned()
}

/// GET `/v1/models/active` and decode the response. Exposed for integration
/// tests against a wiremock server.
///
/// # Errors
///
/// Returns `anyhow::Error` if the request fails, the response is non-2xx,
/// or the body can't be decoded as [`ActiveModelResponse`].
pub async fn fetch_active(server_url: &str) -> Result<ActiveModelResponse> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let resp = client
        .get(format!("{server_url}/v1/models/active"))
        .send()
        .await
        .context("GET /v1/models/active")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("{status} from /v1/models/active: {body}"));
    }
    resp.json::<ActiveModelResponse>()
        .await
        .context("parse /v1/models/active response")
}

/// Resolve the model cache dir for `mnm models pull` with precedence
/// **flag (`--cache-dir`) > config (`[models].cache_dir`) > env-chain**
/// (`MIDNIGHT_MANUAL_MODEL_CACHE_DIR` > `XDG_DATA_HOME` > `HOME`).
fn resolve_cache_dir(flag: Option<PathBuf>, cfg_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = flag {
        return Ok(p);
    }
    let env = mnm_embedding::cache::StdEnv;
    mnm_embedding::cache::resolve_with_override(cfg_dir, &env).context(
        "could not resolve model cache dir; set [models].cache_dir, \
         MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME",
    )
}

/// Active-model response — mirrors the server's typed shape.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActiveModelResponse {
    /// Canonical model name (e.g. `bge-base-en-v1.5`).
    pub name: String,
    /// Monotonic revision; combined with `name` forms the wire id.
    pub revision: i32,
    /// Embedding dimensionality.
    pub dim: i32,
    /// Provider tag (e.g. `baai`).
    pub provider: String,
    /// Output dtype the corpus is encoded with (e.g. `"float"`). The client
    /// derives its embedder's `output_dtype` from this so the model that
    /// COMPUTES a query vector matches the one whose wire id LABELS it. Defaults
    /// to `"float"` for servers that predate the field.
    #[serde(default = "default_active_dtype")]
    pub dtype: String,
    /// The corpus's code-embedding model (dual embeddings). `None` when the
    /// server has no resolved code model (or predates dual embeddings) —
    /// code search is then unavailable server-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ActiveCode>,
}

/// Default dtype for an active-model response that omits the field (a server
/// that predates the dtype field). The corpus dtype is `"float"`.
fn default_active_dtype() -> String {
    "float".to_owned()
}

/// The code-embedding half of [`ActiveModelResponse`]. `name@revision` forms
/// the wire id clients pin code vectors against.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActiveCode {
    /// Canonical model name (e.g. `voyage-code-3`).
    pub name: String,
    /// Monotonic revision; combined with `name` forms the wire id.
    pub revision: i32,
    /// Embedding dimensionality.
    #[serde(default)]
    pub dim: i32,
    /// Provider tag (e.g. `voyageai`).
    #[serde(default)]
    pub provider: String,
    /// Output dtype the code column is encoded with. Defaults to `"float"`.
    #[serde(default = "default_active_dtype")]
    pub dtype: String,
}

#[derive(Debug, Serialize)]
struct PullOutput<'a> {
    action: &'a str,
    duration_ms: u32,
    cache_dir: String,
}

fn format_pull_output(duration_ms: u32, cache_dir: &Path, json: bool) -> String {
    if json {
        let body = PullOutput {
            action: "models.pull",
            duration_ms,
            cache_dir: cache_dir.display().to_string(),
        };
        return serde_json::to_string(&body).unwrap_or_default();
    }
    let mut out = String::new();
    writeln!(out, "model cache primed in {duration_ms} ms:").ok();
    writeln!(out, "  reranker: VoyageAI (remote — nothing to download)").ok();
    writeln!(out, "  embedder: VoyageAI (remote — nothing to download)").ok();
    write!(out, "  cache:    {}", cache_dir.display()).ok();
    out
}

#[derive(Debug, Serialize)]
struct ActiveOutput<'a> {
    action: &'a str,
    name: &'a str,
    revision: i32,
    dim: i32,
    provider: &'a str,
    wire_id: String,
}

fn format_active_output(model: &ActiveModelResponse, json: bool) -> String {
    let wire_id = format!("{}@{}", model.name, model.revision);
    if json {
        let body = ActiveOutput {
            action: "models.active",
            name: &model.name,
            revision: model.revision,
            dim: model.dim,
            provider: &model.provider,
            wire_id,
        };
        return serde_json::to_string(&body).unwrap_or_default();
    }
    let mut out = String::new();
    writeln!(out, "corpus active embedding model:").ok();
    writeln!(out, "  wire id:   {wire_id}").ok();
    writeln!(out, "  name:      {}", model.name).ok();
    writeln!(out, "  revision:  {}", model.revision).ok();
    writeln!(out, "  dim:       {}", model.dim).ok();
    write!(out, "  provider:  {}", model.provider).ok();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_active() -> ActiveModelResponse {
        ActiveModelResponse {
            name: "bge-base-en-v1.5".to_owned(),
            revision: 1,
            dim: 768,
            provider: "baai".to_owned(),
            dtype: "float".to_owned(),
            code: None,
        }
    }

    #[test]
    fn active_response_deserializes_without_code() {
        // Pre-dual-embeddings servers omit `code` entirely.
        let v: ActiveModelResponse = serde_json::from_value(serde_json::json!({
            "name": "voyage-context-3", "revision": 1, "dim": 1024, "provider": "voyageai"
        }))
        .unwrap();
        assert!(v.code.is_none());
    }

    #[test]
    fn active_response_deserializes_with_code() {
        let v: ActiveModelResponse = serde_json::from_value(serde_json::json!({
            "name": "voyage-context-3", "revision": 1, "dim": 1024, "provider": "voyageai",
            "code": { "name": "voyage-code-3", "revision": 1, "dim": 1024, "provider": "voyageai" }
        }))
        .unwrap();
        let code = v.code.expect("code model present");
        assert_eq!(code.name, "voyage-code-3");
        assert_eq!(code.revision, 1);
        assert_eq!(code.dim, 1024);
        assert_eq!(code.provider, "voyageai");
    }

    #[test]
    fn pull_human_output_describes_each_model() {
        let s = format_pull_output(1234, Path::new("/tmp/cache"), false);
        assert!(s.contains("1234 ms"));
        // Both models are remote VoyageAI now — nothing is downloaded.
        assert!(s.contains("reranker: VoyageAI"));
        assert!(s.contains("embedder: VoyageAI"));
        assert!(s.contains("/tmp/cache"));
    }

    #[test]
    fn pull_json_output_is_stable() {
        let s = format_pull_output(42, Path::new("/tmp/c"), true);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["action"], "models.pull");
        assert_eq!(v["duration_ms"], 42);
        assert_eq!(v["cache_dir"], "/tmp/c");
        // No model-download fields anymore — both models are remote VoyageAI.
        assert!(v.get("reranker").is_none());
        assert!(v.get("reranker_downloaded").is_none());
        assert!(v.get("embedder").is_none());
        assert!(v.get("embedder_downloaded").is_none());
    }

    #[test]
    fn active_human_output_contains_wire_id() {
        let s = format_active_output(&sample_active(), false);
        assert!(s.contains("bge-base-en-v1.5@1"));
        assert!(s.contains("768"));
        assert!(s.contains("baai"));
    }

    #[test]
    fn active_json_output_is_stable() {
        let s = format_active_output(&sample_active(), true);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["action"], "models.active");
        assert_eq!(v["name"], "bge-base-en-v1.5");
        assert_eq!(v["revision"], 1);
        assert_eq!(v["wire_id"], "bge-base-en-v1.5@1");
    }

    #[test]
    fn resolve_cache_dir_prefers_flag() {
        let p = PathBuf::from("/some/explicit/cache");
        // Flag wins even when a config dir is also present.
        let resolved = resolve_cache_dir(Some(p.clone()), Some(Path::new("/from/config"))).unwrap();
        assert_eq!(resolved, p);
    }

    #[test]
    fn resolve_cache_dir_prefers_config_over_env() {
        // No flag → the config `[models].cache_dir` wins over the env-chain
        // fallback (verified inside mnm_embedding::cache::resolve_with_override).
        let cfg_dir = PathBuf::from("/from/config/cache");
        let resolved = resolve_cache_dir(None, Some(cfg_dir.as_path())).unwrap();
        assert_eq!(resolved, cfg_dir);
    }

    fn sources_envelope() -> serde_json::Value {
        serde_json::json!({
            "sources": [
                { "slug": "midnight-docs", "origin_url": "https://github.com/m/docs.git" },
                { "slug": "compact-lang",  "origin_url": null },
                { "slug": "ledger",        "origin_url": "https://github.com/m/ledger.git" }
            ]
        })
    }

    #[test]
    fn parse_sources_preserves_order_and_origin_urls() {
        let parsed = parse_and_filter_sources(&sources_envelope(), &[]).unwrap();
        assert_eq!(
            parsed,
            vec![
                SourceRef {
                    slug: "midnight-docs".to_owned(),
                    origin_url: Some("https://github.com/m/docs.git".to_owned()),
                },
                SourceRef {
                    slug: "compact-lang".to_owned(),
                    origin_url: None,
                },
                SourceRef {
                    slug: "ledger".to_owned(),
                    origin_url: Some("https://github.com/m/ledger.git".to_owned()),
                },
            ]
        );
    }

    #[test]
    fn parse_sources_applies_source_filter_in_provenance_order() {
        // Filter to two slugs; order follows the server list (provenance), not
        // the order they were supplied on the command line.
        let filter = vec!["ledger".to_owned(), "midnight-docs".to_owned()];
        let parsed = parse_and_filter_sources(&sources_envelope(), &filter).unwrap();
        let slugs: Vec<&str> = parsed.iter().map(|s| s.slug.as_str()).collect();
        assert_eq!(slugs, vec!["midnight-docs", "ledger"]);
    }

    #[test]
    fn parse_sources_unknown_filter_name_is_ignored() {
        let filter = vec!["does-not-exist".to_owned()];
        let parsed = parse_and_filter_sources(&sources_envelope(), &filter).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_sources_missing_array_errors() {
        let bad = serde_json::json!({ "not_sources": [] });
        assert!(parse_and_filter_sources(&bad, &[]).is_err());
    }

    #[test]
    fn migrate_output_human_lists_migrated_and_remaining() {
        let summary = MigrateSummary {
            migrated: vec!["src-1".to_owned()],
            docs: 10,
            tokens: 100,
            conflicts: 0,
            remaining: vec!["src-2".to_owned()],
        };
        let s = format_migrate_output(&summary, "voyage-code-4@1", false);
        assert!(s.contains("voyage-code-4@1"));
        assert!(s.contains("10 docs"));
        assert!(s.contains("100 tokens"));
        assert!(s.contains("+ src-1"));
        assert!(s.contains("- src-2"));
        assert!(!s.contains("conflict"), "no conflict clause when the total is zero: {s}");
    }

    #[test]
    fn migrate_output_human_surfaces_conflicts() {
        let summary = MigrateSummary {
            migrated: vec!["src-1".to_owned()],
            docs: 10,
            tokens: 100,
            conflicts: 3,
            remaining: Vec::new(),
        };
        let s = format_migrate_output(&summary, "voyage-code-4@1", false);
        assert!(s.contains("3 conflicts"), "human summary surfaces the conflict total: {s}");
    }

    #[test]
    fn migrate_output_json_is_stable() {
        let summary = MigrateSummary {
            migrated: vec!["src-1".to_owned()],
            docs: 10,
            tokens: 100,
            conflicts: 0,
            remaining: vec!["src-2".to_owned()],
        };
        let s = format_migrate_output(&summary, "voyage-code-4@1", true);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["action"], "models.migrate");
        assert_eq!(v["target_model"], "voyage-code-4@1");
        assert_eq!(v["documents"], 10);
        assert_eq!(v["spent_tokens"], 100);
        // `conflicts` is always present (parity with single-source `ingest`'s
        // `conflict_count`), even when zero, so machine consumers can rely on it.
        assert_eq!(v["conflicts"], 0);
        assert_eq!(v["migrated"][0], "src-1");
        assert_eq!(v["remaining"][0], "src-2");
    }

    /// The migrate `--json` output carries a non-zero aggregate `conflicts`
    /// total so machine consumers can detect partial-failure migrations — the
    /// observability gap this thread closed.
    #[test]
    fn migrate_output_json_surfaces_conflicts() {
        let summary = MigrateSummary {
            migrated: vec!["src-1".to_owned(), "src-2".to_owned()],
            docs: 20,
            tokens: 200,
            conflicts: 5,
            remaining: Vec::new(),
        };
        let s = format_migrate_output(&summary, "voyage-code-4@1", true);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["conflicts"], 5);
    }
}

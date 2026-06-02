//! `mnm models <subcommand>` — local model management + corpus-side
//! introspection.
//!
//! - `mnm models pull` downloads the reranker into the local model cache.
//!   First call fetches ~270 MB from the upstream model registry; subsequent
//!   calls are no-ops. The corpus embedder is VoyageAI (remote — BYOK or the
//!   server's `/v1/embeddings` proxy), so nothing is downloaded for it.
//! - `mnm models active` GETs `/v1/models/active` so callers can verify
//!   that the corpus's active model matches what their queries embed with.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::events::{Component, EventPayload, Outcome};
use mn_telemetry::{Event, TelemetryClient};
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
    /// Download the reranker into the local cache (the corpus embedder is
    /// remote VoyageAI, so nothing is fetched for it).
    Pull(PullArgs),
    /// Show the corpus's currently active embedding model.
    Active(ActiveArgs),
}

/// Args for `mnm models pull`.
#[derive(Debug, ClapArgs)]
pub struct PullArgs {
    /// Override the local model cache directory. Defaults to
    /// `$MIDNIGHT_MANUAL_MODEL_CACHE_DIR` or `$HOME/.cache/midnight-manual/models`.
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
}

/// Args for `mnm models active`.
#[derive(Debug, ClapArgs)]
pub struct ActiveArgs;

/// Dispatch.
///
/// # Errors
///
/// Returns `anyhow::Error` when the cache dir cannot be resolved, the model
/// loader fails, or the cloud round-trip fails for the `active` path.
pub async fn run(
    args: Args,
    server_flag: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    match args.cmd {
        ModelsCmd::Pull(p) => run_pull(p, telemetry, cli_version, json).await,
        ModelsCmd::Active(_) => run_active(server_flag, json).await,
    }
}

async fn run_pull(
    args: PullArgs,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    let started = Instant::now();
    let cache_dir = resolve_cache_dir(args.cache_dir)?;
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create model cache dir at {}", cache_dir.display()))?;

    let result = mn_mcp::tools::run_pull_models(cache_dir.clone())
        .await
        .map_err(|e| anyhow!("{e}"))?;

    let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    telemetry
        .emit(Event::new(
            Component::Cli,
            cli_version,
            EventPayload::PullModels {
                // The corpus embedder is remote VoyageAI; `pull_models` never
                // downloads a local embedder, so this is always false.
                embedder_downloaded: false,
                reranker_downloaded: result.reranker_loaded,
                duration_ms,
                outcome: Outcome::Ok,
            },
        ))
        .await;

    println!(
        "{}",
        format_pull_output(result.reranker, result.reranker_loaded, duration_ms, &cache_dir, json)
    );
    Ok(())
}

async fn run_active(server_flag: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server_flag);
    let parsed = fetch_active(&server_url).await?;
    println!("{}", format_active_output(&parsed, json));
    Ok(())
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

fn resolve_cache_dir(flag: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = flag {
        return Ok(p);
    }
    let env = mn_embedding::cache::StdEnv;
    mn_embedding::cache::resolve(&env)
        .context("could not resolve model cache dir; set MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME")
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
}

#[derive(Debug, Serialize)]
struct PullOutput<'a> {
    action: &'a str,
    reranker: &'a str,
    reranker_downloaded: bool,
    duration_ms: u32,
    cache_dir: String,
}

fn format_pull_output(
    reranker: &str,
    reranker_loaded: bool,
    duration_ms: u32,
    cache_dir: &Path,
    json: bool,
) -> String {
    if json {
        let body = PullOutput {
            action: "models.pull",
            reranker,
            reranker_downloaded: reranker_loaded,
            duration_ms,
            cache_dir: cache_dir.display().to_string(),
        };
        return serde_json::to_string(&body).unwrap_or_default();
    }
    let mut out = String::new();
    writeln!(out, "models pulled in {duration_ms} ms:").ok();
    writeln!(
        out,
        "  reranker: {reranker} ({})",
        if reranker_loaded {
            "downloaded"
        } else {
            "cached"
        },
    )
    .ok();
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
        }
    }

    #[test]
    fn pull_human_output_describes_each_model() {
        let s =
            format_pull_output("bge-reranker-base", false, 1234, Path::new("/tmp/cache"), false);
        assert!(s.contains("1234 ms"));
        assert!(s.contains("bge-reranker-base (cached)"));
        // The embedder is remote Voyage now — no download line, but a note.
        assert!(s.contains("VoyageAI"));
        assert!(s.contains("/tmp/cache"));
    }

    #[test]
    fn pull_json_output_is_stable() {
        let s = format_pull_output("reranker", true, 42, Path::new("/tmp/c"), true);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["action"], "models.pull");
        assert_eq!(v["reranker_downloaded"], true);
        assert_eq!(v["duration_ms"], 42);
        // No embedder fields anymore (corpus embedder is remote Voyage).
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
        let resolved = resolve_cache_dir(Some(p.clone())).unwrap();
        assert_eq!(resolved, p);
    }
}

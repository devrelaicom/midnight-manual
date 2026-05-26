//! `mnm chunks <subcommand>` dispatcher.

use anyhow::{Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::TelemetryClient;

pub mod next;
pub mod prev;
pub mod show;

/// Chunks namespace arguments.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Subcommand.
    #[command(subcommand)]
    pub cmd: ChunksCmd,
}

/// Chunks subcommands.
#[derive(Debug, Subcommand)]
pub enum ChunksCmd {
    /// Fetch and render one chunk with bundled document + source context.
    Show(show::Args),
    /// Fetch the next N chunks after the anchor in the same document.
    Next(next::Args),
    /// Fetch the previous N chunks before the anchor in the same document.
    Prev(prev::Args),
}

/// Dispatcher for chunks namespace.
pub async fn run(
    args: Args,
    server: Option<&str>,
    _telemetry: &TelemetryClient,
    _cli_version: &str,
    json: bool,
) -> Result<()> {
    match args.cmd {
        ChunksCmd::Show(a) => show::run(a, server, json).await,
        ChunksCmd::Next(a) => next::run(a, server, json).await,
        ChunksCmd::Prev(a) => prev::run(a, server, json).await,
    }
}

/// Shared helper used by next + prev. Extracted here to avoid duplicating
/// 25 lines of HTTP + render glue.
pub(super) async fn run_chunk_list(
    args: next::Args,
    server: Option<&str>,
    json: bool,
    dir: &str,
) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let url = format!("{server_url}/v1/chunks/{}/{}?count={}", args.chunk_id, dir, args.count);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::anyhow!("{status} from {url}: {body}"));
    }
    if json {
        println!("{body}");
    } else {
        next::render_chunks(&body, args.full)?;
    }
    Ok(())
}

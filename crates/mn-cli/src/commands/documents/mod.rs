//! `mnm documents <subcommand>` dispatcher.

use anyhow::{Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::TelemetryClient;

pub mod chunks;
pub mod full;
pub mod show;

/// Documents namespace arguments.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Subcommand.
    #[command(subcommand)]
    pub cmd: DocumentsCmd,
}

/// Documents subcommands.
#[derive(Debug, Subcommand)]
pub enum DocumentsCmd {
    /// Render the document overview with ordered chunk IDs.
    Show(show::Args),
    /// Render the complete document with every chunk inline.
    Full(full::Args),
    /// Render a windowed slice of the document's chunks.
    Chunks(chunks::Args),
}

/// Dispatcher for documents namespace.
pub async fn run(
    args: Args,
    server: Option<&str>,
    _telemetry: &TelemetryClient,
    _cli_version: &str,
    json: bool,
) -> Result<()> {
    match args.cmd {
        DocumentsCmd::Show(a) => show::run(a, server, json).await,
        DocumentsCmd::Full(a) => full::run(a, server, json).await,
        DocumentsCmd::Chunks(a) => chunks::run(a, server, json).await,
    }
}

/// Shared GET helper used by show + chunks (full has its own because it
/// handles 412 specially).
pub(super) async fn fetch(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build HTTP client")?;
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::anyhow!("{status} from {url}: {body}"));
    }
    Ok(body)
}

//! `mnm ingest <subcommand>` dispatcher. See §2 of the ingest-UX spec.

use std::path::Path;

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::TelemetryClient;

pub mod plan;
pub mod run;

/// Infer the git short SHA for the source root, or return `"unknown"`.
///
/// Shared by `plan` and `run` so both commands produce consistent revision
/// labels when `--revision` is omitted.
pub(super) fn infer_revision(base: &Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(base)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Top-level arguments for `mnm ingest`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The ingest subcommand to execute.
    #[command(subcommand)]
    pub cmd: IngestCmd,
}

/// `mnm ingest` subcommands.
#[derive(Debug, Subcommand)]
pub enum IngestCmd {
    /// Compute the ingest plan locally without starting a server-side run.
    #[command(hide = true)]
    Plan(plan::Args),
    /// Execute an ingest against the cloud server.
    Run(run::Args),
}

/// Dispatch `mnm ingest <subcommand>`.
pub async fn run(
    args: Args,
    server: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    match args.cmd {
        IngestCmd::Plan(a) => plan::run(a, server, json).await,
        IngestCmd::Run(a) => run::run(a, server, telemetry, cli_version, json).await,
    }
}

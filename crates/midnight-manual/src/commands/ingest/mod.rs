//! `mnm ingest <subcommand>` dispatcher. See §2 of the ingest-UX spec.

use std::path::Path;

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

pub mod plan;
pub mod report;
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
    ///
    /// Always hidden from `--help` (statically `hide = true`). Unlike the parent
    /// `ingest` command — which `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` un-hides via
    /// [`crate::cli`]'s `ADMIN_SUBCOMMANDS` — this nested variant stays hidden
    /// regardless of the toggle; it still runs when called by name.
    #[command(hide = true)]
    Plan(plan::Args),
    /// Execute an ingest against the cloud server.
    Run(run::Args),
}

/// Dispatch `mnm ingest <subcommand>`.
pub async fn run(
    args: Args,
    server: Option<&str>,
    config_path: Option<&Path>,
    voyage_api_key: Option<&str>,
    telemetry: &mnm_telemetry::Telemetry,
    json: bool,
) -> Result<()> {
    match args.cmd {
        IngestCmd::Plan(a) => plan::run(a, server, config_path, json).await,
        IngestCmd::Run(a) => {
            run::run(a, server, config_path, voyage_api_key, telemetry, json).await
        }
    }
}

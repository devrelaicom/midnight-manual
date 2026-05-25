//! `mnm ingest <subcommand>` dispatcher. See §2 of the ingest-UX spec.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::TelemetryClient;

pub mod plan;
pub mod run;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: IngestCmd,
}

#[derive(Debug, Subcommand)]
pub enum IngestCmd {
    /// Compute the ingest plan locally without starting a server-side run.
    #[command(hide = true)]
    Plan(plan::Args),
    /// Execute an ingest against the cloud server.
    Run(run::Args),
}

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

//! `mnm documents <subcommand>` dispatcher.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::TelemetryClient;

// Sub-modules added in Task 10:
//   pub mod show;
//   pub mod full;
//   pub mod chunks;

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
    /// Placeholder variant (hidden); replaced by real verbs in Task 10.
    #[command(hide = true, name = "__reserved")]
    __Reserved,
}

/// Dispatcher for documents namespace. Tasks 9 and 10 add Show/Full/Chunks verbs.
pub async fn run(
    _args: Args,
    _server: Option<&str>,
    _telemetry: &TelemetryClient,
    _cli_version: &str,
    _json: bool,
) -> Result<()> {
    unreachable!("no documents subcommands yet — Task 10 wires Show/Full/Chunks")
}

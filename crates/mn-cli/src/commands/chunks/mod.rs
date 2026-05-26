//! `mnm chunks <subcommand>` dispatcher.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use mn_telemetry::TelemetryClient;

// Sub-modules added in Task 9:
//   pub mod show;
//   pub mod next;
//   pub mod prev;

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
    /// Placeholder variant (hidden); replaced by real verbs in Task 9.
    #[command(hide = true, name = "__reserved")]
    __Reserved,
}

/// Dispatcher for chunks namespace. Tasks 9 and 10 add Show/Next/Prev verbs.
pub async fn run(
    _args: Args,
    _server: Option<&str>,
    _telemetry: &TelemetryClient,
    _cli_version: &str,
    _json: bool,
) -> Result<()> {
    unreachable!("no chunks subcommands yet — Task 9 wires Show/Next/Prev")
}

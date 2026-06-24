//! `mnm admin injection <subcommand>` — prompt-injection detector tooling
//! (issue #103).
//!
//! Two admin-only leaves, both hitting the cloud server's
//! `/v1/admin/injection/...` endpoints with an admin bearer resolved from
//! `auth.toml`:
//!
//! - `service-start` — long-poll the server until its hosted model detector is
//!   warmed up (or report that it is not configured / timed out).
//! - `score <content> [--detector …]` — run an ad-hoc scan of arbitrary content
//!   and print the [`mnm_core::injection::ScanReport`] breakdown.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

pub mod score;
pub mod service_start;

/// `mnm admin injection <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The injection sub-subcommand.
    #[command(subcommand)]
    pub cmd: InjectionCmd,
}

/// `admin injection` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum InjectionCmd {
    /// Warm up the hosted model detector (long-polls the server).
    ServiceStart(ServiceStartArgs),
    /// Run an ad-hoc injection scan against arbitrary content.
    Score(ScoreArgs),
}

/// `mnm admin injection service-start` arguments (none).
#[derive(Debug, ClapArgs)]
pub struct ServiceStartArgs {}

/// `mnm admin injection score <content>` arguments.
#[derive(Debug, ClapArgs)]
pub struct ScoreArgs {
    /// Content to scan for injection.
    #[arg()]
    pub content: String,
    /// Detector(s) to run: `pattern`, `model`, or `pattern,model`.
    #[arg(long, default_value = "pattern,model")]
    pub detector: String,
}

/// Dispatch `mnm admin injection <subcommand>`.
///
/// # Errors
///
/// Returns an error on network failure, non-2xx responses, response-parse
/// failures, or when no admin bearer can be resolved from `auth.toml`.
pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    match args.cmd {
        InjectionCmd::ServiceStart(a) => service_start::run(a, server, json).await,
        InjectionCmd::Score(a) => score::run(a, server, json).await,
    }
}

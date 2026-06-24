//! `mnm admin <subcommand>` — admin-only command group (issue #103).
//!
//! Currently hosts the `injection` subtree (prompt-injection model-detector
//! warmup + ad-hoc scoring against the cloud server's admin endpoints). The
//! whole subtree is admin-only and hidden from `--help` by default (D23); it
//! still executes when called by name. Every leaf resolves an admin bearer from
//! `auth.toml`.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

pub mod injection;

/// `mnm admin <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The admin sub-subcommand.
    #[command(subcommand)]
    pub cmd: AdminCmd,
}

/// `admin` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum AdminCmd {
    /// Prompt-injection detector tooling (model warmup + ad-hoc scoring).
    Injection(injection::Args),
}

/// Dispatch `mnm admin <subcommand>`.
///
/// # Errors
///
/// Returns an error on network failure, non-2xx responses, argument-parse
/// failures, or when no admin bearer can be resolved from `auth.toml`.
pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    match args.cmd {
        AdminCmd::Injection(a) => injection::run(a, server, json).await,
    }
}

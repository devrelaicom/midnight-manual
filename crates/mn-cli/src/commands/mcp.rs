//! `mnm mcp serve` — run the MCP server over stdio.
//!
//! This is the subcommand AI clients (Claude Code, Cursor, etc.) invoke as
//! their MCP transport. Stdout is the wire — only logging goes to stderr
//! (FR-021).

use anyhow::{Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};

/// `mnm mcp <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: McpCmd,
}

/// `mcp` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum McpCmd {
    /// Run the MCP server over stdio (long-running).
    Serve,
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error if the model cache dir cannot be resolved or if the
/// server loop exits abnormally.
pub async fn run(args: Args) -> Result<()> {
    match args.cmd {
        McpCmd::Serve => serve().await,
    }
}

async fn serve() -> Result<()> {
    let env = mn_embedding::cache::StdEnv;
    let cache_dir = mn_embedding::cache::resolve(&env).context(
        "could not resolve model cache dir; set MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME",
    )?;

    // Ensure the directory exists; fastembed will populate it on first model
    // load. Creating it here gives a clearer error message if permissions
    // are wrong.
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create model cache dir at {}", cache_dir.display()))?;

    let cfg = mn_mcp::ServerConfig { cache_dir };
    mn_mcp::run(cfg).await.context("MCP server loop failed")?;
    Ok(())
}

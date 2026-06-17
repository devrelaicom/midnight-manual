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
    let env = mnm_embedding::cache::StdEnv;
    let cache_dir = mnm_embedding::cache::resolve(&env).context(
        "could not resolve model cache dir; set MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME",
    )?;
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create model cache dir at {}", cache_dir.display()))?;

    // Resolve cloud URL via the same Config precedence the rest of the CLI uses.
    let cfg_env = mnm_core::config::StdEnv;
    let (cfg, _) = mnm_core::config::Config::discover(None, &cfg_env).unwrap_or_default();

    // Resolve a read-uplift bearer if the user has run `mnm auth github`.
    // Anonymous mode is fine — the cloud's read endpoints work without auth,
    // they just hit the lower rate-limit tier.
    let bearer_token = crate::shared::resolve_read_uplift_token();

    let mut server_cfg = mnm_mcp::ServerConfig::with_defaults(cache_dir);
    server_cfg.telemetry_url =
        format!("{}/v1/telemetry/events", cfg.server.url.trim_end_matches('/'));
    server_cfg.telemetry_enabled = cfg.telemetry.enabled;
    server_cfg.cloud_url = cfg.server.url;
    server_cfg.bearer_token = bearer_token;

    mnm_mcp::run(server_cfg)
        .await
        .map_err(|e| anyhow::anyhow!("MCP server loop failed: {e}"))?;
    Ok(())
}

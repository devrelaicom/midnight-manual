//! `mnm mcp serve` — run the MCP server over stdio.
//!
//! This is the subcommand AI clients (Claude Code, Cursor, etc.) invoke as
//! their MCP transport. Stdout is the wire — only logging goes to stderr
//! (FR-021).

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use time::OffsetDateTime;

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
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create model cache dir at {}", cache_dir.display()))?;

    // Resolve cloud URL via the same Config precedence the rest of the CLI uses.
    let cfg_env = mn_core::config::StdEnv;
    let (cfg, _) = mn_core::config::Config::discover(None, &cfg_env).unwrap_or_default();

    // Resolve a read-uplift bearer if the user has run `mnm auth github`.
    // Anonymous mode is fine — the cloud's read endpoints work without auth,
    // they just hit the lower rate-limit tier.
    let bearer_token = resolve_read_uplift_token();

    let mut server_cfg = mn_mcp::ServerConfig::with_defaults(cache_dir);
    server_cfg.cloud_url = cfg.server.url;
    server_cfg.bearer_token = bearer_token;

    mn_mcp::run(server_cfg)
        .await
        .map_err(|e| anyhow::anyhow!("MCP server loop failed: {e}"))?;
    Ok(())
}

/// Look up the active read-uplift bearer in `$XDG_CONFIG_HOME/midnight-manual/auth.toml`.
/// Absent or expired tokens degrade silently to anonymous mode.
fn resolve_read_uplift_token() -> Option<String> {
    let path = auth_file_path()?;
    let file = mn_core::auth_file::AuthFile::read_optional(&path)
        .ok()
        .flatten()?;
    file.active_read_uplift_token(OffsetDateTime::now_utc())
        .map(str::to_owned)
}

fn auth_file_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("midnight-manual").join("auth.toml"));
    }
    if let Ok(home) = std::env::var("HOME") {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join("midnight-manual")
                .join("auth.toml"),
        );
    }
    None
}

//! `mnm mcp serve` — run the MCP server over stdio.
//!
//! This is the subcommand AI clients (Claude Code, Cursor, etc.) invoke as
//! their MCP transport. Stdout is the wire — only logging goes to stderr
//! (FR-021).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mnm_core::config::Config;

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
pub async fn run(args: Args, server_flag: Option<&str>, config_path: Option<&Path>) -> Result<()> {
    match args.cmd {
        McpCmd::Serve => serve(server_flag, config_path).await,
    }
}

async fn serve(server_flag: Option<&str>, config_path: Option<&Path>) -> Result<()> {
    // Discover config FIRST so the `[models].cache_dir` override can take part in
    // cache-dir resolution. Falling back to defaults keeps `serve` working even
    // when no config file exists. This single read backs every config-derived
    // value below (cache_dir, telemetry_enabled, AND the server URL via
    // `build_serve_config`) — see `resolve_server_url_from` for why threading
    // the one `cfg` matters.
    let cfg_env = mnm_core::config::StdEnv;
    let (cfg, _) = Config::discover(config_path, &cfg_env).unwrap_or_default();

    // Cache dir precedence for `mnm mcp serve`: config (`[models].cache_dir`) >
    // env-chain (`MIDNIGHT_MANUAL_MODEL_CACHE_DIR` > `XDG_DATA_HOME` > `HOME`).
    // There is no `--cache-dir` flag on this subcommand, so config is the top
    // layer.
    let cache_env = mnm_embedding::cache::StdEnv;
    let cache_dir =
        mnm_embedding::cache::resolve_with_override(cfg.models.cache_dir.as_deref(), &cache_env)
            .context(
                "could not resolve model cache dir; set [models].cache_dir, \
                 MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME",
            )?;
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create model cache dir at {}", cache_dir.display()))?;

    // `mnm mcp serve` forwards ONLY the read-uplift bearer (never an admin
    // token), so it runs at the read-uplift / anonymous tier. This is
    // deliberate: the read-uplift token's 30-day TTL suits a long-running
    // server, whereas an admin token's 1-hour TTL would expire mid-session;
    // and the MCP tool surface is read-only, so admin credentials buy nothing.
    // Resolve a read-uplift bearer if the user has run `mnm auth github`.
    // Anonymous mode is fine — the cloud's read endpoints work without auth,
    // they just hit the lower rate-limit tier.
    let bearer_token = crate::shared::resolve_read_uplift_token();

    let mut server_cfg = build_serve_config(server_flag, &cfg, cache_dir);
    server_cfg.bearer_token = bearer_token;
    server_cfg.security = mnm_core::config::resolve_security_level(None, &cfg.security, &cfg_env);

    mnm_mcp::run(server_cfg)
        .await
        .map_err(|e| anyhow::anyhow!("MCP server loop failed: {e}"))?;
    Ok(())
}

/// Assemble the MCP [`ServerConfig`](mnm_mcp::ServerConfig) for `mnm mcp serve`.
///
/// Pure (no I/O) so the URL/cache-dir precedence is unit-testable:
///
/// - `cloud_url` is resolved via [`crate::shared::resolve_server_url_from`]
///   against the *same* `cfg` the caller already discovered, i.e. flag
///   (`--server`/`MIDNIGHT_MANUAL_SERVER`) > config `[server].url` >
///   compiled-in default. Reusing that one `cfg` (rather than re-running
///   `Config::discover` internally) keeps `cloud_url` and `telemetry_enabled`
///   in lockstep — they cannot read two different on-disk states.
/// - `telemetry_endpoint` comes from `resolve_telemetry_endpoint`, which reads
///   the config's `[telemetry].endpoint` and the `MIDNIGHT_MANUAL_GAUGE_ENDPOINT`
///   env override, defaulting to [`mnm_core::config::DEFAULT_TELEMETRY_ENDPOINT`].
///   The endpoint is Gauge-specific and independent of the cloud server URL.
/// - `cache_dir` is passed through verbatim (the caller has already applied the
///   config > env precedence).
///
/// The read-uplift `bearer_token` is intentionally left at its default (`None`)
/// here; the caller wires it in after this returns.
fn build_serve_config(
    server_flag: Option<&str>,
    cfg: &Config,
    cache_dir: PathBuf,
) -> mnm_mcp::ServerConfig {
    let env = mnm_core::config::StdEnv;
    let cloud_url = crate::shared::resolve_server_url_from(server_flag, cfg);
    let mut server_cfg = mnm_mcp::ServerConfig::with_defaults(cache_dir);
    server_cfg.telemetry_endpoint =
        mnm_core::config::resolve_telemetry_endpoint(&cfg.telemetry, &env);
    server_cfg.telemetry_enabled = cfg.telemetry.enabled;
    server_cfg.cloud_url = cloud_url;
    server_cfg
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn build_serve_config_prefers_flag_over_config_url() {
        let mut cfg = Config::default();
        "https://config.example".clone_into(&mut cfg.server.url);
        let built =
            build_serve_config(Some("http://localhost:8080/"), &cfg, PathBuf::from("/cache"));
        // Flag wins and the trailing slash is stripped by the shared resolver.
        assert_eq!(built.cloud_url, "http://localhost:8080");
        // Telemetry endpoint now comes from Gauge config, not derived from the server URL.
        assert_eq!(built.telemetry_endpoint, mnm_core::config::DEFAULT_TELEMETRY_ENDPOINT);
        assert_eq!(built.cache_dir, PathBuf::from("/cache"));
    }

    #[test]
    fn build_serve_config_carries_telemetry_enabled_flag() {
        let mut cfg = Config::default();
        cfg.telemetry.enabled = false;
        // No flag → `resolve_server_url_from` reads `cloud_url` from the *same*
        // `cfg` whose `telemetry.enabled` we assert below, so both come from one
        // source (no second `Config::discover`, no on-disk read).
        let built = build_serve_config(None, &cfg, PathBuf::from("/cache"));
        assert!(!built.telemetry_enabled);
    }

    #[test]
    fn build_serve_config_no_flag_uses_threaded_cfg_url() {
        // With no flag, the URL must come from the threaded `cfg`, not a fresh
        // on-disk discovery — proving the single-source-of-truth refactor.
        let mut cfg = Config::default();
        "https://config.example/".clone_into(&mut cfg.server.url);
        let built = build_serve_config(None, &cfg, PathBuf::from("/cache"));
        assert_eq!(built.cloud_url, "https://config.example");
        // Telemetry endpoint is Gauge-specific, not derived from the server URL.
        assert_eq!(built.telemetry_endpoint, mnm_core::config::DEFAULT_TELEMETRY_ENDPOINT);
    }

    /// Regression guard for the dispatch wiring (cli.rs:209): the original bug
    /// was that `--server` / `MIDNIGHT_MANUAL_SERVER` never reached
    /// `mcp::run`. Assert clap parses the flag form into `Cli::server` so the
    /// value the dispatch threads in is actually populated. No env mutation —
    /// the long-flag form is sufficient to prove the field is wired.
    #[test]
    fn server_flag_parses_into_cli_field() {
        use clap::Parser as _;

        use crate::cli::Cli;

        let cli = Cli::parse_from(["mnm", "--server", "http://localhost:8080", "mcp", "serve"]);
        assert_eq!(cli.server.as_deref(), Some("http://localhost:8080"));
    }

    /// `[security] level = "strict"` in the discovered config must reach the
    /// MCP `ServerConfig` via the same `resolve_security_level` call `serve`
    /// uses. With a no-op env (every var `None`), the config layer is the sole
    /// deciding source, so the strict level proves the config wiring works.
    #[test]
    fn serve_security_level_resolves_strict_from_config() {
        struct NoEnv;
        impl mnm_core::config::ConfigEnv for NoEnv {
            fn var(&self, _name: &str) -> Option<String> {
                None
            }
        }
        let mut cfg = Config::default();
        cfg.security.level = Some("strict".into());
        let level = mnm_core::config::resolve_security_level(None, &cfg.security, &NoEnv);
        assert_eq!(level, mnm_core::injection::SecurityLevel::Strict);
    }
}

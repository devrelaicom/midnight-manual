//! `mnm mcp serve` — run the MCP server over stdio or Streamable HTTP.
//!
//! This is the subcommand AI clients (Claude Code, Cursor, etc.) invoke as
//! their MCP transport. By default it speaks stdio: stdout is the wire — only
//! logging goes to stderr (FR-021). With `--http` it serves the same tool
//! surface over Streamable HTTP (`POST /mcp`), bound to `127.0.0.1:2400`
//! unless `--bind` / `MIDNIGHT_MANUAL_MCP_BIND` says otherwise.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mnm_core::config::Config;

/// Default Streamable HTTP bind address: loopback, port 2400 (24:00 —
/// midnight). Loopback-only unless the operator opts out via `--bind` /
/// `MIDNIGHT_MANUAL_MCP_BIND`.
const DEFAULT_HTTP_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 2400);

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
    /// Run the MCP server (long-running). Speaks stdio by default; `--http`
    /// switches to the Streamable HTTP transport.
    Serve {
        /// Serve Streamable HTTP (`POST /mcp`) instead of stdio.
        #[arg(long)]
        http: bool,
        /// Bind address for the HTTP transport. Requires `--http` (an
        /// explicit transport choice — `--bind` alone errors rather than
        /// silently implying it). Falls back to `MIDNIGHT_MANUAL_MCP_BIND`,
        /// then `127.0.0.1:2400`.
        #[arg(long, requires = "http", value_name = "IP:PORT")]
        bind: Option<SocketAddr>,
    },
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error if the model cache dir cannot be resolved or if the
/// server loop exits abnormally.
pub async fn run(args: Args, server_flag: Option<&str>, config_path: Option<&Path>) -> Result<()> {
    match args.cmd {
        McpCmd::Serve { http, bind } => serve(server_flag, config_path, http, bind).await,
    }
}

async fn serve(
    server_flag: Option<&str>,
    config_path: Option<&Path>,
    http: bool,
    bind_flag: Option<SocketAddr>,
) -> Result<()> {
    // Discover config FIRST so the `[models].cache_dir` override can take part in
    // cache-dir resolution. Falling back to defaults keeps `serve` working even
    // when no config file exists. This single read backs every config-derived
    // value below (cache_dir, telemetry_enabled, AND the server URL via
    // `build_serve_config`) — see `resolve_server_url_from` for why threading
    // the one `cfg` matters.
    let cfg_env = mnm_core::config::StdEnv;
    let (cfg, _) = Config::discover(config_path, &cfg_env)?;

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
    server_cfg.security = mnm_core::config::resolve_security_level(None, &cfg.security, &cfg_env)?;

    // Every config resolved above is transport-independent — the transport
    // switch changes only how the wire is served, never what it serves.
    if http {
        let bind = resolve_http_bind(bind_flag, &cfg_env)?;
        mnm_mcp::run_http(server_cfg, bind)
            .await
            .map_err(|e| anyhow::anyhow!("MCP HTTP server failed: {e}"))?;
    } else {
        mnm_mcp::run(server_cfg)
            .await
            .map_err(|e| anyhow::anyhow!("MCP server loop failed: {e}"))?;
    }
    Ok(())
}

/// Resolve the Streamable HTTP bind address: `--bind` flag >
/// `MIDNIGHT_MANUAL_MCP_BIND` env > [`DEFAULT_HTTP_BIND`].
///
/// Pure over a [`ConfigEnv`](mnm_core::config::ConfigEnv) accessor (house
/// pattern — see `resolve_security_level`) so the precedence is unit-testable
/// without mutating process env. An empty/whitespace env value falls through
/// to the default like an absent one; a non-empty unparsable value is an error
/// (never a silent fallback). No config-file layer for now — the resolver
/// leaves room to slot `[mcp].http_bind` under the env layer later.
///
/// # Errors
///
/// Returns an error if `MIDNIGHT_MANUAL_MCP_BIND` is set to a value that does
/// not parse as `IP:PORT`.
fn resolve_http_bind(
    flag: Option<SocketAddr>,
    env: &impl mnm_core::config::ConfigEnv,
) -> Result<SocketAddr> {
    if let Some(addr) = flag {
        return Ok(addr);
    }
    if let Some(raw) = env.var("MIDNIGHT_MANUAL_MCP_BIND") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.parse().map_err(|e| {
                anyhow::anyhow!(
                    "invalid MIDNIGHT_MANUAL_MCP_BIND value {trimmed:?}: {e} \
                     (expected IP:PORT, e.g. 127.0.0.1:2400)"
                )
            });
        }
    }
    Ok(DEFAULT_HTTP_BIND)
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
        let level = mnm_core::config::resolve_security_level(None, &cfg.security, &NoEnv)
            .expect("config level=strict resolves cleanly");
        assert_eq!(level, mnm_core::injection::SecurityLevel::Strict);
    }

    // ── HTTP transport flags + bind precedence ──────────────────────────────

    /// A `ConfigEnv` that answers only `MIDNIGHT_MANUAL_MCP_BIND`, with a
    /// caller-chosen value — precedence tests without process-env mutation
    /// (house pattern, same as the `NoEnv` above).
    struct BindEnv(Option<&'static str>);
    impl mnm_core::config::ConfigEnv for BindEnv {
        fn var(&self, name: &str) -> Option<String> {
            (name == "MIDNIGHT_MANUAL_MCP_BIND")
                .then(|| self.0.map(str::to_owned))
                .flatten()
        }
    }

    /// `--bind` is an HTTP-transport knob: clap must reject it without
    /// `--http` (`requires = "http"`) rather than silently implying the
    /// transport switch.
    #[test]
    fn clap_rejects_bind_without_http() {
        use clap::Parser as _;

        use crate::cli::Cli;

        let err = Cli::try_parse_from(["mnm", "mcp", "serve", "--bind", "127.0.0.1:2400"])
            .expect_err("--bind without --http must be a clap error");
        assert!(
            err.to_string().contains("--http"),
            "the error must name the missing --http flag: {err}"
        );
    }

    /// `--http --bind 0.0.0.0:9999` parses into the struct variant with both
    /// fields populated (the deliberate-public-exposure form).
    #[test]
    fn clap_parses_http_with_bind() {
        use clap::Parser as _;

        use crate::cli::{Cli, Command};

        let cli = Cli::parse_from(["mnm", "mcp", "serve", "--http", "--bind", "0.0.0.0:9999"]);
        let Command::Mcp(args) = cli.cmd else {
            panic!("expected the Mcp subcommand");
        };
        let McpCmd::Serve { http, bind } = args.cmd;
        assert!(http, "--http must set the transport switch");
        assert_eq!(bind, Some("0.0.0.0:9999".parse().unwrap()));
    }

    /// Bare `mnm mcp serve` still parses (stdio default): both fields at rest.
    #[test]
    fn clap_parses_bare_serve_as_stdio() {
        use clap::Parser as _;

        use crate::cli::{Cli, Command};

        let cli = Cli::parse_from(["mnm", "mcp", "serve"]);
        let Command::Mcp(args) = cli.cmd else {
            panic!("expected the Mcp subcommand");
        };
        let McpCmd::Serve { http, bind } = args.cmd;
        assert!(!http, "no --http → stdio transport");
        assert_eq!(bind, None);
    }

    #[test]
    fn resolve_http_bind_flag_wins_over_env() {
        let flag: SocketAddr = "10.0.0.5:8000".parse().unwrap();
        let got =
            resolve_http_bind(Some(flag), &BindEnv(Some("127.0.0.1:1234"))).expect("flag resolves");
        assert_eq!(got, flag);
    }

    #[test]
    fn resolve_http_bind_env_wins_over_default() {
        let got = resolve_http_bind(None, &BindEnv(Some("0.0.0.0:2500"))).expect("env resolves");
        assert_eq!(got, "0.0.0.0:2500".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn resolve_http_bind_defaults_to_loopback_2400() {
        let got = resolve_http_bind(None, &BindEnv(None)).expect("default resolves");
        assert_eq!(got, "127.0.0.1:2400".parse::<SocketAddr>().unwrap());
        assert!(got.ip().is_loopback(), "the default MUST be loopback-only");
    }

    /// An empty (or whitespace) env value falls through to the default like an
    /// absent one — matching how every other env layer in the config resolvers
    /// treats empties.
    #[test]
    fn resolve_http_bind_empty_env_falls_through() {
        let got = resolve_http_bind(None, &BindEnv(Some("  "))).expect("empty env falls through");
        assert_eq!(got, DEFAULT_HTTP_BIND);
    }

    /// A garbage env value is a loud error naming the variable — never a
    /// silent fallback to the default (which would mask a typo'd operator
    /// intent to bind elsewhere).
    #[test]
    fn resolve_http_bind_bad_env_value_errors() {
        let err = resolve_http_bind(None, &BindEnv(Some("not-an-addr")))
            .expect_err("garbage env value must error");
        let msg = err.to_string();
        assert!(
            msg.contains("MIDNIGHT_MANUAL_MCP_BIND") && msg.contains("not-an-addr"),
            "error must name the env var and the bad value: {msg}"
        );
    }
}

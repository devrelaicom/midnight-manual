//! Top-level CLI parser.
//!
//! Both `midnight-manual` and `mnm` install from this crate; the two binaries
//! call into [`run`]. The global flags (D17/D18) — `--config`, `--server`,
//! `--token`, `--json`, `--log-level`, `--no-telemetry` — are parsed at the
//! top level and threaded into subcommands.
//!
//! Admin subcommands (`keys`, `login`, `users`) are hidden from `--help` by
//! default (D23 / FR-066). Set `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` (or
//! `cli.show_admin_cmds = true` in `config.toml`) to surface them. The
//! visibility gate never gates *invocation* — a hidden command still runs
//! when called by name.

use anyhow::Result;
use clap::{CommandFactory as _, FromArgMatches as _, Parser, Subcommand};

use crate::commands;

/// midnight-manual: a RAG platform for the Midnight Network.
///
/// Telemetry is opt-out. To disable, do any of:
///
/// 1. Set `MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1` in the environment.
///
/// 2. Set `telemetry.enabled = false` in your `config.toml`.
///
/// 3. Run `mnm telemetry disable` (writes a runtime marker).
///
/// When disabled, zero events leave your machine and no connection to the
/// telemetry endpoint is opened. See the README's 'Telemetry & Privacy'
/// section for what is collected.
#[derive(Debug, Parser)]
#[command(name = "mnm", version, about, long_about = None)]
pub struct Cli {
    /// Override the discovered config file path.
    #[arg(long, global = true, env = "MIDNIGHT_MANUAL_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    /// Override the cloud server URL.
    #[arg(long, global = true, env = "MIDNIGHT_MANUAL_SERVER")]
    pub server: Option<String>,

    /// Emit JSON on stdout instead of human-formatted text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Logging verbosity: `error`, `warn`, `info`, `debug`, `trace`.
    #[arg(long, global = true, env = "RUST_LOG")]
    pub log_level: Option<String>,

    /// Disable telemetry for this invocation (FR-107 mechanism #1).
    #[arg(long, global = true, env = "MIDNIGHT_MANUAL_DISABLE_TELEMETRY")]
    pub no_telemetry: bool,

    /// The subcommand.
    #[command(subcommand)]
    pub cmd: Command,
}

/// All top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the CLI version + build metadata.
    Version,
    /// Diagnostic report.
    Doctor(commands::doctor::Args),
    /// Source registry inspection.
    Sources(commands::sources::Args),
    /// Source-version inspection.
    Versions(commands::versions::Args),
    /// Show or edit the resolved config.
    Config(commands::config::Args),
    /// MCP server (stdio JSON-RPC) and related tooling.
    Mcp(commands::mcp::Args),
    /// GitHub OAuth read-uplift flow + local auth-file inspection.
    Auth(commands::auth::Args),
    /// Ed25519 keypair management (admin; hidden by default).
    Keys(commands::keys::Args),
    /// Admin login via challenge-response (admin; hidden by default).
    Login(commands::login::Args),
    /// Local user-store CRUD (admin; hidden by default).
    Users(commands::users::Args),
}

/// Subcommand names that are admin-only and therefore hidden from `--help`
/// unless `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` is set (FR-066).
const ADMIN_SUBCOMMANDS: &[&str] = &["keys", "login", "users"];

/// Parse argv and dispatch.
///
/// # Errors
///
/// Returns `anyhow::Error` on argument-parse failures or subcommand failures.
pub async fn run() -> Result<()> {
    let show_admin = should_show_admin_cmds();
    let mut cmd = Cli::command();
    if !show_admin {
        for name in ADMIN_SUBCOMMANDS {
            cmd = cmd.mut_subcommand(*name, |c| c.hide(true));
        }
    }
    let matches = cmd.get_matches();
    let cli = Cli::from_arg_matches(&matches).map_err(anyhow::Error::from)?;
    init_logging(cli.log_level.as_deref());

    match cli.cmd {
        Command::Version => commands::version::run(cli.json),
        Command::Doctor(args) => commands::doctor::run(args, cli.json).await,
        Command::Sources(args) => {
            commands::sources::run(args, cli.server.as_deref(), cli.json).await
        }
        Command::Versions(args) => {
            commands::versions::run(args, cli.server.as_deref(), cli.json).await
        }
        Command::Config(args) => commands::config::run(args, cli.config.as_deref(), cli.json).await,
        Command::Mcp(args) => commands::mcp::run(args).await,
        Command::Auth(args) => commands::auth::run(args, cli.server.as_deref(), cli.json).await,
        Command::Keys(args) => commands::keys::run(args, cli.json),
        Command::Login(args) => commands::login::run(args, cli.server.as_deref(), cli.json).await,
        Command::Users(args) => commands::users::run(args, cli.json),
    }
}

/// Resolve admin-visibility (D23 / FR-066).
///
/// Precedence: `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS` env > `cli.show_admin_cmds`
/// config field > hidden.
fn should_show_admin_cmds() -> bool {
    if let Ok(v) = std::env::var("MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS") {
        // Match the same truthy-set the rest of the CLI uses (FR-016).
        return matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES");
    }
    let env = mn_core::config::StdEnv;
    let (cfg, _) = mn_core::config::Config::discover(None, &env).unwrap_or_default();
    cfg.cli.show_admin_cmds
}

fn init_logging(level: Option<&str>) {
    use tracing_subscriber::EnvFilter;
    let filter = match level {
        Some(l) => EnvFilter::new(l),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
    };
    // CLI diagnostics go to stderr (FR-021); stdout is reserved for --json payloads.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

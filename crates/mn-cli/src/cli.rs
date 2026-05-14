//! Top-level CLI parser.
//!
//! Both `midnight-manual` and `mnm` install from this crate; the two binaries
//! call into [`run`]. The global flags (D17/D18) — `--config`, `--server`,
//! `--token`, `--json`, `--log-level`, `--no-telemetry` — are parsed at the
//! top level and threaded into subcommands.

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::commands;

/// midnight-manual: a RAG platform for the Midnight Network.
///
/// Telemetry is opt-out. To disable, do any of:
///
/// 1. Set `MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1` in the environment.
/// 2. Set `telemetry.enabled = false` in your `config.toml`.
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
}

/// Parse argv and dispatch.
///
/// # Errors
///
/// Returns `anyhow::Error` on argument-parse failures or subcommand failures.
pub async fn run() -> Result<()> {
    let cli = Cli::parse();
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
    }
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

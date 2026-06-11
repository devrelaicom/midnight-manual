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

use std::time::Instant;

use anyhow::Result;
use clap::{CommandFactory as _, FromArgMatches as _, Parser, Subcommand};
use mn_telemetry::events::{CliCommandName, Component, EventPayload, Outcome};
use mn_telemetry::{Event, TelemetryClient};

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

    /// Voyage API key for BYOK embedding (overrides env + config).
    /// Resolution order: this flag > VOYAGE_API_KEY env > config.toml.
    /// When absent, embedding is proxied through the server endpoint.
    #[arg(long, global = true)]
    pub voyage_api_key: Option<String>,

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
    /// Connectivity, auth, and model readiness check.
    Status(commands::status::Args),
    /// Ad-hoc retrieval — `mnm search <query>`. Boxed: the search Args is by
    /// far the largest payload (clippy::large_enum_variant).
    Search(Box<commands::search::Args>),
    /// Print the corpus's filterable facets (modes + filter keys/values).
    Facets(commands::facets::Args),
    /// Source registry inspection.
    Sources(commands::sources::Args),
    /// Source-version inspection.
    Versions(commands::versions::Args),
    /// Show or edit the resolved config.
    Config(commands::config::Args),
    /// MCP server (stdio JSON-RPC) and related tooling.
    Mcp(commands::mcp::Args),
    /// Local model management — `mnm models {pull,active}`.
    Models(commands::models::Args),
    /// GitHub OAuth read-uplift flow + local auth-file inspection.
    Auth(commands::auth::Args),
    /// Telemetry opt-out toggle and status.
    Telemetry(commands::telemetry::Args),
    /// Ed25519 keypair management (admin; hidden by default).
    Keys(commands::keys::Args),
    /// Admin login via challenge-response (admin; hidden by default).
    Login(commands::login::Args),
    /// Local user-store CRUD (admin; hidden by default).
    Users(commands::users::Args),
    /// Run an admin ingest from a manifest (admin; hidden by default).
    Ingest(commands::ingest::Args),
    /// Per-CIDR rate-limit override CRUD (admin; hidden by default).
    Ratelimits(commands::ratelimits::Args),
    /// Per-CIDR / per-user embedding token-limit override CRUD (admin; hidden by default).
    Tokenlimits(commands::tokenlimits::Args),
    /// Manifest authoring + validation (local only).
    Manifest(commands::manifest::Args),
    /// Inspect chunks: show, next, prev, neighbors.
    Chunks(commands::chunks::Args),
    /// Inspect documents: show, chunks.
    Documents(commands::documents::Args),
    /// Install the advanced-search skill into your AI harness(es).
    Skills(commands::skills::Args),
}

/// Subcommand names that are admin-only and therefore hidden from `--help`
/// unless `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` is set (FR-066).
const ADMIN_SUBCOMMANDS: &[&str] = &[
    "keys",
    "login",
    "users",
    "ingest",
    "ratelimits",
    "tokenlimits",
];

/// Parse argv and dispatch.
///
/// # Errors
///
/// Returns `anyhow::Error` on argument-parse failures or subcommand failures.
#[allow(clippy::too_many_lines)]
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

    let started = Instant::now();
    let env = mn_core::config::StdEnv;
    let (cfg, _) =
        mn_core::config::Config::discover(cli.config.as_deref(), &env).unwrap_or_default();
    // FR-107 mechanism #3: seed the runtime toggle from the persistent
    // marker so a previous `mnm telemetry disable` survives invocation
    // boundaries. The two other mechanisms are env (#1) and config (#2).
    mn_telemetry::optout::load_persistent_marker(
        mn_core::paths::telemetry_marker_path(&env).as_deref(),
    );
    let cloud_url = cli.server.clone().unwrap_or_else(|| cfg.server.url.clone());
    let telemetry_url = format!("{}/v1/telemetry/events", cloud_url.trim_end_matches('/'));
    let config_enabled = cfg.telemetry.enabled && !cli.no_telemetry;
    let telemetry =
        TelemetryClient::boot(&telemetry_url, config_enabled).unwrap_or(TelemetryClient::Disabled);

    let command_name = cli_command_name(&cli.cmd);

    let result = match cli.cmd {
        Command::Version => commands::version::run(cli.json),
        Command::Doctor(args) => commands::doctor::run(args, cli.json).await,
        Command::Status(args) => {
            commands::status::run(
                args,
                cli.server.as_deref(),
                cli.voyage_api_key.as_deref(),
                cli.json,
            )
            .await
        }
        Command::Search(args) => {
            commands::search::run(
                *args,
                cli.server.as_deref(),
                cli.config.as_deref(),
                cli.voyage_api_key.as_deref(),
                &telemetry,
                crate::VERSION,
                cli.json,
            )
            .await
        }
        Command::Facets(args) => commands::facets::run(args, cli.server.as_deref(), cli.json).await,
        Command::Sources(args) => {
            commands::sources::run(args, cli.server.as_deref(), cli.json).await
        }
        Command::Versions(args) => {
            commands::versions::run(args, cli.server.as_deref(), cli.json).await
        }
        Command::Config(args) => commands::config::run(args, cli.config.as_deref(), cli.json).await,
        Command::Mcp(args) => commands::mcp::run(args).await,
        Command::Models(args) => {
            commands::models::run(
                args,
                cli.server.as_deref(),
                cli.config.as_deref(),
                cli.voyage_api_key.as_deref(),
                &telemetry,
                crate::VERSION,
                cli.json,
            )
            .await
        }
        Command::Auth(args) => commands::auth::run(args, cli.server.as_deref(), cli.json).await,
        Command::Telemetry(args) => commands::telemetry::run(&args, cli.json),
        Command::Keys(args) => commands::keys::run(args, cli.json),
        Command::Login(args) => commands::login::run(args, cli.server.as_deref(), cli.json).await,
        Command::Users(args) => commands::users::run(args, cli.json),
        Command::Ingest(args) => {
            commands::ingest::run(
                args,
                cli.server.as_deref(),
                cli.config.as_deref(),
                cli.voyage_api_key.as_deref(),
                &telemetry,
                crate::VERSION,
                cli.json,
            )
            .await
        }
        Command::Ratelimits(args) => {
            commands::ratelimits::run(args, cli.server.as_deref(), cli.json).await
        }
        Command::Tokenlimits(args) => {
            commands::tokenlimits::run(args, cli.server.as_deref(), cli.json).await
        }
        Command::Manifest(args) => commands::manifest::run(args).await,
        Command::Chunks(args) => {
            commands::chunks::run(args, cli.server.as_deref(), &telemetry, crate::VERSION, cli.json)
                .await
        }
        Command::Documents(args) => {
            commands::documents::run(
                args,
                cli.server.as_deref(),
                &telemetry,
                crate::VERSION,
                cli.json,
            )
            .await
        }
        Command::Skills(args) => commands::skills::run(args, cli.json),
    };

    let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    let outcome = if result.is_ok() {
        Outcome::Ok
    } else {
        Outcome::Error
    };
    telemetry
        .emit(Event::new(
            Component::Cli,
            crate::VERSION,
            EventPayload::CliCommand {
                command: command_name,
                duration_ms,
                outcome,
            },
        ))
        .await;
    // Force the queue out before exit — the CLI is typically too short-lived
    // to hit the 30s timer.
    telemetry.flush().await;

    result
}

const fn cli_command_name(cmd: &Command) -> CliCommandName {
    match cmd {
        Command::Version => CliCommandName::Version,
        Command::Doctor(_) => CliCommandName::Doctor,
        Command::Status(_) => CliCommandName::Status,
        // No dedicated `Sources` variant in the closed enum yet — emit as
        // `sources` via the dedicated CliCommandName::Sources discriminant.
        Command::Search(_) => CliCommandName::Search,
        Command::Facets(_) => CliCommandName::Facets,
        Command::Sources(_) | Command::Versions(_) => CliCommandName::Sources,
        Command::Config(_) => CliCommandName::Config,
        Command::Mcp(_) => CliCommandName::Mcp,
        Command::Models(_) => CliCommandName::Models,
        Command::Auth(_) | Command::Login(_) | Command::Keys(_) | Command::Users(_) => {
            CliCommandName::Auth
        }
        Command::Telemetry(_) => CliCommandName::Telemetry,
        Command::Ingest(_) => CliCommandName::Ingest,
        Command::Ratelimits(_) => CliCommandName::Ratelimits,
        Command::Tokenlimits(_) => CliCommandName::Tokenlimits,
        Command::Manifest(_) => CliCommandName::Manifest,
        Command::Chunks(_) => CliCommandName::Chunks,
        Command::Documents(_) => CliCommandName::Documents,
        Command::Skills(_) => CliCommandName::Skills,
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

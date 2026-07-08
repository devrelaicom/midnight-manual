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
// `ConfigEnv` brings `.var(..)` into scope for the Sentry DSN lookup in `run`.
use mnm_core::config::ConfigEnv as _;
use mnm_telemetry::events::{CliCommand, CliCommandName, Outcome};
use mnm_telemetry::{build as build_telemetry, BuildParams, Telemetry, FLUSH_ARGS};

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

    /// Disable telemetry for this invocation (FR-107 mechanism #1 is the env var).
    #[arg(long, global = true)]
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
    /// Show the resolved config.
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
    /// Admin tooling group: prompt-injection detector warmup + scoring (admin; hidden by default).
    Admin(commands::admin::Args),
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
    /// Install bundled skills into your AI harness(es).
    Skills(commands::skills::Args),
}

/// Subcommand names that are admin-only and therefore hidden from `--help`
/// unless `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` is set (FR-066).
const ADMIN_SUBCOMMANDS: &[&str] = &[
    "admin",
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

    let env = mnm_core::config::StdEnv;

    // Opt-in Sentry error reporting (mnm-sentry). Disabled by default. Init
    // BEFORE `init_logging` so the sentry-tracing layer can attach. The guard
    // is held for all of `run()` (process lifetime for `mcp serve`); it is
    // `Send`+`Sync`, so holding it across `.await` is fine. The client gate
    // also requires the local `auth.toml` to carry an `[admin]` section, but we
    // only read that file when the cheap env pre-check already passes — so a
    // disabled-by-default install never touches auth.toml here.
    let sentry_guard = if mnm_sentry::env_gate_passes(&env) {
        // Only now touch auth.toml. Any read error (missing, malformed, or
        // insecure-permission file) is treated as "no admin" — a fail-safe that
        // disables Sentry rather than blocking startup. Logging isn't initialized
        // yet here, so there is nowhere to surface the error.
        let auth = mnm_core::paths::auth_file_path(&env).and_then(|p| {
            mnm_core::auth_file::AuthFile::read_optional(&p)
                .ok()
                .flatten()
        });
        let admin = auth.as_ref().and_then(|f| f.admin.as_ref());
        let admin_present = admin.is_some();
        let admin_user_id = admin.map(|a| a.user_id.clone());
        let mut secrets = Vec::new();
        if let Some(a) = admin {
            secrets.push(a.token.clone());
        }
        if let Some(ru) = auth.as_ref().and_then(|f| f.read_uplift.as_ref()) {
            secrets.push(ru.token.clone());
        }
        if let Some(v) = std::env::var("VOYAGE_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
        {
            secrets.push(v);
        }
        if let Some(dsn) = env.var(mnm_sentry::KEY_ENV) {
            secrets.push(dsn);
        }
        mnm_sentry::init(
            &env,
            mnm_sentry::InitOptions {
                admin_present,
                release: crate::VERSION,
                default_environment: "development",
                admin_user_id,
                secrets,
                enable_logs: true,
                enable_metrics: true,
                enable_traces: true,
                traces_sample_rate: 1.0,
                surface: "cli",
            },
        )
    } else {
        None
    };

    // Discover config BEFORE logging so `[log].level` can govern the logger.
    // A genuinely-absent file still yields defaults; a present-but-malformed
    // file fails loud here (propagated to `main` -> stderr + exit 1). Sentry is
    // already initialised above and stays config-independent.
    let (cfg, _) = mnm_core::config::Config::discover(cli.config.as_deref(), &env)?;

    let log_level = mnm_core::config::resolve_log_level(cli.log_level.as_deref(), &cfg.log, &env);
    init_logging(log_level.as_deref(), sentry_guard.is_some());
    // Keep the Sentry guard alive until `run()` returns so buffered events flush.
    let _sentry_guard = sentry_guard;

    let started = Instant::now();

    // Resolve the three opt-out mechanisms into Gauge's two consent inputs.
    let marker = mnm_core::paths::telemetry_marker_path(&env);
    let runtime_enabled = !cli.no_telemetry
        && !mnm_telemetry::optout::env_disabled(&env)
        && !marker
            .as_deref()
            .is_some_and(mnm_telemetry::optout::marker_present);
    let endpoint = mnm_core::config::resolve_telemetry_endpoint(&cfg.telemetry, &env);
    let telemetry: Telemetry = build_telemetry(BuildParams {
        app_version: crate::VERSION.to_owned(),
        endpoint,
        install_id_path: mnm_core::paths::telemetry_install_id_path(&env),
        config_enabled: cfg.telemetry.enabled,
        runtime_enabled,
        flush_args: FLUSH_ARGS.iter().map(|s| (*s).to_owned()).collect(),
    });

    // Hidden `telemetry flush` re-exec: drain and exit BEFORE any normal path
    // (load-bearing — see gauge_telemetry::Telemetry::run_flush docs).
    if let Command::Telemetry(args) = &cli.cmd {
        if matches!(args.cmd, commands::telemetry::TelemetryCmd::Flush) {
            telemetry.run_flush();
            return Ok(());
        }
    }

    let command_name = cli_command_name(&cli.cmd);

    let result = match cli.cmd {
        Command::Version => commands::version::run(cli.json),
        Command::Doctor(args) => {
            commands::doctor::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json)
                .await
        }
        Command::Status(args) => {
            commands::status::run(
                args,
                cli.server.as_deref(),
                cli.config.as_deref(),
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
                cli.json,
            )
            .await
        }
        Command::Facets(args) => {
            commands::facets::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json)
                .await
        }
        Command::Sources(args) => {
            commands::sources::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json)
                .await
        }
        Command::Versions(args) => {
            commands::versions::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json)
                .await
        }
        Command::Config(args) => {
            commands::config::run(
                args,
                cli.config.as_deref(),
                cli.server.as_deref(),
                cli.voyage_api_key.as_deref(),
                cli.no_telemetry,
                cli.json,
            )
            .await
        }
        Command::Mcp(args) => {
            commands::mcp::run(args, cli.server.as_deref(), cli.config.as_deref()).await
        }
        Command::Models(args) => {
            commands::models::run(
                args,
                cli.server.as_deref(),
                cli.config.as_deref(),
                cli.voyage_api_key.as_deref(),
                &telemetry,
                cli.json,
            )
            .await
        }
        Command::Auth(args) => {
            commands::auth::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json).await
        }
        Command::Telemetry(args) => commands::telemetry::run(&args, cli.json),
        Command::Keys(args) => commands::keys::run(args, cli.json),
        Command::Login(args) => {
            commands::login::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json).await
        }
        Command::Users(args) => commands::users::run(args, cli.json),
        Command::Admin(args) => {
            commands::admin::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json).await
        }
        Command::Ingest(args) => {
            commands::ingest::run(
                args,
                cli.server.as_deref(),
                cli.config.as_deref(),
                cli.voyage_api_key.as_deref(),
                &telemetry,
                cli.json,
            )
            .await
        }
        Command::Ratelimits(args) => {
            commands::ratelimits::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json)
                .await
        }
        Command::Tokenlimits(args) => {
            commands::tokenlimits::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json)
                .await
        }
        Command::Manifest(args) => commands::manifest::run(args, cli.json).await,
        Command::Chunks(args) => {
            commands::chunks::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json)
                .await
        }
        Command::Documents(args) => {
            commands::documents::run(args, cli.server.as_deref(), cli.config.as_deref(), cli.json)
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
    telemetry.emit(&CliCommand {
        command: command_name,
        duration_ms,
        outcome,
    });
    // Hand the queue to a detached flusher so the user's prompt returns now;
    // events are already durably on disk from emit().
    telemetry.spawn_detached_flush();

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
        Command::Admin(_) => CliCommandName::Admin,
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
    // Best-effort: this runs before clap parsing (to decide which subcommands
    // to hide) so it cannot honor `--config`, and a malformed config here just
    // hides admin commands — the authoritative loud failure fires moments later
    // at the main `Config::discover` in `run`.
    let env = mnm_core::config::StdEnv;
    let (cfg, _) = mnm_core::config::Config::discover(None, &env).unwrap_or_default();
    mnm_core::config::resolve_show_admin_cmds(&cfg.cli, &env)
}

fn init_logging(level: Option<&str>, sentry_on: bool) {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::Layer as _;
    let filter = match level {
        Some(l) => EnvFilter::new(l),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
    };
    // CLI diagnostics go to stderr (FR-021); stdout is reserved for --json payloads.
    // The sentry-tracing layer is attached only when Sentry initialized (inert
    // otherwise). `Option<Layer>` is itself a `Layer`, so this is a no-op when off.
    let sentry_layer = if sentry_on {
        Some(mnm_sentry::tracing_layer())
    } else {
        None
    };
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(filter),
        )
        .with(sentry_layer)
        .try_init();
}

//! `mnm doctor` — diagnostic report. First-pass implementation; Phase 8 expands
//! it with model presence, MCP install state, etc.

use anyhow::Result;
use clap::Args as ClapArgs;
use mn_telemetry::optout;
use serde::Serialize;

use crate::commands::version::VersionInfo;

/// Arguments for `mnm doctor`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Emit a single JSON object instead of human-formatted output.
    #[arg(long)]
    pub json: bool,
}

/// Diagnostic report shape.
#[derive(Debug, Serialize)]
struct Report {
    cli: VersionInfo,
    config_file: Option<String>,
    admin_visibility: bool,
    telemetry: TelemetryReport,
}

/// Telemetry section of the doctor report (FR-073 + Phase 8b).
#[derive(Debug, Serialize)]
struct TelemetryReport {
    /// `true` when the three-mechanism resolver currently allows emission.
    enabled: bool,
    /// Resolved sink URL, derived from `[server].url`.
    sink_url: String,
    /// Per-mechanism disable status (env / config / runtime).
    disabled_by: DisabledBy,
}

/// Which of the three opt-out mechanisms is currently active. Pulled into
/// its own struct so the doctor `Report` does not bump up against clippy's
/// "more than 3 bools in a struct" pedantic lint.
#[derive(Debug, Serialize)]
struct DisabledBy {
    /// `MIDNIGHT_MANUAL_DISABLE_TELEMETRY` is set to a truthy value.
    env: bool,
    /// `config.telemetry.enabled` is `false`.
    config: bool,
    /// Process-local runtime toggle is set.
    runtime: bool,
}

/// Run the `doctor` subcommand.
///
/// # Errors
///
/// Returns an error if config discovery fails.
pub async fn run(args: Args, json: bool) -> Result<()> {
    let emit_json = json || args.json;

    let env = mn_core::config::StdEnv;
    let (cfg, path) = mn_core::config::Config::discover(None, &env)?;
    let env_disabled = std::env::var(optout::DISABLE_ENV_VAR)
        .ok()
        .is_some_and(|v| {
            matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
        });
    let telemetry = TelemetryReport {
        enabled: optout::is_enabled(&optout::StdEnv, cfg.telemetry.enabled),
        sink_url: format!("{}/v1/telemetry/events", cfg.server.url.trim_end_matches('/')),
        disabled_by: DisabledBy {
            env: env_disabled,
            config: !cfg.telemetry.enabled,
            runtime: optout::runtime_disabled(),
        },
    };
    let report = Report {
        cli: VersionInfo::current(),
        config_file: path.map(|p| p.display().to_string()),
        admin_visibility: std::env::var("MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS")
            .ok()
            .is_some_and(|v| !matches!(v.as_str(), "0" | "false" | "no")),
        telemetry,
    };

    if emit_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("mnm doctor");
        println!("  version:           {} ({})", report.cli.version, report.cli.host);
        println!(
            "  config file:       {}",
            report
                .config_file
                .as_deref()
                .unwrap_or("(none — using defaults)")
        );
        println!("  admin visibility:  {}", if report.admin_visibility { "on" } else { "off" });
        println!(
            "  telemetry:         {} (sink: {})",
            if report.telemetry.enabled {
                "on"
            } else {
                "off"
            },
            report.telemetry.sink_url,
        );
        if report.telemetry.disabled_by.env {
            println!("    - disabled by env ({})", optout::DISABLE_ENV_VAR);
        }
        if report.telemetry.disabled_by.config {
            println!("    - disabled by config.toml (telemetry.enabled = false)");
        }
        if report.telemetry.disabled_by.runtime {
            println!("    - disabled by runtime toggle (`mnm telemetry disable`)");
        }
    }
    Ok(())
}

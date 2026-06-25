//! `mnm telemetry` — runtime opt-out toggle (FR-107 mechanism #3) and status.
//!
//! Three subcommands:
//!
//! - `mnm telemetry disable` — write the persistent marker. After this point
//!   every CLI / MCP invocation on this machine boots with telemetry off,
//!   regardless of env or config.
//! - `mnm telemetry enable` — remove the marker.
//! - `mnm telemetry status` — print the resolved state plus the three
//!   per-mechanism disable flags. Same data the `mnm doctor` telemetry
//!   block surfaces.

use anyhow::{Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mnm_telemetry::optout;
use serde::Serialize;

/// `mnm telemetry <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: TelemetryCmd,
}

/// `telemetry` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum TelemetryCmd {
    /// Persistently disable telemetry on this machine.
    Disable,
    /// Re-enable telemetry (removes the persistent marker).
    Enable,
    /// Show the resolved opt-out state.
    Status,
    /// Internal: drain the on-disk telemetry queue and exit (detached flush).
    #[command(hide = true)]
    Flush,
}

/// JSON shape for `--json` output.
#[derive(Debug, Serialize)]
struct StatusReport {
    enabled: bool,
    endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    marker_path: Option<String>,
    marker_present: bool,
    /// Flattened for backwards-compat with the doctor JSON shape.
    #[serde(flatten)]
    disabled_by: DisabledByFlat,
}

/// Flattening helper — prefixed at the JSON wire so existing scripts keep working.
#[derive(Debug, Serialize)]
struct DisabledByFlat {
    disabled_by_env: bool,
    disabled_by_config: bool,
    disabled_by_runtime: bool,
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error if the marker file cannot be created / removed, or if
/// the XDG config directory cannot be resolved (no `HOME`).
pub fn run(args: &Args, json: bool) -> Result<()> {
    match &args.cmd {
        TelemetryCmd::Disable => run_disable(json),
        TelemetryCmd::Enable => run_enable(json),
        TelemetryCmd::Status => run_status(json),
        // Intercepted in `cli::run` before dispatch; unreachable in practice.
        TelemetryCmd::Flush => Ok(()),
    }
}

fn marker_path() -> Result<std::path::PathBuf> {
    let env = mnm_core::config::StdEnv;
    mnm_core::paths::telemetry_marker_path(&env)
        .context("could not resolve telemetry marker path; set HOME or XDG_CONFIG_HOME")
}

fn run_disable(json: bool) -> Result<()> {
    let path = marker_path()?;
    optout::write_marker(&path)
        .with_context(|| format!("write telemetry marker at {}", path.display()))?;
    if json {
        println!("{}", serde_json::json!({"telemetry": "disabled", "marker_path": path}));
    } else {
        println!("Telemetry disabled. Marker written to {}.", path.display());
        println!("Reverse with: mnm telemetry enable");
    }
    Ok(())
}

fn run_enable(json: bool) -> Result<()> {
    let path = marker_path()?;
    optout::remove_marker(&path)
        .with_context(|| format!("remove telemetry marker at {}", path.display()))?;
    if json {
        println!("{}", serde_json::json!({"telemetry": "enabled", "marker_path": path}));
    } else {
        println!(
            "Telemetry enabled (subject to env / config). Marker removed from {}.",
            path.display()
        );
    }
    Ok(())
}

fn run_status(json: bool) -> Result<()> {
    let env = mnm_core::config::StdEnv;
    let (cfg, _) = mnm_core::config::Config::discover(None, &env).unwrap_or_default();
    let path = mnm_core::paths::telemetry_marker_path(&env);
    let marker_present = path.as_deref().is_some_and(optout::marker_present);
    let disabled_by_env = optout::env_disabled(&env);
    let disabled_by_config = !cfg.telemetry.enabled;
    let enabled = !disabled_by_config && !disabled_by_env && !marker_present;
    let report = StatusReport {
        enabled,
        endpoint: mnm_core::config::resolve_telemetry_endpoint(&cfg.telemetry, &env),
        marker_path: path.as_ref().map(|p| p.display().to_string()),
        marker_present,
        disabled_by: DisabledByFlat {
            disabled_by_env,
            disabled_by_config,
            disabled_by_runtime: marker_present,
        },
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("telemetry: {}", if report.enabled { "on" } else { "off" });
        println!("  endpoint:           {}", report.endpoint);
        println!(
            "  marker file:        {}",
            report
                .marker_path
                .as_deref()
                .unwrap_or("(no HOME / XDG_CONFIG_HOME)"),
        );
        println!("  marker present:     {}", report.marker_present);
        if report.disabled_by.disabled_by_env {
            println!("  - disabled by env ({})", optout::DISABLE_ENV_VAR);
        }
        if report.disabled_by.disabled_by_config {
            println!("  - disabled by config.toml (telemetry.enabled = false)");
        }
        if report.disabled_by.disabled_by_runtime {
            println!("  - disabled by runtime marker");
        }
    }
    Ok(())
}

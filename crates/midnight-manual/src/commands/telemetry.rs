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
}

/// JSON shape for `--json` output.
#[derive(Debug, Serialize)]
struct StatusReport {
    enabled: bool,
    sink_url: String,
    marker_path: Option<String>,
    marker_present: bool,
    // Flattened for backwards-compat with the doctor JSON shape, which
    // exposes the same per-mechanism keys at the top level.
    #[serde(flatten)]
    disabled_by: DisabledByFlat,
}

/// Flattening helper — same fields as [`DisabledBy`] but prefixed at the
/// JSON wire so existing scripts that read `disabled_by_env` keep working.
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
    }
}

fn marker_path() -> Result<std::path::PathBuf> {
    let env = mnm_core::config::StdEnv;
    mnm_core::paths::telemetry_marker_path(&env)
        .context("could not resolve telemetry marker path; set HOME or XDG_CONFIG_HOME")
}

fn run_disable(json: bool) -> Result<()> {
    let path = marker_path()?;
    optout::write_persistent_marker(&path)
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
    optout::remove_persistent_marker(&path)
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
    if let Some(p) = path.as_deref() {
        optout::load_persistent_marker(Some(p));
    } else {
        optout::load_persistent_marker(None);
    }
    let env_disabled = std::env::var(optout::DISABLE_ENV_VAR)
        .ok()
        .is_some_and(|v| {
            matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
        });
    let marker_present = path.as_deref().is_some_and(std::path::Path::exists);
    let report = StatusReport {
        enabled: optout::is_enabled(&optout::StdEnv, cfg.telemetry.enabled),
        sink_url: format!("{}/v1/telemetry/events", cfg.server.url.trim_end_matches('/')),
        marker_path: path.as_ref().map(|p| p.display().to_string()),
        marker_present,
        disabled_by: DisabledByFlat {
            disabled_by_env: env_disabled,
            disabled_by_config: !cfg.telemetry.enabled,
            disabled_by_runtime: optout::runtime_disabled(),
        },
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("telemetry: {}", if report.enabled { "on" } else { "off" });
        println!("  sink:               {}", report.sink_url);
        println!(
            "  marker file:        {}",
            report
                .marker_path
                .as_deref()
                .unwrap_or("(no HOME / XDG_CONFIG_HOME — cannot resolve)"),
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

//! `mnm doctor` — diagnostic report. First-pass implementation; Phase 8 expands
//! it with model presence, MCP install state, etc.

use anyhow::Result;
use clap::Args as ClapArgs;
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
}

/// Run the `doctor` subcommand.
///
/// # Errors
///
/// Returns an error if config discovery fails.
pub async fn run(args: Args, json: bool) -> Result<()> {
    let emit_json = json || args.json;

    let env = mn_core::config::StdEnv;
    let (_, path) = mn_core::config::Config::discover(None, &env)?;
    let report = Report {
        cli: VersionInfo::current(),
        config_file: path.map(|p| p.display().to_string()),
        admin_visibility: std::env::var("MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS")
            .ok()
            .is_some_and(|v| !matches!(v.as_str(), "0" | "false" | "no")),
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
    }
    Ok(())
}

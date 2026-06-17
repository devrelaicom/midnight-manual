//! `mnm skills add` — install the advanced-search skill into detected (or
//! specified) harnesses.

use anyhow::Result;
use clap::Args as ClapArgs;
use mnm_skills::StdSkillEnv;

/// Arguments for `mnm skills add`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Comma-separated harnesses (`claude-code`, `codex`, `opencode`,
    /// `cursor`). Omit to auto-detect installed harnesses.
    #[arg(long)]
    pub harness: Option<String>,
    /// Install scope: `user` (all your projects) or `project` (this repo).
    #[arg(long, default_value = "user")]
    pub scope: String,
}

/// Run `mnm skills add`.
///
/// # Errors
///
/// Bad flags, no harness detected, or a filesystem write failure.
pub fn run(args: &Args, json: bool) -> Result<()> {
    let targets = super::parse_harnesses(args.harness.as_deref())?;
    let scope = super::parse_scope(&args.scope)?;
    let report = mnm_skills::install(targets.as_deref(), scope, &StdSkillEnv)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Installed `{}` at {} scope:\n", report.skill_name, report.scope);
    for h in &report.installed {
        let verb = match h.action {
            mnm_skills::InstallAction::Created => "created",
            mnm_skills::InstallAction::Updated => "updated",
            mnm_skills::InstallAction::Unchanged => "already current",
        };
        println!("  {} — {verb}", h.harness);
        println!("    path:   {}", h.path.display());
        println!("    reload: {}\n", h.reload_step);
    }
    if !report.not_detected.is_empty() {
        println!("Not detected (skipped): {}", report.not_detected.join(", "));
        println!("Force one with: mnm skills add --harness <name>");
        println!();
    }
    Ok(())
}

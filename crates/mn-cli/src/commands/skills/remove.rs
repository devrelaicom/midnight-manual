//! `mnm skills remove` — delete the advanced-search skill.

use anyhow::Result;
use clap::Args as ClapArgs;
use mn_skills::StdSkillEnv;

/// Arguments for `mnm skills remove`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Comma-separated harnesses to remove from. Omit to auto-detect.
    #[arg(long)]
    pub harness: Option<String>,
    /// Scope: `user` or `project`.
    #[arg(long, default_value = "user")]
    pub scope: String,
}

/// Run `mnm skills remove`.
///
/// # Errors
///
/// Bad flags, no harness detected, or a filesystem delete failure.
pub fn run(args: &Args, json: bool) -> Result<()> {
    let targets = super::parse_harnesses(args.harness.as_deref())?;
    let scope = super::parse_scope(&args.scope)?;
    let report = mn_skills::remove(targets.as_deref(), scope, &StdSkillEnv)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Removed `{}` at {} scope:\n", report.skill_name, report.scope);
    for h in &report.removed {
        let verb = match h.action {
            mn_skills::RemoveAction::Removed => "removed",
            mn_skills::RemoveAction::Absent => "not installed",
        };
        println!("  {} — {verb} ({})", h.harness, h.path.display());
    }
    Ok(())
}

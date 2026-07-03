//! `mnm skills remove` — delete bundled skills. Removes all bundled skills by
//! default; `--skill <name>` (repeatable) or `--all` select which.

use anyhow::Result;
use clap::Args as ClapArgs;
use mnm_skills::StdSkillEnv;

/// Arguments for `mnm skills remove`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Skill to remove (repeatable). Omit to remove every bundled skill.
    #[arg(long = "skill", value_name = "NAME", conflicts_with = "all")]
    pub skill: Vec<String>,
    /// Remove every bundled skill (the default when no `--skill` is given).
    // Consumed by clap for `conflicts_with`; `run` never reads it — resolution
    // keys off an empty `skill` selector, which `mnm_skills::select` maps to all.
    #[arg(long)]
    pub all: bool,
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
/// Bad flags, unknown skill, no harness detected, or a filesystem delete failure.
pub fn run(args: &Args, json: bool) -> Result<()> {
    let targets = super::parse_harnesses(args.harness.as_deref())?;
    let scope = super::parse_scope(&args.scope)?;
    let skills = super::parse_skills(&args.skill)?;
    let report = mnm_skills::remove(targets.as_deref(), &skills, scope, &StdSkillEnv)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Removed at {} scope:\n", report.scope);
    for skill in &report.skills {
        println!("`{}`:", skill.skill_name);
        for h in &skill.removed {
            let verb = match h.action {
                mnm_skills::RemoveAction::Removed => "removed",
                mnm_skills::RemoveAction::Absent => "not installed",
            };
            println!("  {} — {verb} ({})", h.harness, h.path.display());
        }
        println!();
    }
    Ok(())
}

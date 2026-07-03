//! `mnm skills add` — install bundled skills into detected (or specified)
//! harnesses. Installs all bundled skills by default; `--skill <name>` (repeatable)
//! or `--all` select which.

use anyhow::Result;
use clap::Args as ClapArgs;
use mnm_skills::StdSkillEnv;

/// Arguments for `mnm skills add`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Skill to install (repeatable). Omit to install every bundled skill.
    #[arg(long = "skill", value_name = "NAME", conflicts_with = "all")]
    pub skill: Vec<String>,
    /// Install every bundled skill (the default when no `--skill` is given).
    // Consumed by clap for `conflicts_with`; `run` never reads it — resolution
    // keys off an empty `skill` selector, which `mnm_skills::select` maps to all.
    #[arg(long)]
    pub all: bool,
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
/// Bad flags, unknown skill, no harness detected, or a filesystem write failure.
pub fn run(args: &Args, json: bool) -> Result<()> {
    let targets = super::parse_harnesses(args.harness.as_deref())?;
    let scope = super::parse_scope(&args.scope)?;
    let skills = super::parse_skills(&args.skill)?;
    let report = mnm_skills::install(targets.as_deref(), &skills, scope, &StdSkillEnv)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Installed at {} scope:\n", report.scope);
    for skill in &report.skills {
        println!("`{}`:", skill.skill_name);
        for h in &skill.installed {
            let verb = match h.action {
                mnm_skills::InstallAction::Created => "created",
                mnm_skills::InstallAction::Updated => "updated",
                mnm_skills::InstallAction::Unchanged => "already current",
            };
            println!("  {} — {verb}", h.harness);
            println!("    path:   {}", h.path.display());
            println!("    reload: {}", h.reload_step);
        }
        println!();
    }
    if !report.not_detected.is_empty() {
        println!("Not detected (skipped): {}", report.not_detected.join(", "));
        println!("Force one with: mnm skills add --harness <name>");
        println!();
    }
    Ok(())
}

//! `mnm skills status` — show install state per harness.

use anyhow::Result;
use clap::Args as ClapArgs;
use mnm_skills::StdSkillEnv;

/// Arguments for `mnm skills status`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Scope to inspect: `user` or `project`.
    #[arg(long, default_value = "user")]
    pub scope: String,
}

/// Run `mnm skills status`.
///
/// # Errors
///
/// Bad `--scope`, or an unresolvable home / cwd.
pub fn run(args: &Args, json: bool) -> Result<()> {
    let scope = super::parse_scope(&args.scope)?;
    let report = mnm_skills::status(scope, &StdSkillEnv)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("`{}` at {} scope:\n", report.skill_name, report.scope);
    println!("  {:<12}  {:<9}  {:<10}  state", "harness", "detected", "installed");
    for h in &report.harnesses {
        let state = if !h.installed {
            "—"
        } else if h.up_to_date {
            "up to date"
        } else {
            "stale"
        };
        println!(
            "  {:<12}  {:<9}  {:<10}  {}",
            h.harness,
            yes_no(h.detected),
            yes_no(h.installed),
            state
        );
    }
    Ok(())
}

const fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

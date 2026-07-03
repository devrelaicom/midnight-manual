//! `mnm skills <subcommand>` — install / inspect / remove the bundled Agent
//! Skills in the user's AI harness(es). The set of skills is registry-driven
//! (`mnm_skills::SKILLS`); `--skill <name>` (repeatable) selects specific
//! bundles and the default (or `--all`) targets every bundled skill.

use anyhow::{anyhow, Result};
use clap::{Args as ClapArgs, Subcommand};
use mnm_skills::{Harness, Scope, SkillBundle};

pub mod add;
pub mod remove;
pub mod status;

/// Skills namespace arguments.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Subcommand.
    #[command(subcommand)]
    pub cmd: SkillsCmd,
}

/// Skills subcommands.
#[derive(Debug, Subcommand)]
pub enum SkillsCmd {
    /// Install (or update) bundled skills (default: all).
    Add(add::Args),
    /// Show, per skill and harness, where each skill is installed and whether
    /// it's current.
    Status(status::Args),
    /// Remove bundled skills (default: all).
    Remove(remove::Args),
}

/// Dispatcher for the skills namespace.
///
/// # Errors
///
/// Propagates subcommand failures (bad `--harness` / `--scope`, install IO).
pub fn run(args: Args, json: bool) -> Result<()> {
    match args.cmd {
        SkillsCmd::Add(a) => add::run(&a, json),
        SkillsCmd::Status(a) => status::run(&a, json),
        SkillsCmd::Remove(a) => remove::run(&a, json),
    }
}

/// Parse the optional `--harness a,b,c` flag into harnesses. `None` (flag
/// omitted) means auto-detect; an empty string is rejected.
///
/// # Errors
///
/// Returns an error for an unknown harness id or an empty `--harness` value.
pub(super) fn parse_harnesses(raw: Option<&str>) -> Result<Option<Vec<Harness>>> {
    let Some(raw) = raw else { return Ok(None) };
    let mut out = Vec::new();
    for tok in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let h = tok.parse::<Harness>().map_err(|bad| {
            anyhow!("unknown harness `{bad}` (expected: claude-code, codex, opencode, cursor)")
        })?;
        if !out.contains(&h) {
            out.push(h);
        }
    }
    if out.is_empty() {
        return Err(anyhow!(
            "--harness was empty; give one or more of: claude-code, codex, opencode, cursor"
        ));
    }
    Ok(Some(out))
}

/// Parse the `--scope` flag (default `user`).
///
/// # Errors
///
/// Returns an error for any value other than `user` / `project`.
pub(super) fn parse_scope(raw: &str) -> Result<Scope> {
    raw.parse::<Scope>()
        .map_err(|bad| anyhow!("unknown scope `{bad}` (expected: user, project)"))
}

/// Resolve the `--skill` selector into bundles. An empty selector (neither
/// `--skill` nor with `--all`, or `--all` alone) means every bundled skill,
/// matching the MCP `install_skill` omit=all semantics.
///
/// # Errors
///
/// Returns an error naming the known skills if any `--skill` value is unknown.
pub(super) fn parse_skills(names: &[String]) -> Result<Vec<&'static SkillBundle>> {
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    mnm_skills::select(&refs).map_err(|e| anyhow!(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_harnesses_dedupes_and_validates() {
        let got = parse_harnesses(Some("claude-code,cursor,claude-code"))
            .unwrap()
            .unwrap();
        assert_eq!(got, vec![Harness::ClaudeCode, Harness::Cursor]);
    }

    #[test]
    fn parse_harnesses_none_is_autodetect() {
        assert!(parse_harnesses(None).unwrap().is_none());
    }

    #[test]
    fn parse_harnesses_rejects_unknown() {
        assert!(parse_harnesses(Some("windsurf")).is_err());
    }

    #[test]
    fn parse_harnesses_rejects_empty_string() {
        assert!(parse_harnesses(Some("")).is_err());
        assert!(parse_harnesses(Some(",,")).is_err());
    }

    #[test]
    fn parse_scope_defaults_and_rejects() {
        assert_eq!(parse_scope("user").unwrap(), Scope::User);
        assert_eq!(parse_scope("project").unwrap(), Scope::Project);
        assert!(parse_scope("global").is_err());
    }

    #[test]
    fn parse_skills_empty_is_all() {
        let all = parse_skills(&[]).unwrap();
        assert_eq!(all.len(), mnm_skills::SKILLS.len());
    }

    #[test]
    fn parse_skills_named_selects_and_dedupes() {
        let got = parse_skills(&[
            "midnight-advanced-search".to_owned(),
            "midnight-advanced-search".to_owned(),
        ])
        .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, mnm_skills::SEARCH_SKILL);
    }

    #[test]
    fn parse_skills_rejects_unknown() {
        assert!(parse_skills(&["no-such-skill".to_owned()]).is_err());
    }
}

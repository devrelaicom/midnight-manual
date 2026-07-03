//! The harness registry: the four supported AI harnesses, the two install
//! scopes, and — per (harness, scope) — the skills-root directory, detection
//! markers, and the reload instruction shown to the user.

use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Install scope, mirroring how each harness scopes skills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// All the user's projects (home-rooted).
    User,
    /// This repository only (repo-root-rooted, committed).
    Project,
}

impl Scope {
    /// Wire / display string (`"user"` or `"project"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

impl FromStr for Scope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "project" => Ok(Self::Project),
            other => Err(other.to_owned()),
        }
    }
}

/// A supported AI harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    /// Anthropic Claude Code.
    ClaudeCode,
    /// `OpenAI` Codex CLI.
    Codex,
    /// `OpenCode`.
    OpenCode,
    /// Cursor (2.4+ native Agent Skills).
    Cursor,
}

impl Harness {
    /// Every supported harness, in display order.
    pub const ALL: [Self; 4] = [Self::ClaudeCode, Self::Codex, Self::OpenCode, Self::Cursor];

    /// The stable id used on the CLI / MCP wire (`claude-code`, `codex`,
    /// `opencode`, `cursor`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
        }
    }

    /// Human-readable name for CLI output.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex CLI",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor",
        }
    }

    /// The directory holding per-skill folders for this harness at `scope`,
    /// rooted at `base` (home dir for [`Scope::User`], repo root for
    /// [`Scope::Project`]).
    #[must_use]
    pub fn skills_root(self, scope: Scope, base: &Path) -> PathBuf {
        match (self, scope) {
            (Self::ClaudeCode, _) => base.join(".claude").join("skills"),
            (Self::Codex, _) => base.join(".agents").join("skills"),
            (Self::OpenCode, Scope::User) => base.join(".config").join("opencode").join("skills"),
            (Self::OpenCode, Scope::Project) => base.join(".opencode").join("skills"),
            (Self::Cursor, _) => base.join(".cursor").join("skills"),
        }
    }

    /// The owned skill directory for `skill` (`<skills_root>/<skill>`).
    #[must_use]
    pub fn skill_dir(self, skill: &str, scope: Scope, base: &Path) -> PathBuf {
        self.skills_root(scope, base).join(skill)
    }

    /// The installed `SKILL.md` path for `skill`.
    #[must_use]
    pub fn skill_file(self, skill: &str, scope: Scope, base: &Path) -> PathBuf {
        self.skill_dir(skill, scope, base).join("SKILL.md")
    }

    /// Detection markers for this harness at `scope`, rooted at `base`. The
    /// harness is considered present if ANY marker exists. Codex at
    /// [`Scope::Project`] additionally checks for `AGENTS.md` (created at the
    /// repo root) as a light signal the project uses Codex.
    #[must_use]
    pub fn markers(self, scope: Scope, base: &Path) -> Vec<PathBuf> {
        match (self, scope) {
            (Self::ClaudeCode, _) => vec![base.join(".claude")],
            (Self::Codex, Scope::User) => vec![base.join(".codex"), base.join(".agents")],
            (Self::Codex, Scope::Project) => {
                vec![
                    base.join(".codex"),
                    base.join(".agents"),
                    base.join("AGENTS.md"),
                ]
            }
            (Self::OpenCode, Scope::User) => vec![base.join(".config").join("opencode")],
            (Self::OpenCode, Scope::Project) => vec![base.join(".opencode")],
            (Self::Cursor, _) => vec![base.join(".cursor")],
        }
    }

    /// The per-harness "reload your skills" instruction to print / relay.
    #[must_use]
    pub const fn reload_step(self) -> &'static str {
        match self {
            Self::ClaudeCode => {
                "Claude Code auto-discovers skills. If its skills directory was created just now, \
                 restart Claude Code; otherwise it loads live. Verify by asking \"what skills are available?\"."
            }
            Self::Codex => {
                "Codex auto-detects new skills. If it doesn't appear, restart Codex (or run /skills to list)."
            }
            Self::OpenCode => "Restart your OpenCode session to load the new skill.",
            Self::Cursor => "Cursor discovers skills on startup. Restart Cursor to load the new skill.",
        }
    }
}

impl FromStr for Harness {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude-code" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::OpenCode),
            "cursor" => Ok(Self::Cursor),
            other => Err(other.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn id_round_trips() {
        for h in Harness::ALL {
            assert_eq!(Harness::from_str(h.id()), Ok(h));
        }
    }

    #[test]
    fn unknown_id_is_err() {
        // The error echoes the bad token so callers can format their own message.
        assert_eq!(Harness::from_str("windsurf").unwrap_err(), "windsurf");
    }

    #[test]
    fn user_paths_match_verified_matrix() {
        let home = Path::new("/home/u");
        let s = crate::SEARCH_SKILL;
        assert_eq!(
            Harness::ClaudeCode.skill_file(s, Scope::User, home),
            Path::new("/home/u/.claude/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::Codex.skill_file(s, Scope::User, home),
            Path::new("/home/u/.agents/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::OpenCode.skill_file(s, Scope::User, home),
            Path::new("/home/u/.config/opencode/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::Cursor.skill_file(s, Scope::User, home),
            Path::new("/home/u/.cursor/skills/midnight-advanced-search/SKILL.md")
        );
    }

    #[test]
    fn project_paths_match_verified_matrix() {
        let root = Path::new("/repo");
        let s = crate::SEARCH_SKILL;
        assert_eq!(
            Harness::ClaudeCode.skill_file(s, Scope::Project, root),
            Path::new("/repo/.claude/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::Codex.skill_file(s, Scope::Project, root),
            Path::new("/repo/.agents/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::OpenCode.skill_file(s, Scope::Project, root),
            Path::new("/repo/.opencode/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::Cursor.skill_file(s, Scope::Project, root),
            Path::new("/repo/.cursor/skills/midnight-advanced-search/SKILL.md")
        );
    }

    #[test]
    fn skill_dir_uses_the_named_skill_folder() {
        let home = Path::new("/home/u");
        // A hypothetical second skill resolves to its own owned dir — proof the
        // path layer is skill-parameterized, not hard-bound to one name.
        assert_eq!(
            Harness::ClaudeCode.skill_dir("another-skill", Scope::User, home),
            Path::new("/home/u/.claude/skills/another-skill")
        );
    }

    #[test]
    fn scope_round_trips() {
        assert_eq!(Scope::from_str("user"), Ok(Scope::User));
        assert_eq!(Scope::from_str("project"), Ok(Scope::Project));
        assert!(Scope::from_str("global").is_err());
    }

    #[test]
    fn markers_cover_codex_and_opencode_asymmetry() {
        let base = Path::new("/b");
        // Codex: 2 markers at user scope, 3 (incl. AGENTS.md) at project scope.
        let user = Harness::Codex.markers(Scope::User, base);
        assert_eq!(user.len(), 2);
        assert!(user.contains(&base.join(".codex")) && user.contains(&base.join(".agents")));
        let proj = Harness::Codex.markers(Scope::Project, base);
        assert_eq!(proj.len(), 3);
        assert!(proj.contains(&base.join("AGENTS.md")));
        // OpenCode: different root per scope.
        assert_eq!(
            Harness::OpenCode.markers(Scope::User, base),
            vec![base.join(".config").join("opencode")]
        );
        assert_eq!(Harness::OpenCode.markers(Scope::Project, base), vec![base.join(".opencode")]);
    }
}

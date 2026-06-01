//! Base-directory resolution (home for user scope, repo root for project
//! scope) and marker-based harness detection.

use std::path::{Path, PathBuf};

use crate::error::SkillError;
use crate::harness::{Harness, Scope};
use crate::SkillEnv;

/// The directory all paths for `scope` are rooted at: the home dir for
/// [`Scope::User`], the repository root (walked up from cwd) for
/// [`Scope::Project`].
///
/// # Errors
///
/// [`SkillError::NoHome`] / [`SkillError::NoCwd`] when the environment can't
/// supply the anchor.
pub fn base_dir(scope: Scope, env: &impl SkillEnv) -> Result<PathBuf, SkillError> {
    match scope {
        Scope::User => env.home_dir().ok_or(SkillError::NoHome),
        Scope::Project => {
            let cwd = env.current_dir().ok_or(SkillError::NoCwd)?;
            Ok(repo_root(&cwd))
        }
    }
}

/// Walk up from `start` to the nearest ancestor containing a `.git` entry.
/// Falls back to `start` itself when no `.git` is found.
fn repo_root(start: &Path) -> PathBuf {
    let mut cur: &Path = start;
    loop {
        if cur.join(".git").exists() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => return start.to_path_buf(),
        }
    }
}

/// Detect which harnesses are present at `scope`. A harness is present when any
/// of its markers exists under the resolved base dir.
///
/// # Errors
///
/// Propagates [`base_dir`] errors.
pub fn detect(scope: Scope, env: &impl SkillEnv) -> Result<Vec<Harness>, SkillError> {
    let base = base_dir(scope, env)?;
    Ok(Harness::ALL
        .into_iter()
        .filter(|h| h.markers(scope, &base).iter().any(|m| m.exists()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct FakeEnv {
        home: PathBuf,
        cwd: PathBuf,
    }
    impl SkillEnv for FakeEnv {
        fn home_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
        fn current_dir(&self) -> Option<PathBuf> {
            Some(self.cwd.clone())
        }
    }

    #[test]
    fn detect_user_scope_by_marker() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".cursor")).unwrap();
        let env = FakeEnv { home: home.clone(), cwd: home };
        let mut got = detect(Scope::User, &env).unwrap();
        got.sort_by_key(|h| h.id());
        assert_eq!(got, vec![Harness::ClaudeCode, Harness::Cursor]);
    }

    #[test]
    fn detect_none_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv {
            home: tmp.path().to_path_buf(),
            cwd: tmp.path().to_path_buf(),
        };
        assert!(detect(Scope::User, &env).unwrap().is_empty());
    }

    #[test]
    fn project_base_walks_up_to_git_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        let nested = root.join("crates").join("x");
        fs::create_dir_all(&nested).unwrap();
        let env = FakeEnv {
            home: root.to_path_buf(),
            cwd: nested,
        };
        assert_eq!(base_dir(Scope::Project, &env).unwrap(), root);
    }

    #[test]
    fn project_base_falls_back_to_cwd_without_git() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().join("loose");
        fs::create_dir_all(&cwd).unwrap();
        let env = FakeEnv {
            home: tmp.path().to_path_buf(),
            cwd: cwd.clone(),
        };
        assert_eq!(base_dir(Scope::Project, &env).unwrap(), cwd);
    }

    #[test]
    fn detect_codex_project_via_agents_md() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("AGENTS.md"), "x").unwrap();
        let env = FakeEnv {
            home: root.to_path_buf(),
            cwd: root.to_path_buf(),
        };
        assert_eq!(detect(Scope::Project, &env).unwrap(), vec![Harness::Codex]);
    }
}

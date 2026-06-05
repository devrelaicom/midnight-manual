//! Idempotent install / remove / status over the owned
//! `midnight-advanced-search/` directory.

use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use crate::detect::{base_dir, detect};
use crate::error::SkillError;
use crate::harness::{Harness, Scope};
use crate::{skill_files, SkillEnv, SKILL_NAME};

/// What an install did to a single harness's owned dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallAction {
    /// The skill file did not exist; it was written.
    Created,
    /// The skill file existed with different content; it was overwritten.
    Updated,
    /// The skill file existed with byte-identical content; no write.
    Unchanged,
}

/// Per-harness install outcome.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessInstall {
    /// Harness id (`claude-code`, …).
    pub harness: String,
    /// Scope (`user` / `project`).
    pub scope: String,
    /// The `SKILL.md` path written.
    pub path: PathBuf,
    /// What happened.
    pub action: InstallAction,
    /// The "reload your skills" instruction for this harness.
    pub reload_step: String,
}

/// Result of an [`install`] call.
#[derive(Debug, Clone, Serialize)]
pub struct InstallReport {
    /// The installed skill's name.
    pub skill_name: String,
    /// Scope all writes targeted.
    pub scope: String,
    /// One entry per harness written.
    pub installed: Vec<HarnessInstall>,
    /// Harness ids probed but not detected (empty when `--harness` was forced).
    pub not_detected: Vec<String>,
}

/// Per-harness removal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveAction {
    /// The owned dir existed and was deleted.
    Removed,
    /// The owned dir did not exist.
    Absent,
}

/// One harness's removal result.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessRemove {
    /// Harness id.
    pub harness: String,
    /// Scope.
    pub scope: String,
    /// The owned skill **directory** deleted (not the `SKILL.md` file), e.g.
    /// `.../skills/midnight-advanced-search/`. Contrast with
    /// [`HarnessInstall::path`], which is the file.
    pub path: PathBuf,
    /// What happened.
    pub action: RemoveAction,
}

/// Result of a [`remove`] call.
#[derive(Debug, Clone, Serialize)]
pub struct RemoveReport {
    /// The skill's name.
    pub skill_name: String,
    /// Scope.
    pub scope: String,
    /// One entry per harness targeted.
    pub removed: Vec<HarnessRemove>,
}

/// One harness's status at a scope.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessStatus {
    /// Harness id.
    pub harness: String,
    /// Scope.
    pub scope: String,
    /// Whether the harness's marker is present.
    pub detected: bool,
    /// Whether our `SKILL.md` is installed.
    pub installed: bool,
    /// Whether the installed copy is byte-identical to the embedded skill.
    /// `false` when not installed or when it differs (stale / user-edited).
    pub up_to_date: bool,
    /// The resolved `SKILL.md` path.
    pub path: PathBuf,
}

/// Result of a [`status`] call.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    /// The skill's name.
    pub skill_name: String,
    /// Scope.
    pub scope: String,
    /// One entry per supported harness.
    pub harnesses: Vec<HarnessStatus>,
}

/// Resolve which harnesses to act on:
/// - `Some(list)` → exactly those (forced; detection skipped, `not_detected`
///   empty).
/// - `None` → auto-detect; errors [`SkillError::NoHarnessDetected`] if none.
///
/// Returns the targets plus the ids that were probed-but-absent (only
/// meaningful in the auto-detect branch).
fn resolve_targets(
    explicit: Option<&[Harness]>,
    scope: Scope,
    env: &impl SkillEnv,
) -> Result<(Vec<Harness>, Vec<String>), SkillError> {
    if let Some(list) = explicit {
        return Ok((list.to_vec(), Vec::new()));
    }
    let detected = detect(scope, env)?;
    if detected.is_empty() {
        return Err(SkillError::NoHarnessDetected {
            scope: scope.as_str().to_owned(),
            probed: Harness::ALL
                .iter()
                .map(|h| h.id())
                .collect::<Vec<_>>()
                .join(", "),
        });
    }
    let not_detected = Harness::ALL
        .into_iter()
        .filter(|h| !detected.contains(h))
        .map(|h| h.id().to_owned())
        .collect();
    Ok((detected, not_detected))
}

/// Install the embedded skill for `explicit` harnesses (or auto-detected ones),
/// idempotently, at `scope`.
///
/// # Errors
///
/// [`SkillError::NoHarnessDetected`] when auto-detect finds nothing,
/// path-resolution errors, or [`SkillError::Io`] on a failed write.
pub fn install(
    explicit: Option<&[Harness]>,
    scope: Scope,
    env: &impl SkillEnv,
) -> Result<InstallReport, SkillError> {
    let base = base_dir(scope, env)?;
    let (targets, not_detected) = resolve_targets(explicit, scope, env)?;
    let files = skill_files();
    let mut installed = Vec::with_capacity(targets.len());
    for h in targets {
        let dir = h.skill_dir(scope, &base);
        let dir_existed = dir.exists();
        let mut changed = false;

        // Write every manifest file, creating parent dirs as needed.
        for &(rel, body) in files {
            let file = join_rel(&dir, rel);
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent).map_err(|source| SkillError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            let up_to_date = fs::read_to_string(&file)
                .map(|c| c == body)
                .unwrap_or(false);
            if !up_to_date {
                write_file(&file, body)?;
                changed = true;
            }
        }

        // Prune any file in the owned dir that the manifest does not ship.
        if dir_existed && prune_orphans(&dir, files)? {
            changed = true;
        }

        let action = if !dir_existed {
            InstallAction::Created
        } else if changed {
            InstallAction::Updated
        } else {
            InstallAction::Unchanged
        };

        installed.push(HarnessInstall {
            harness: h.id().to_owned(),
            scope: scope.as_str().to_owned(),
            path: h.skill_file(scope, &base),
            action,
            reload_step: h.reload_step().to_owned(),
        });
    }
    Ok(InstallReport {
        skill_name: SKILL_NAME.to_owned(),
        scope: scope.as_str().to_owned(),
        installed,
        not_detected,
    })
}

/// Write `body` to `path`, mapping any io failure to [`SkillError::Io`] with
/// the path attached.
fn write_file(path: &std::path::Path, body: &str) -> Result<(), SkillError> {
    fs::write(path, body).map_err(|source| SkillError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Join a manifest-relative path (which uses `/` separators) onto `dir`
/// component-by-component, so it is correct on every platform.
fn join_rel(dir: &std::path::Path, rel: &str) -> PathBuf {
    let mut p = dir.to_path_buf();
    for seg in rel.split('/') {
        p.push(seg);
    }
    p
}

/// Delete any regular file under `dir` whose path is not shipped by `files`.
/// Scoped strictly to the owned skill dir; leaves directories in place. Returns
/// `true` if anything was removed.
fn prune_orphans(dir: &std::path::Path, files: &[(&str, &str)]) -> Result<bool, SkillError> {
    use std::collections::HashSet;
    let owned: HashSet<PathBuf> = files.iter().map(|&(rel, _)| join_rel(dir, rel)).collect();
    let mut removed = false;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries =
            fs::read_dir(&d).map_err(|source| SkillError::Io { path: d.clone(), source })?;
        for entry in entries {
            let entry = entry.map_err(|source| SkillError::Io { path: d.clone(), source })?;
            let path = entry.path();
            let ft = entry
                .file_type()
                .map_err(|source| SkillError::Io { path: path.clone(), source })?;
            if ft.is_dir() {
                stack.push(path);
            } else if !owned.contains(&path) {
                fs::remove_file(&path)
                    .map_err(|source| SkillError::Io { path: path.clone(), source })?;
                removed = true;
            }
        }
    }
    Ok(removed)
}

/// Remove the owned skill dir for `explicit` harnesses (or auto-detected ones)
/// at `scope`.
///
/// # Errors
///
/// [`SkillError::NoHarnessDetected`] when auto-detect finds nothing,
/// [`SkillError::NoHome`] / [`SkillError::NoCwd`] on an unresolvable base dir,
/// or [`SkillError::Io`] on a failed delete.
pub fn remove(
    explicit: Option<&[Harness]>,
    scope: Scope,
    env: &impl SkillEnv,
) -> Result<RemoveReport, SkillError> {
    let base = base_dir(scope, env)?;
    let (targets, _) = resolve_targets(explicit, scope, env)?;
    let mut removed = Vec::with_capacity(targets.len());
    for h in targets {
        let dir = h.skill_dir(scope, &base);
        let action = if dir.exists() {
            fs::remove_dir_all(&dir)
                .map_err(|source| SkillError::Io { path: dir.clone(), source })?;
            RemoveAction::Removed
        } else {
            RemoveAction::Absent
        };
        removed.push(HarnessRemove {
            harness: h.id().to_owned(),
            scope: scope.as_str().to_owned(),
            path: dir,
            action,
        });
    }
    Ok(RemoveReport {
        skill_name: SKILL_NAME.to_owned(),
        scope: scope.as_str().to_owned(),
        removed,
    })
}

/// Report detection + install state for every supported harness at `scope`.
/// Never errors on "nothing detected" — only on an unresolvable base dir.
///
/// # Errors
///
/// Path-resolution errors only.
pub fn status(scope: Scope, env: &impl SkillEnv) -> Result<StatusReport, SkillError> {
    let base = base_dir(scope, env)?;
    let files = skill_files();
    let harnesses = Harness::ALL
        .into_iter()
        .map(|h| {
            let file = h.skill_file(scope, &base);
            let dir = h.skill_dir(scope, &base);
            let detected = h.markers(scope, &base).iter().any(|m| m.exists());
            // `installed` keys on the primary file (SKILL.md); `up_to_date`
            // requires every bundled file to be present and byte-identical.
            let installed = file.exists();
            let up_to_date = installed
                && files.iter().all(|&(rel, body)| {
                    fs::read_to_string(join_rel(&dir, rel))
                        .map(|got| got == body)
                        .unwrap_or(false)
                });
            HarnessStatus {
                harness: h.id().to_owned(),
                scope: scope.as_str().to_owned(),
                detected,
                installed,
                up_to_date,
                path: file,
            }
        })
        .collect();
    Ok(StatusReport {
        skill_name: SKILL_NAME.to_owned(),
        scope: scope.as_str().to_owned(),
        harnesses,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct FakeEnv {
        home: PathBuf,
    }
    impl SkillEnv for FakeEnv {
        fn home_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
        fn current_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
    }

    fn env_with_marker(harness: Harness) -> (TempDir, FakeEnv) {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        if let Some(m) = harness.markers(Scope::User, &home).into_iter().next() {
            std::fs::create_dir_all(&m).unwrap();
        }
        let env = FakeEnv { home };
        (tmp, env)
    }

    #[test]
    fn install_then_reinstall_is_idempotent() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let first = install(None, Scope::User, &env).unwrap();
        assert_eq!(first.installed.len(), 1);
        assert_eq!(first.installed[0].action, InstallAction::Created);
        assert!(first.installed[0].path.exists());

        let second = install(None, Scope::User, &env).unwrap();
        assert_eq!(second.installed[0].action, InstallAction::Unchanged);
    }

    #[test]
    fn install_overwrites_stale_content_as_updated() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, Scope::User, &env).unwrap();
        let path = report.installed[0].path.clone();
        std::fs::write(&path, "stale body").unwrap();

        let again = install(None, Scope::User, &env).unwrap();
        assert_eq!(again.installed[0].action, InstallAction::Updated);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), crate::skill_markdown());
    }

    #[test]
    fn explicit_harness_forces_install_even_when_undetected() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf() };
        let report = install(Some(&[Harness::Cursor]), Scope::User, &env).unwrap();
        assert_eq!(report.installed.len(), 1);
        assert_eq!(report.installed[0].harness, "cursor");
        assert!(report.not_detected.is_empty());
    }

    #[test]
    fn autodetect_with_no_harness_errors() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf() };
        let err = install(None, Scope::User, &env).unwrap_err();
        assert!(matches!(err, SkillError::NoHarnessDetected { .. }));
    }

    #[test]
    fn not_detected_lists_absent_harnesses_on_autodetect() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, Scope::User, &env).unwrap();
        assert_eq!(report.installed.len(), 1);
        let mut nd = report.not_detected;
        nd.sort();
        let mut expected: Vec<String> = Harness::ALL
            .into_iter()
            .filter(|h| *h != Harness::ClaudeCode)
            .map(|h| h.id().to_owned())
            .collect();
        expected.sort();
        assert_eq!(nd, expected);
    }

    #[test]
    fn status_reports_installed_and_stale() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        install(None, Scope::User, &env).unwrap();
        let st = status(Scope::User, &env).unwrap();
        let cc = st
            .harnesses
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        assert!(cc.detected && cc.installed && cc.up_to_date);
        let cursor = st.harnesses.iter().find(|h| h.harness == "cursor").unwrap();
        assert!(!cursor.detected && !cursor.installed && !cursor.up_to_date);

        std::fs::write(&cc.path, "stale").unwrap();
        let st2 = status(Scope::User, &env).unwrap();
        let cc2 = st2
            .harnesses
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        assert!(cc2.installed && !cc2.up_to_date);
    }

    #[test]
    fn status_not_up_to_date_when_a_reference_is_stale() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        install(None, Scope::User, &env).unwrap();
        // Make ONLY a reference stale; SKILL.md is untouched.
        let dir = Harness::ClaudeCode.skill_dir(Scope::User, &env.home);
        std::fs::write(dir.join("references").join("advanced-techniques.md"), "stale").unwrap();

        let st = status(Scope::User, &env).unwrap();
        let cc = st
            .harnesses
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        assert!(cc.installed, "SKILL.md still present → installed");
        assert!(!cc.up_to_date, "a stale reference must make up_to_date false");
    }

    #[test]
    fn remove_deletes_then_reports_absent() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        install(None, Scope::User, &env).unwrap();
        let r1 = remove(Some(&[Harness::ClaudeCode]), Scope::User, &env).unwrap();
        assert_eq!(r1.removed[0].action, RemoveAction::Removed);
        assert!(!r1.removed[0].path.exists());
        let r2 = remove(Some(&[Harness::ClaudeCode]), Scope::User, &env).unwrap();
        assert_eq!(r2.removed[0].action, RemoveAction::Absent);
    }

    #[test]
    fn report_serializes_to_expected_json_shape() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, Scope::User, &env).unwrap();
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["skill_name"], "midnight-advanced-search");
        assert_eq!(v["scope"], "user");
        assert_eq!(v["installed"][0]["harness"], "claude-code");
        assert_eq!(v["installed"][0]["action"], "created");
        assert!(v["installed"][0]["reload_step"].is_string());
    }

    #[test]
    fn install_propagates_non_notfound_read_error() {
        // A directory where SKILL.md is itself a directory makes read_to_string
        // fail with something other than NotFound; install must surface Io, not
        // mis-report Created.
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let dir = Harness::ClaudeCode.skill_dir(Scope::User, &env.home);
        std::fs::create_dir_all(dir.join("SKILL.md")).unwrap(); // SKILL.md is a dir
        let err = install(None, Scope::User, &env).unwrap_err();
        assert!(matches!(err, SkillError::Io { .. }));
    }

    #[test]
    fn install_writes_every_bundle_file() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, Scope::User, &env).unwrap();
        let dir = report.installed[0].path.parent().unwrap().to_path_buf();
        for &(rel, body) in crate::skill_files() {
            let mut p = dir.clone();
            for seg in rel.split('/') {
                p.push(seg);
            }
            assert!(p.exists(), "missing bundled file {rel}");
            assert_eq!(std::fs::read_to_string(&p).unwrap(), body, "{rel} content mismatch");
        }
    }

    #[test]
    fn reinstall_prunes_orphans_and_reports_updated() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, Scope::User, &env).unwrap();
        let dir = report.installed[0].path.parent().unwrap().to_path_buf();
        // Drop a stray file at the root and inside references/.
        std::fs::write(dir.join("stray.md"), "junk").unwrap();
        std::fs::write(dir.join("references").join("orphan.md"), "junk").unwrap();

        let again = install(None, Scope::User, &env).unwrap();
        assert_eq!(again.installed[0].action, InstallAction::Updated, "prune must mark Updated");
        assert!(!dir.join("stray.md").exists(), "root orphan not pruned");
        assert!(!dir.join("references").join("orphan.md").exists(), "nested orphan not pruned");
        // Manifest files survive the prune.
        assert!(dir.join("SKILL.md").exists());
        assert!(dir.join("references").join("filters-and-modes.md").exists());
    }

    #[test]
    fn install_project_scope_writes_under_repo_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        // FakeEnv.current_dir() == home == root; base_dir(Project) walks to .git at root.
        let env = FakeEnv { home: root.to_path_buf() };
        let report = install(None, Scope::Project, &env).unwrap();
        assert_eq!(report.scope, "project");
        let cc = report
            .installed
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        assert_eq!(cc.path, root.join(".claude/skills/midnight-advanced-search/SKILL.md"));
        assert!(cc.path.exists());
    }
}

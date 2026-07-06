//! Idempotent install / remove / status over each selected bundle's owned
//! `<skills_root>/<skill-name>/` directory.
//!
//! Every operation is per bundle: the reports carry a per-skill breakdown, and
//! `status` is a per-skill × per-harness matrix.

use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use crate::detect::{base_dir, detect};
use crate::error::SkillError;
use crate::harness::{Harness, Scope};
use crate::{SkillBundle, SkillEnv, SKILLS};

/// What an install did to a single harness's owned dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallAction {
    /// The owned skill dir did not exist; the whole bundle was written.
    Created,
    /// The dir existed; at least one bundled file was (re)written, or an orphan
    /// was pruned.
    Updated,
    /// The dir existed with every bundled file byte-identical and no orphans; no
    /// write.
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
    /// True when a pre-existing **symlink** at the owned skill-dir path was
    /// dropped and replaced with a real directory before writing (its target
    /// was left untouched). Surfaced so the replacement is never silent — a user
    /// who intentionally symlinked the dir gets a signal instead of losing it
    /// indistinguishably from a first-time `Created`.
    pub replaced_symlink: bool,
    /// The "reload your skills" instruction for this harness.
    pub reload_step: String,
}

/// One skill's install outcome across the targeted harnesses.
#[derive(Debug, Clone, Serialize)]
pub struct SkillInstall {
    /// The installed skill's name.
    pub skill_name: String,
    /// One entry per harness written.
    pub installed: Vec<HarnessInstall>,
}

/// Result of an [`install`] call: which harnesses were targeted (shared across
/// skills) plus a per-skill breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct InstallReport {
    /// Scope all writes targeted.
    pub scope: String,
    /// Harness ids written to (forced or auto-detected).
    pub detected: Vec<String>,
    /// Harness ids probed but not detected (empty when harnesses were forced).
    pub not_detected: Vec<String>,
    /// One entry per selected skill.
    pub skills: Vec<SkillInstall>,
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

/// One skill's removal outcome across the targeted harnesses.
#[derive(Debug, Clone, Serialize)]
pub struct SkillRemove {
    /// The skill's name.
    pub skill_name: String,
    /// One entry per harness targeted.
    pub removed: Vec<HarnessRemove>,
}

/// Result of a [`remove`] call.
#[derive(Debug, Clone, Serialize)]
pub struct RemoveReport {
    /// Scope.
    pub scope: String,
    /// One entry per selected skill.
    pub skills: Vec<SkillRemove>,
}

/// One harness's status for one skill at a scope.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessStatus {
    /// Harness id.
    pub harness: String,
    /// Scope.
    pub scope: String,
    /// Whether the harness's marker is present.
    pub detected: bool,
    /// Whether this skill's `SKILL.md` is installed.
    pub installed: bool,
    /// Whether the installed copy is byte-identical to the embedded bundle.
    /// `false` when not installed or when it differs (stale / user-edited).
    pub up_to_date: bool,
    /// The resolved `SKILL.md` path.
    pub path: PathBuf,
}

/// One skill's status across every supported harness.
#[derive(Debug, Clone, Serialize)]
pub struct SkillStatus {
    /// The skill's name.
    pub skill_name: String,
    /// One entry per supported harness.
    pub harnesses: Vec<HarnessStatus>,
}

/// Result of a [`status`] call: the full per-skill × per-harness matrix.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    /// Scope.
    pub scope: String,
    /// One entry per bundled skill.
    pub skills: Vec<SkillStatus>,
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

/// Install each `skill` bundle for `explicit` harnesses (or auto-detected ones),
/// idempotently, at `scope`.
///
/// Pass the bundles from [`crate::select`] (an empty selector means all). Each
/// bundle is written into its own `<skills_root>/<skill-name>/` directory.
///
/// # Errors
///
/// [`SkillError::NoHarnessDetected`] when auto-detect finds nothing,
/// path-resolution errors, or [`SkillError::Io`] on a failed write.
pub fn install(
    explicit: Option<&[Harness]>,
    skills: &[&SkillBundle],
    scope: Scope,
    env: &impl SkillEnv,
) -> Result<InstallReport, SkillError> {
    let base = base_dir(scope, env)?;
    let (targets, not_detected) = resolve_targets(explicit, scope, env)?;
    let detected: Vec<String> = targets.iter().map(|h| h.id().to_owned()).collect();

    let mut skill_reports = Vec::with_capacity(skills.len());
    for bundle in skills {
        let mut installed = Vec::with_capacity(targets.len());
        for &h in &targets {
            let dir = h.skill_dir(bundle.name, scope, &base);

            // Materialise the owned dir as a *real* directory, fail-closed: a
            // `create_dir_all` here would silently follow a symlink planted at
            // the leaf and write the bundle into foreign storage. `ensure_owned_dir`
            // creates it with the non-recursive `create_dir` (which never follows
            // the final symlink) and refuses anything that is not our own dir.
            let owned = ensure_owned_dir(&dir)?;
            let dir_existed = matches!(owned, OwnedDir::Existed);
            let replaced_symlink = matches!(owned, OwnedDir::Created { replaced_symlink: true });
            let mut changed = replaced_symlink;

            // Write every manifest file. The leaf is now a confirmed-real owned
            // dir, so only strictly-nested parents (e.g. `references/`) still need
            // creating — and under a real owned dir there is no pre-existing node
            // to hijack, so `create_dir_all` is safe for those.
            for &(rel, body) in bundle.files {
                let file = join_rel(&dir, rel);
                if let Some(parent) = file.parent() {
                    if parent != dir {
                        fs::create_dir_all(parent).map_err(|source| SkillError::Io {
                            path: parent.to_path_buf(),
                            source,
                        })?;
                    }
                }
                let up_to_date = fs::read_to_string(&file)
                    .map(|c| c == body)
                    .unwrap_or(false);
                if !up_to_date {
                    write_file(&file, body)?;
                    changed = true;
                }
            }

            // Prune any file in the owned dir that this bundle does not ship.
            if dir_existed && prune_orphans(&dir, bundle.files)? {
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
                path: h.skill_file(bundle.name, scope, &base),
                action,
                replaced_symlink,
                reload_step: h.reload_step().to_owned(),
            });
        }
        skill_reports.push(SkillInstall {
            skill_name: bundle.name.to_owned(),
            installed,
        });
    }

    Ok(InstallReport {
        scope: scope.as_str().to_owned(),
        detected,
        not_detected,
        skills: skill_reports,
    })
}

/// Outcome of materialising a bundle's owned skill dir as a real directory.
enum OwnedDir {
    /// The dir was created fresh. `replaced_symlink` is true when a pre-existing
    /// symlink at the path was dropped (its target left intact) to make room.
    Created {
        /// Whether a symlink squatting the owned-dir path was replaced.
        replaced_symlink: bool,
    },
    /// A real directory already existed — the idempotent re-install / update case.
    Existed,
}

/// Materialise `dir` as a *real* owned directory, fail-closed against a symlink
/// planted (or re-planted) at the path.
///
/// The leaf is created with the non-recursive [`fs::create_dir`] (`mkdir`),
/// which — unlike [`fs::create_dir_all`] — never follows a symlink at the final
/// component: it returns `AlreadyExists` for a symlink, file, or dir alike. So a
/// link left at `<skills_root>/<skill>` cannot redirect the write into foreign
/// storage the way `create_dir_all` (which resolves an existing symlink via
/// `is_dir()` and treats it as "already there") silently would.
///
/// On `AlreadyExists` we re-stat with [`fs::symlink_metadata`] (no follow):
/// * a real directory → an ordinary re-install, proceed;
/// * a symlink → drop the link (never its target) and re-create the real dir. A
///   second `AlreadyExists` means an attacker re-planted in the race window, so
///   we fail closed rather than follow it;
/// * anything else (a regular file, a fifo, …) → fail closed.
///
/// The parent `<skills_root>` is not the attack surface (the leaf is), so it is
/// created recursively to keep a first-ever install working.
///
/// On Windows, `remove_file` fails on a *directory* symlink, which degrades to a
/// fail-closed [`SkillError::Io`] rather than a clobber — still safe.
fn ensure_owned_dir(dir: &std::path::Path) -> Result<OwnedDir, SkillError> {
    if let Some(parent) = dir.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| SkillError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    match fs::create_dir(dir) {
        Ok(()) => Ok(OwnedDir::Created { replaced_symlink: false }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let ft = fs::symlink_metadata(dir)
                .map_err(|source| SkillError::Io {
                    path: dir.to_path_buf(),
                    source,
                })?
                .file_type();
            if ft.is_dir() {
                return Ok(OwnedDir::Existed);
            }
            if !ft.is_symlink() {
                // A regular file (or other non-dir node) squats the owned-dir
                // path — refuse rather than write around it.
                return Err(SkillError::Io {
                    path: dir.to_path_buf(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "owned skill dir path is occupied by a non-directory",
                    ),
                });
            }
            // Drop the symlink (its target is untouched) and re-create a real
            // dir. A second `AlreadyExists` is a raced re-plant: fail closed.
            fs::remove_file(dir).map_err(|source| SkillError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
            match fs::create_dir(dir) {
                Ok(()) => Ok(OwnedDir::Created { replaced_symlink: true }),
                Err(source) => Err(SkillError::Io {
                    path: dir.to_path_buf(),
                    source,
                }),
            }
        }
        Err(source) => Err(SkillError::Io {
            path: dir.to_path_buf(),
            source,
        }),
    }
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
/// `true` if anything was removed. Leaves now-empty directories in place
/// (harmless while the manifest always ships `references/`).
fn prune_orphans(dir: &std::path::Path, files: &[(&str, &str)]) -> Result<bool, SkillError> {
    use std::collections::HashSet;
    // Refuse to walk a symlinked owned dir: read_dir would follow it into
    // foreign storage and prune files we do not own. (Subdir symlinks are
    // already safe — file_type() below never pushes them onto the stack.)
    let root_meta = fs::symlink_metadata(dir).map_err(|source| SkillError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    if root_meta.file_type().is_symlink() {
        return Ok(false);
    }
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

/// Remove each `skill` bundle's owned dir for `explicit` harnesses (or
/// auto-detected ones) at `scope`.
///
/// # Errors
///
/// [`SkillError::NoHarnessDetected`] when auto-detect finds nothing,
/// [`SkillError::NoHome`] / [`SkillError::NoCwd`] on an unresolvable base dir,
/// or [`SkillError::Io`] on a failed delete.
pub fn remove(
    explicit: Option<&[Harness]>,
    skills: &[&SkillBundle],
    scope: Scope,
    env: &impl SkillEnv,
) -> Result<RemoveReport, SkillError> {
    let base = base_dir(scope, env)?;
    let (targets, _) = resolve_targets(explicit, scope, env)?;
    let mut skill_reports = Vec::with_capacity(skills.len());
    for bundle in skills {
        let mut removed = Vec::with_capacity(targets.len());
        for &h in &targets {
            let dir = h.skill_dir(bundle.name, scope, &base);
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
        skill_reports.push(SkillRemove {
            skill_name: bundle.name.to_owned(),
            removed,
        });
    }
    Ok(RemoveReport {
        scope: scope.as_str().to_owned(),
        skills: skill_reports,
    })
}

/// Report detection + install state for every bundled skill × supported harness
/// at `scope`. Never errors on "nothing detected" — only on an unresolvable base
/// dir.
///
/// # Errors
///
/// Path-resolution errors only.
pub fn status(scope: Scope, env: &impl SkillEnv) -> Result<StatusReport, SkillError> {
    let all: Vec<&SkillBundle> = SKILLS.iter().collect();
    status_in(&all, scope, env)
}

/// [`status`] over an explicit set of `skills` (the seam [`status`] delegates to
/// with the full registry). Reports the per-skill × per-harness matrix.
///
/// # Errors
///
/// Path-resolution errors only.
pub fn status_in(
    skills: &[&SkillBundle],
    scope: Scope,
    env: &impl SkillEnv,
) -> Result<StatusReport, SkillError> {
    let base = base_dir(scope, env)?;
    let skills = skills
        .iter()
        .map(|bundle| {
            let harnesses = Harness::ALL
                .into_iter()
                .map(|h| {
                    let file = h.skill_file(bundle.name, scope, &base);
                    let dir = h.skill_dir(bundle.name, scope, &base);
                    let detected = h.markers(scope, &base).iter().any(|m| m.exists());
                    // `installed` keys on the primary file (SKILL.md);
                    // `up_to_date` requires every bundled file present + identical.
                    let installed = file.exists();
                    let up_to_date = installed
                        && bundle.files.iter().all(|&(rel, body)| {
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
            SkillStatus {
                skill_name: bundle.name.to_owned(),
                harnesses,
            }
        })
        .collect();
    Ok(StatusReport {
        scope: scope.as_str().to_owned(),
        skills,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bundle, SEARCH_SKILL};
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

    /// All bundled skills — the CLI/MCP default selector.
    fn all() -> Vec<&'static SkillBundle> {
        SKILLS.iter().collect()
    }

    /// The search bundle alone.
    fn search() -> Vec<&'static SkillBundle> {
        vec![bundle(SEARCH_SKILL).unwrap()]
    }

    /// The search skill's install report (only bundle today).
    fn search_install(report: &InstallReport) -> &SkillInstall {
        report
            .skills
            .iter()
            .find(|s| s.skill_name == SEARCH_SKILL)
            .expect("search skill in report")
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

    /// A test-only second bundle. `install`/`remove`/`status_in` take
    /// `&[&SkillBundle]`, so this exercises the true multi-bundle path with zero
    /// production change (it is NOT added to the `SKILLS` registry).
    const SYNTH: SkillBundle = SkillBundle {
        name: "synthetic-test-skill",
        files: &[("SKILL.md", "---\nname: synthetic-test-skill\ndescription: t\n---\nbody\n")],
    };

    #[test]
    fn multi_bundle_install_and_remove_are_isolated_per_skill() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let both: [&SkillBundle; 2] = [bundle(SEARCH_SKILL).unwrap(), &SYNTH];

        // Install both: the per-bundle loop runs twice (non-vacuous), the report
        // carries 2 skills in first-seen (passed) order, and BOTH owned dirs exist.
        let report = install(None, &both, Scope::User, &env).unwrap();
        assert_eq!(report.skills.len(), 2);
        assert_eq!(report.skills[0].skill_name, SEARCH_SKILL, "first-seen order preserved");
        assert_eq!(report.skills[1].skill_name, "synthetic-test-skill");
        let search_dir = Harness::ClaudeCode.skill_dir(SEARCH_SKILL, Scope::User, &env.home);
        let synth_dir =
            Harness::ClaudeCode.skill_dir("synthetic-test-skill", Scope::User, &env.home);
        assert!(search_dir.join("SKILL.md").exists());
        assert!(synth_dir.join("SKILL.md").exists());

        // Remove ONLY the synthetic skill: its dir is gone, the search skill's
        // dir/file SURVIVES. This is the per-skill isolation guarantee — it would
        // fail if remove weren't scoped to each bundle's own dir.
        let rm = remove(Some(&[Harness::ClaudeCode]), &[&SYNTH], Scope::User, &env).unwrap();
        assert_eq!(rm.skills.len(), 1);
        assert_eq!(rm.skills[0].skill_name, "synthetic-test-skill");
        assert_eq!(rm.skills[0].removed[0].action, RemoveAction::Removed);
        assert!(!synth_dir.exists(), "synthetic skill dir must be removed");
        assert!(
            search_dir.join("SKILL.md").exists(),
            "search skill must survive a scoped removal of a different skill"
        );
    }

    #[test]
    fn status_matrix_is_per_skill_independent() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let both: [&SkillBundle; 2] = [bundle(SEARCH_SKILL).unwrap(), &SYNTH];
        install(None, &both, Scope::User, &env).unwrap();

        let cc_up_to_date = |st: &StatusReport, name: &str| -> bool {
            st.skills
                .iter()
                .find(|s| s.skill_name == name)
                .unwrap()
                .harnesses
                .iter()
                .find(|h| h.harness == "claude-code")
                .unwrap()
                .up_to_date
        };

        // Baseline matrix: 2 rows (one per skill), both up_to_date on claude-code.
        let st = status_in(&both, Scope::User, &env).unwrap();
        assert_eq!(st.skills.len(), 2, "matrix has one row per skill");
        assert!(cc_up_to_date(&st, SEARCH_SKILL));
        assert!(cc_up_to_date(&st, "synthetic-test-skill"));

        // Make ONLY the search skill stale; the synthetic skill must stay current.
        let search_md = Harness::ClaudeCode.skill_file(SEARCH_SKILL, Scope::User, &env.home);
        std::fs::write(&search_md, "stale").unwrap();
        let st2 = status_in(&both, Scope::User, &env).unwrap();
        assert!(!cc_up_to_date(&st2, SEARCH_SKILL), "search skill went stale");
        assert!(
            cc_up_to_date(&st2, "synthetic-test-skill"),
            "one skill going stale must not flip another skill's up_to_date"
        );
    }

    #[test]
    fn install_covers_every_selected_skill() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, &all(), Scope::User, &env).unwrap();
        assert_eq!(report.skills.len(), SKILLS.len(), "one SkillInstall per selected bundle");
        for s in &report.skills {
            assert_eq!(
                s.installed.len(),
                1,
                "{} installed to the one detected harness",
                s.skill_name
            );
        }
    }

    #[test]
    fn install_then_reinstall_is_idempotent() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let first = install(None, &search(), Scope::User, &env).unwrap();
        let si = search_install(&first);
        assert_eq!(si.installed.len(), 1);
        assert_eq!(si.installed[0].action, InstallAction::Created);
        assert!(si.installed[0].path.exists());

        let second = install(None, &search(), Scope::User, &env).unwrap();
        assert_eq!(search_install(&second).installed[0].action, InstallAction::Unchanged);
    }

    #[test]
    fn install_overwrites_stale_content_as_updated() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, &search(), Scope::User, &env).unwrap();
        let path = search_install(&report).installed[0].path.clone();
        std::fs::write(&path, "stale body").unwrap();

        let again = install(None, &search(), Scope::User, &env).unwrap();
        assert_eq!(search_install(&again).installed[0].action, InstallAction::Updated);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            bundle(SEARCH_SKILL).unwrap().skill_markdown()
        );
    }

    #[test]
    fn explicit_harness_forces_install_even_when_undetected() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf() };
        let report = install(Some(&[Harness::Cursor]), &search(), Scope::User, &env).unwrap();
        let si = search_install(&report);
        assert_eq!(si.installed.len(), 1);
        assert_eq!(si.installed[0].harness, "cursor");
        assert_eq!(report.detected, vec!["cursor".to_owned()], "forced targets fill `detected`");
        assert!(report.not_detected.is_empty());
    }

    #[test]
    fn forced_claude_code_install_reports_detected() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf() };
        let report = install(Some(&[Harness::ClaudeCode]), &search(), Scope::User, &env).unwrap();
        assert_eq!(report.detected, vec!["claude-code".to_owned()]);
    }

    #[test]
    fn autodetect_with_no_harness_errors() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf() };
        let err = install(None, &all(), Scope::User, &env).unwrap_err();
        assert!(matches!(err, SkillError::NoHarnessDetected { .. }));
    }

    #[test]
    fn not_detected_lists_absent_harnesses_on_autodetect() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, &search(), Scope::User, &env).unwrap();
        assert_eq!(search_install(&report).installed.len(), 1);
        assert_eq!(
            report.detected,
            vec!["claude-code".to_owned()],
            "auto-detect fills `detected` with the detected set"
        );
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
        install(None, &search(), Scope::User, &env).unwrap();
        let st = status(Scope::User, &env).unwrap();
        assert_eq!(st.skills.len(), SKILLS.len(), "status is a per-skill matrix");
        let search_skill = st
            .skills
            .iter()
            .find(|s| s.skill_name == SEARCH_SKILL)
            .unwrap();
        let cc = search_skill
            .harnesses
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        assert!(cc.detected && cc.installed && cc.up_to_date);
        let cursor = search_skill
            .harnesses
            .iter()
            .find(|h| h.harness == "cursor")
            .unwrap();
        assert!(!cursor.detected && !cursor.installed && !cursor.up_to_date);

        std::fs::write(&cc.path, "stale").unwrap();
        let st2 = status(Scope::User, &env).unwrap();
        let cc2 = st2
            .skills
            .iter()
            .find(|s| s.skill_name == SEARCH_SKILL)
            .unwrap()
            .harnesses
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        assert!(cc2.installed && !cc2.up_to_date);
    }

    #[test]
    fn status_not_up_to_date_when_a_reference_is_stale() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        install(None, &search(), Scope::User, &env).unwrap();
        // Make ONLY a reference stale; SKILL.md is untouched.
        let dir = Harness::ClaudeCode.skill_dir(SEARCH_SKILL, Scope::User, &env.home);
        std::fs::write(dir.join("references").join("advanced-techniques.md"), "stale").unwrap();

        let st = status(Scope::User, &env).unwrap();
        let cc = st
            .skills
            .iter()
            .find(|s| s.skill_name == SEARCH_SKILL)
            .unwrap()
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
        install(None, &search(), Scope::User, &env).unwrap();
        let r1 = remove(Some(&[Harness::ClaudeCode]), &search(), Scope::User, &env).unwrap();
        assert_eq!(r1.skills[0].removed[0].action, RemoveAction::Removed);
        assert!(!r1.skills[0].removed[0].path.exists());
        let r2 = remove(Some(&[Harness::ClaudeCode]), &search(), Scope::User, &env).unwrap();
        assert_eq!(r2.skills[0].removed[0].action, RemoveAction::Absent);
    }

    #[test]
    fn report_serializes_to_expected_json_shape() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, &search(), Scope::User, &env).unwrap();
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["scope"], "user");
        assert_eq!(v["detected"], serde_json::json!(["claude-code"]));
        assert!(v["not_detected"].is_array());
        assert_eq!(v["skills"][0]["skill_name"], "midnight-advanced-search");
        assert_eq!(v["skills"][0]["installed"][0]["harness"], "claude-code");
        assert_eq!(v["skills"][0]["installed"][0]["action"], "created");
        assert!(v["skills"][0]["installed"][0]["reload_step"].is_string());
    }

    #[test]
    fn install_propagates_non_notfound_read_error() {
        // A directory where SKILL.md is itself a directory makes read_to_string
        // fail with something other than NotFound; install must surface Io, not
        // mis-report Created.
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let dir = Harness::ClaudeCode.skill_dir(SEARCH_SKILL, Scope::User, &env.home);
        std::fs::create_dir_all(dir.join("SKILL.md")).unwrap(); // SKILL.md is a dir
        let err = install(None, &search(), Scope::User, &env).unwrap_err();
        assert!(matches!(err, SkillError::Io { .. }));
    }

    #[test]
    fn install_writes_every_bundle_file() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, &search(), Scope::User, &env).unwrap();
        let dir = search_install(&report).installed[0]
            .path
            .parent()
            .unwrap()
            .to_path_buf();
        for &(rel, body) in bundle(SEARCH_SKILL).unwrap().files {
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
        let report = install(None, &search(), Scope::User, &env).unwrap();
        let dir = search_install(&report).installed[0]
            .path
            .parent()
            .unwrap()
            .to_path_buf();
        // Drop a stray file at the root and inside references/.
        std::fs::write(dir.join("stray.md"), "junk").unwrap();
        std::fs::write(dir.join("references").join("orphan.md"), "junk").unwrap();

        let again = install(None, &search(), Scope::User, &env).unwrap();
        assert_eq!(
            search_install(&again).installed[0].action,
            InstallAction::Updated,
            "prune must mark Updated"
        );
        assert!(!dir.join("stray.md").exists(), "root orphan not pruned");
        assert!(!dir.join("references").join("orphan.md").exists(), "nested orphan not pruned");
        // Manifest files survive the prune.
        assert!(dir.join("SKILL.md").exists());
        assert!(dir.join("references").join("filters-and-modes.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_does_not_traverse_symlinked_skill_dir() {
        use std::os::unix::fs::symlink;
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        // A foreign dir holding a file the manifest does NOT ship.
        let foreign = env.home.join("foreign-notes");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("keep.md"), "precious").unwrap();
        // Make the owned skill dir a pre-existing symlink to the foreign dir.
        let skill_dir = Harness::ClaudeCode.skill_dir(SEARCH_SKILL, Scope::User, &env.home);
        std::fs::create_dir_all(skill_dir.parent().unwrap()).unwrap();
        symlink(&foreign, &skill_dir).unwrap();

        // Install must not let prune traverse the symlink and delete foreign data.
        install(None, &search(), Scope::User, &env).unwrap();

        assert!(
            foreign.join("keep.md").exists(),
            "prune traversed a symlinked skill dir and deleted foreign data"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_does_not_write_through_symlinked_skill_dir() {
        use std::os::unix::fs::symlink;
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        // A foreign dir the owned skill dir is symlinked at, holding a same-named
        // file (SKILL.md) that the write phase would otherwise clobber.
        let foreign = env.home.join("foreign-notes");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("SKILL.md"), "precious").unwrap();

        let skill_dir = Harness::ClaudeCode.skill_dir(SEARCH_SKILL, Scope::User, &env.home);
        std::fs::create_dir_all(skill_dir.parent().unwrap()).unwrap();
        symlink(&foreign, &skill_dir).unwrap();

        let report = install(None, &search(), Scope::User, &env).unwrap();

        // The foreign SKILL.md is untouched: the write did not follow the link.
        assert_eq!(std::fs::read_to_string(foreign.join("SKILL.md")).unwrap(), "precious");
        // The owned dir is now a real directory (link replaced) holding our file.
        let meta = std::fs::symlink_metadata(&skill_dir).unwrap();
        assert!(!meta.file_type().is_symlink(), "owned dir must no longer be a symlink");
        assert!(meta.is_dir(), "owned dir must be a real directory");
        assert_ne!(
            std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
            "precious",
            "SKILL.md must be the bundle's content, written into the owned dir"
        );
        // The replacement is reported, not silent.
        assert!(
            search_install(&report).installed[0].replaced_symlink,
            "a dropped owned-dir symlink must be signalled in the report"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_refuses_when_owned_dir_is_a_regular_file() {
        // A non-dir squatting the owned-dir path must fail closed — the same
        // fail-closed branch a re-planted symlink hits on the second create_dir —
        // rather than being written around.
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let skill_dir = Harness::ClaudeCode.skill_dir(SEARCH_SKILL, Scope::User, &env.home);
        std::fs::create_dir_all(skill_dir.parent().unwrap()).unwrap();
        std::fs::write(&skill_dir, "not a directory").unwrap();

        let err = install(None, &search(), Scope::User, &env).unwrap_err();
        assert!(matches!(err, SkillError::Io { .. }), "expected fail-closed Io, got {err:?}");
        // The squatting file is left exactly as-is — not clobbered.
        assert_eq!(std::fs::read_to_string(&skill_dir).unwrap(), "not a directory");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_owned_dir_fresh_existing_and_symlink() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();

        // Fresh: created, no symlink replaced.
        let fresh = tmp.path().join("root").join("skill");
        assert!(matches!(
            ensure_owned_dir(&fresh).unwrap(),
            OwnedDir::Created { replaced_symlink: false }
        ));
        assert!(fresh.is_dir());

        // Idempotent: a real dir already there → Existed.
        assert!(matches!(ensure_owned_dir(&fresh).unwrap(), OwnedDir::Existed));

        // Symlink: replaced with a real dir, target left intact, flag set.
        let foreign = tmp.path().join("foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("keep"), "precious").unwrap();
        let linked = tmp.path().join("root").join("linked");
        symlink(&foreign, &linked).unwrap();
        assert!(matches!(
            ensure_owned_dir(&linked).unwrap(),
            OwnedDir::Created { replaced_symlink: true }
        ));
        assert!(!std::fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(linked.is_dir());
        assert_eq!(std::fs::read_to_string(foreign.join("keep")).unwrap(), "precious");

        // Non-dir squatter → fail closed.
        let file_path = tmp.path().join("root").join("afile");
        std::fs::write(&file_path, "x").unwrap();
        assert!(matches!(ensure_owned_dir(&file_path), Err(SkillError::Io { .. })));
    }

    #[test]
    fn install_project_scope_writes_under_repo_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        // FakeEnv.current_dir() == home == root; base_dir(Project) walks to .git at root.
        let env = FakeEnv { home: root.to_path_buf() };
        let report = install(None, &search(), Scope::Project, &env).unwrap();
        assert_eq!(report.scope, "project");
        let cc = search_install(&report)
            .installed
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        assert_eq!(cc.path, root.join(".claude/skills/midnight-advanced-search/SKILL.md"));
        assert!(cc.path.exists());
    }
}

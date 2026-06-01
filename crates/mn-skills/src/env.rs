//! Environment lookups the installer needs, abstracted so tests drive a fake
//! home / cwd without touching the real dotfile layout.

use std::path::PathBuf;

/// Filesystem anchors the installer needs for path resolution.
///
/// Two anchors: the user's home directory (for `--scope user`) and the current
/// working directory (for `--scope project`, from which the repo root is
/// found).
pub trait SkillEnv {
    /// The user's home directory, or `None` if it cannot be determined.
    fn home_dir(&self) -> Option<PathBuf>;
    /// The current working directory, or `None` if it cannot be determined.
    fn current_dir(&self) -> Option<PathBuf>;
}

/// Production [`SkillEnv`] backed by the process environment.
///
/// `home_dir` reads `HOME`, falling back to `USERPROFILE` (Windows). This
/// matches the workspace's existing `HOME`-keyed path resolution in
/// `mn_core::paths`.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdSkillEnv;

impl SkillEnv for StdSkillEnv {
    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }

    fn current_dir(&self) -> Option<PathBuf> {
        std::env::current_dir().ok()
    }
}

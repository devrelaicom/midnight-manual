//! The crate's error type, shared by detection and install logic.

use std::path::PathBuf;

/// Anything that can go wrong resolving paths or writing the skill.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// Neither `HOME` nor `USERPROFILE` is set, so user-scope paths are
    /// unresolvable.
    #[error("could not determine home directory (HOME / USERPROFILE unset)")]
    NoHome,
    /// The current working directory could not be read, so project-scope paths
    /// are unresolvable.
    #[error("could not determine the current working directory")]
    NoCwd,
    /// Auto-detect found no supported harness and none were forced.
    #[error(
        "no supported AI harness detected at {scope} scope (probed: {probed}); \
         pass --harness with one or more of: claude-code, codex, opencode, cursor"
    )]
    NoHarnessDetected {
        /// The scope that was probed.
        scope: String,
        /// Comma-joined harness ids that were probed.
        probed: String,
    },
    /// A `--skill` / `skill` selector named a skill that is not in the registry.
    #[error("unknown skill `{name}` (known: {known})")]
    UnknownSkill {
        /// The unrecognized skill name.
        name: String,
        /// Comma-joined known skill names.
        known: String,
    },
    /// A filesystem write / read / delete failed.
    #[error("filesystem error at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: PathBuf,
        /// The underlying io error.
        #[source]
        source: std::io::Error,
    },
}

//! Model-cache directory resolution (FR-044, D14).
//!
//! Precedence (matches mn-core::config):
//! 1. `MIDNIGHT_MANUAL_MODEL_CACHE_DIR` env override
//! 2. `$XDG_DATA_HOME/midnight-manual/models/`
//! 3. `$HOME/.local/share/midnight-manual/models/`
//!
//! The cache holds the fastembed reranker model (`bge-reranker-base` by
//! default); each model lives in its own subdirectory managed by fastembed.
//! The corpus embedder is no longer local — embedding runs via VoyageAI.

use std::path::PathBuf;

/// Resolve the on-disk model cache directory using the standard precedence.
///
/// Returns `None` only when none of the env vars used to derive the default
/// location are set (e.g. running in a heavily-sandboxed environment without
/// `HOME`). Callers can fall back to a tempdir in that case.
#[must_use]
pub fn resolve(env: &impl Env) -> Option<PathBuf> {
    if let Some(p) = env.var("MIDNIGHT_MANUAL_MODEL_CACHE_DIR") {
        return Some(PathBuf::from(p));
    }
    if let Some(xdg) = env.var("XDG_DATA_HOME") {
        return Some(PathBuf::from(xdg).join("midnight-manual").join("models"));
    }
    if let Some(home) = env.var("HOME") {
        return Some(
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("midnight-manual")
                .join("models"),
        );
    }
    None
}

/// Tiny env-lookup trait so tests can substitute a `FakeEnv` without touching
/// `std::env`.
pub trait Env {
    /// Read an env var by name.
    fn var(&self, name: &str) -> Option<String>;
}

/// Production `Env` impl backed by `std::env::var`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdEnv;

impl Env for StdEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct FakeEnv(HashMap<String, String>);

    impl FakeEnv {
        fn set(mut self, k: &str, v: &str) -> Self {
            self.0.insert(k.into(), v.into());
            self
        }
    }

    impl Env for FakeEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn explicit_env_wins() {
        let env = FakeEnv::default()
            .set("MIDNIGHT_MANUAL_MODEL_CACHE_DIR", "/explicit")
            .set("XDG_DATA_HOME", "/xdg")
            .set("HOME", "/home/x");
        assert_eq!(resolve(&env), Some(PathBuf::from("/explicit")));
    }

    #[test]
    fn xdg_beats_home() {
        let env = FakeEnv::default()
            .set("XDG_DATA_HOME", "/xdg")
            .set("HOME", "/home/x");
        assert_eq!(resolve(&env), Some(PathBuf::from("/xdg/midnight-manual/models")));
    }

    #[test]
    fn home_fallback() {
        let env = FakeEnv::default().set("HOME", "/home/x");
        assert_eq!(
            resolve(&env),
            Some(PathBuf::from("/home/x/.local/share/midnight-manual/models"))
        );
    }

    #[test]
    fn none_when_nothing_set() {
        let env = FakeEnv::default();
        assert!(resolve(&env).is_none());
    }
}

//! Model-cache directory resolution (FR-044, D14).
//!
//! Precedence (matches mnm-core::config):
//! 1. `MIDNIGHT_MANUAL_MODEL_CACHE_DIR` env override
//! 2. `$XDG_DATA_HOME/midnight-manual/models/`
//! 3. `$HOME/.local/share/midnight-manual/models/`
//!
//! The cache holds the on-disk tokenizer assets used for local token counting
//! (see `mnm-content::tokens`); each lives in its own subdirectory. Embedding and
//! reranking are both remote (VoyageAI), so no model weights are cached here.

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

/// Resolve the model cache directory, honouring a config-file `[models].cache_dir`
/// override.
///
/// When `cfg_dir` is `Some`, it wins outright (the config layer sits above the
/// env-chain). When `None`, this falls back to [`resolve`] — i.e. the
/// `MIDNIGHT_MANUAL_MODEL_CACHE_DIR` > `XDG_DATA_HOME` > `HOME` walk.
///
/// Note: the *flag* layer (which sits above config, e.g. `mnm models pull
/// --cache-dir`) is applied by the caller before reaching this helper — flag and
/// config both short-circuit to a concrete path, so the caller checks the flag
/// first and only passes `cfg_dir` here.
#[must_use]
pub fn resolve_with_override(cfg_dir: Option<&std::path::Path>, env: &impl Env) -> Option<PathBuf> {
    if let Some(dir) = cfg_dir {
        return Some(dir.to_path_buf());
    }
    resolve(env)
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

    #[test]
    fn override_wins_over_env_chain() {
        // A config-supplied dir beats even the MIDNIGHT_MANUAL_MODEL_CACHE_DIR env.
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_MODEL_CACHE_DIR", "/from-env");
        let resolved = resolve_with_override(Some(std::path::Path::new("/from-config")), &env);
        assert_eq!(resolved, Some(PathBuf::from("/from-config")));
    }

    #[test]
    fn override_none_falls_back_to_env_chain() {
        let env = FakeEnv::default().set("XDG_DATA_HOME", "/xdg");
        let resolved = resolve_with_override(None, &env);
        assert_eq!(resolved, Some(PathBuf::from("/xdg/midnight-manual/models")));
    }
}

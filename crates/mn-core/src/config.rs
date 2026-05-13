//! Config-file discovery and shape.
//!
//! Precedence (D17/D18, FR-016): explicit `--config <path>` flag > env vars
//! (`MIDNIGHT_MANUAL_CONFIG`, `MIDNIGHT_MANUAL_SERVER`, etc.) > XDG-discovered
//! `$XDG_CONFIG_HOME/midnight-manual/config.toml` > compiled-in defaults. The
//! [`Config`] struct here is the merged in-memory result; the loader
//! [`Config::discover`] performs the precedence walk.
//!
//! No keychain access in v1 (D28) — tokens live in the sibling `auth.toml`
//! handled by [`crate::auth_file`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The merged-and-resolved configuration handed to every CLI/MCP/server entrypoint.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Config {
    /// `[server]` section.
    #[serde(default)]
    pub server: ServerConfig,
    /// `[models]` section.
    #[serde(default)]
    pub models: ModelsConfig,
    /// `[telemetry]` section.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// `[cli]` section — admin-visibility flag etc.
    #[serde(default)]
    pub cli: CliConfig,
}

/// `[server]` — cloud endpoint settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Cloud server base URL (defaults to the production Fly.io deployment).
    pub url: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            url: "https://manual.midnight.network".into(),
        }
    }
}

/// `[models]` — local ML model lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsConfig {
    /// Embedder model identifier (`{name}` only; revision is server-supplied).
    pub embedding: String,
    /// Reranker model identifier.
    pub reranker: String,
    /// Override for the on-disk model cache directory. When `None`, the
    /// discoverer resolves to `$XDG_DATA_HOME/midnight-manual/models/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<PathBuf>,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            embedding: "bge-base-en-v1.5".into(),
            reranker: "bge-reranker-base".into(),
            cache_dir: None,
        }
    }
}

/// `[telemetry]` — opt-out telemetry knobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// Master enable flag. Three mechanisms (FR-107) — env, this flag, or `mnm
    /// telemetry disable` — turn the entire pipeline off.
    pub enabled: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// `[cli]` — admin-visibility et al.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CliConfig {
    /// When `true`, `mnm --help` shows admin commands even without the env
    /// override. Per D23, this never gates invocation.
    #[serde(default)]
    pub show_admin_cmds: bool,
}

impl Config {
    /// Discover and load a config from `--config` (`explicit_path`), then the
    /// `MIDNIGHT_MANUAL_CONFIG` env var, then the XDG location.
    ///
    /// If no file is found, returns [`Config::default`]. Returns the loaded
    /// config and the path the loader resolved (or `None` if defaulted).
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Read`] if the file is unreadable, or
    /// [`ConfigError::Parse`] if the TOML is malformed.
    pub fn discover(
        explicit_path: Option<&Path>,
        env: &impl ConfigEnv,
    ) -> Result<(Self, Option<PathBuf>), ConfigError> {
        let path = explicit_path.map(Path::to_path_buf).or_else(|| {
            env.var("MIDNIGHT_MANUAL_CONFIG")
                .map(PathBuf::from)
                .or_else(|| xdg_config_path(env))
        });

        match path {
            Some(p) if p.exists() => {
                let body = std::fs::read_to_string(&p).map_err(|e| ConfigError::Read {
                    path: p.clone(),
                    message: e.to_string(),
                })?;
                let cfg: Self = toml::from_str(&body).map_err(|e| ConfigError::Parse {
                    path: p.clone(),
                    message: e.to_string(),
                })?;
                Ok((cfg, Some(p)))
            }
            Some(_) | None => Ok((Self::default(), None)),
        }
    }
}

/// Read-only abstraction over environment lookup so config loading is testable
/// without `std::env` side effects.
pub trait ConfigEnv {
    /// Read an environment variable.
    fn var(&self, name: &str) -> Option<String>;
}

/// The default `ConfigEnv` impl reads `std::env`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdEnv;

impl ConfigEnv for StdEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

fn xdg_config_path(env: &impl ConfigEnv) -> Option<PathBuf> {
    if let Some(xdg) = env.var("XDG_CONFIG_HOME") {
        return Some(
            PathBuf::from(xdg)
                .join("midnight-manual")
                .join("config.toml"),
        );
    }
    if let Some(home) = env.var("HOME") {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join("midnight-manual")
                .join("config.toml"),
        );
    }
    None
}

/// All the ways config discovery can fail.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// I/O failure reading the resolved config file.
    #[error("failed to read config file `{}`: {message}", path.display())]
    Read {
        /// File path that failed to read.
        path: PathBuf,
        /// Underlying I/O error message.
        message: String,
    },
    /// TOML parse failure on the resolved config file.
    #[error("failed to parse config file `{}`: {message}", path.display())]
    Parse {
        /// File path that failed to parse.
        path: PathBuf,
        /// Underlying parser error message.
        message: String,
    },
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

    impl ConfigEnv for FakeEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn default_when_nothing_present() {
        let env = FakeEnv::default();
        let (cfg, path) = Config::discover(None, &env).unwrap();
        assert!(path.is_none());
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn explicit_path_beats_env_and_xdg() {
        let tmp = tempdir();
        let cfg_path = tmp.path().join("explicit.toml");
        std::fs::write(&cfg_path, "[server]\nurl = \"https://explicit.example\"\n").unwrap();

        let env = FakeEnv::default().set(
            "MIDNIGHT_MANUAL_CONFIG",
            tmp.path()
                .join("env.toml")
                .to_str()
                .expect("temp path utf-8"),
        );
        let (cfg, resolved) = Config::discover(Some(&cfg_path), &env).unwrap();
        assert_eq!(resolved.as_deref(), Some(cfg_path.as_path()));
        assert_eq!(cfg.server.url, "https://explicit.example");
    }

    #[test]
    fn env_var_beats_xdg() {
        let tmp = tempdir();
        let env_target = tmp.path().join("from-env.toml");
        std::fs::write(&env_target, "[server]\nurl = \"https://env.example\"\n").unwrap();

        let env = FakeEnv::default()
            .set("MIDNIGHT_MANUAL_CONFIG", env_target.to_str().unwrap())
            .set("XDG_CONFIG_HOME", tmp.path().to_str().unwrap());
        let (cfg, _) = Config::discover(None, &env).unwrap();
        assert_eq!(cfg.server.url, "https://env.example");
    }

    #[test]
    fn malformed_toml_returns_parse_error() {
        let tmp = tempdir();
        let path = tmp.path().join("broken.toml");
        std::fs::write(&path, "this is not = valid = toml\n").unwrap();

        let env = FakeEnv::default();
        let err = Config::discover(Some(&path), &env).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("create tempdir")
    }
}

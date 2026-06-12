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
    /// `[rerank]` section.
    #[serde(default)]
    pub rerank: RerankConfig,
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
            url: "https://midnight-manual.midnightntwrk.expert".into(),
        }
    }
}

/// `[models]` — ML model selection and Voyage API settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsConfig {
    /// General corpus embedding model name (e.g. "voyage-context-3").
    pub embedding: String,
    /// Code-specialised embedding model name (dual embeddings, D1).
    #[serde(default = "default_code_embedding")]
    pub code_embedding: String,
    /// Override for the on-disk model cache directory. When `None`, the
    /// discoverer resolves to `$XDG_DATA_HOME/midnight-manual/models/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<PathBuf>,
    /// Voyage API key (BYOK). Resolved with flag > env > config precedence;
    /// this is the config-file fallback only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voyage_api_key: Option<String>,
    /// Voyage output dimension (Matryoshka): 256/512/1024/2048.
    #[serde(default = "default_voyage_dim")]
    pub voyage_output_dimension: u32,
    /// Voyage output dtype: "float" | "int8" | "uint8" | "binary" | "ubinary".
    #[serde(default = "default_voyage_dtype")]
    pub voyage_output_dtype: String,
    /// Per-request timeout (seconds) for BYOK Voyage embedding calls. `voyage-code-3`
    /// embedding of a few-hundred-chunk batch can take ~38s+, so the default sits
    /// well above the old 30s ceiling. Resolved with flag > env > config precedence.
    #[serde(default = "default_voyage_timeout_secs")]
    pub voyage_timeout_secs: u64,
}

const fn default_voyage_dim() -> u32 {
    1024
}

const fn default_voyage_timeout_secs() -> u64 {
    120
}

fn default_voyage_dtype() -> String {
    "float".to_owned()
}

fn default_code_embedding() -> String {
    "voyage-code-3".to_owned()
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            embedding: "voyage-context-3".into(),
            code_embedding: default_code_embedding(),
            cache_dir: None,
            voyage_api_key: None,
            voyage_output_dimension: default_voyage_dim(),
            voyage_output_dtype: default_voyage_dtype(),
            voyage_timeout_secs: default_voyage_timeout_secs(),
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

/// Resolve the Voyage API key with precedence flag > `VOYAGE_API_KEY` env > config.
///
/// An empty string at a level is treated as absent and falls through to the
/// next source. If all sources are absent or empty, returns `None`.
pub fn resolve_voyage_api_key(
    flag: Option<&str>,
    cfg: &ModelsConfig,
    env: &impl ConfigEnv,
) -> Option<String> {
    flag.map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| env.var("VOYAGE_API_KEY").filter(|s| !s.is_empty()))
        .or_else(|| cfg.voyage_api_key.clone().filter(|s| !s.is_empty()))
}

/// Resolve the Voyage request timeout (seconds): flag > `VOYAGE_TIMEOUT_SECS` env > config.
///
/// The env var is parsed as `u64`; a non-numeric, empty, or **zero** value at
/// any level is treated as absent and falls through. Zero is rejected because
/// reqwest treats `timeout(0)` as an immediately-expiring deadline (which would
/// fail every request), not "unlimited". If every source is absent/zero, the
/// built-in default (`default_voyage_timeout_secs`, 120s) is used.
pub fn resolve_voyage_timeout_secs(
    flag: Option<u64>,
    cfg: &ModelsConfig,
    env: &impl ConfigEnv,
) -> u64 {
    flag.filter(|&n| n > 0)
        .or_else(|| {
            env.var("VOYAGE_TIMEOUT_SECS")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|&n| n > 0)
        })
        .or_else(|| (cfg.voyage_timeout_secs > 0).then_some(cfg.voyage_timeout_secs))
        .unwrap_or_else(default_voyage_timeout_secs)
}

/// `[rerank]` — client-side rerank placement and model selection (spec §4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct RerankConfig {
    /// Where reranking runs: `"auto"` (default) | `"local"` | `"server"` | `"off"`.
    pub location: Option<String>,
    /// `VoyageAI` rerank model: `"rerank-2.5"` (default) | `"rerank-2.5-lite"`.
    pub model: Option<String>,
}

/// Where a client runs reranking after placement resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankPlacement {
    /// Call `VoyageAI` directly with the user's own key; tell the server `none`.
    Local,
    /// Ask the server to rerank inline in `/v1/search`.
    Server,
    /// No reranking anywhere; tell the server `none`.
    Off,
}

impl RerankPlacement {
    /// Stable telemetry wire string (`"local"` | `"server"` | `"off"`), matching
    /// the `EventPayload::Rerank.placement` field. This is the single source of
    /// truth shared by the CLI and MCP clients so the placement label stays
    /// byte-aligned across both (and with the server-side telemetry validator).
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Server => "server",
            Self::Off => "off",
        }
    }
}

/// Resolve rerank placement with precedence flag > `MIDNIGHT_MANUAL_RERANK` env > config.
///
/// The config fallback is `[rerank].location`, ending at auto. Auto (and any
/// unrecognized value) resolves by key detection: a Voyage key present means
/// local BYOK, absent means server (D6). Mirrors the embedding-path defaulting.
///
/// An empty flag or env value is treated as absent and falls through to the
/// next source, matching the other config resolvers.
#[must_use]
pub fn resolve_rerank_placement(
    flag: Option<&str>,
    cfg: &RerankConfig,
    env: &impl ConfigEnv,
    has_voyage_key: bool,
) -> RerankPlacement {
    let explicit = |s: &str| match s {
        "local" => Some(RerankPlacement::Local),
        "server" => Some(RerankPlacement::Server),
        "off" => Some(RerankPlacement::Off),
        _ => None, // "auto" or unknown: fall through
    };
    flag.filter(|s| !s.is_empty())
        .and_then(explicit)
        .or_else(|| {
            env.var("MIDNIGHT_MANUAL_RERANK")
                .filter(|s| !s.is_empty())
                .as_deref()
                .and_then(explicit)
        })
        .or_else(|| cfg.location.as_deref().and_then(explicit))
        .unwrap_or(if has_voyage_key {
            RerankPlacement::Local
        } else {
            RerankPlacement::Server
        })
}

/// Resolve the rerank model with precedence flag > `MIDNIGHT_MANUAL_RERANK_MODEL` env > config.
///
/// The config fallback is `[rerank].model`, ending at `rerank-2.5`. Returns a
/// model variant only — never [`crate::rerank::RerankParam::None`] (placement
/// handles "off").
///
/// An empty flag or env value is treated as absent and falls through to the
/// next source, matching the other config resolvers.
#[must_use]
pub fn resolve_rerank_model(
    flag: Option<&str>,
    cfg: &RerankConfig,
    env: &impl ConfigEnv,
) -> crate::rerank::RerankParam {
    use crate::rerank::RerankParam;
    let parse = |s: &str| match s {
        "rerank-2.5" => Some(RerankParam::Rerank25),
        "rerank-2.5-lite" => Some(RerankParam::Rerank25Lite),
        _ => None,
    };
    flag.filter(|s| !s.is_empty())
        .and_then(parse)
        .or_else(|| {
            env.var("MIDNIGHT_MANUAL_RERANK_MODEL")
                .filter(|s| !s.is_empty())
                .as_deref()
                .and_then(parse)
        })
        .or_else(|| cfg.model.as_deref().and_then(parse))
        .unwrap_or(RerankParam::Rerank25)
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

    #[test]
    fn server_url_default_is_production_host() {
        let cfg = Config::default();
        assert_eq!(cfg.server.url, "https://midnight-manual.midnightntwrk.expert");
    }

    #[test]
    fn models_config_defaults_to_dual_voyage_models() {
        let m = ModelsConfig::default();
        assert_eq!(m.embedding, "voyage-context-3");
        assert_eq!(m.code_embedding, "voyage-code-3");
        assert_eq!(m.voyage_output_dimension, 1024);
        assert_eq!(m.voyage_output_dtype, "float");
        assert_eq!(m.voyage_timeout_secs, 120);
        assert!(m.voyage_api_key.is_none());
    }

    #[test]
    fn models_config_roundtrips_through_toml() {
        let toml_src = r#"
embedding = "voyage-code-3"
voyage_output_dimension = 1024
voyage_output_dtype = "float"
"#;
        let m: ModelsConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(m.embedding, "voyage-code-3");
        assert_eq!(m.code_embedding, "voyage-code-3"); // default filled in
        assert_eq!(m.voyage_output_dimension, 1024);
        assert_eq!(m.voyage_output_dtype, "float"); // default filled in
        assert_eq!(m.voyage_timeout_secs, 120); // default filled in
        assert!(m.voyage_api_key.is_none()); // Option default
        assert!(m.cache_dir.is_none()); // Option default
    }

    #[test]
    fn resolve_voyage_key_prefers_flag_then_env_then_config() {
        let cfg = ModelsConfig {
            voyage_api_key: Some("from-config".into()),
            ..Default::default()
        };
        let env = FakeEnv::default().set("VOYAGE_API_KEY", "from-env");

        assert_eq!(
            resolve_voyage_api_key(Some("from-flag"), &cfg, &env).as_deref(),
            Some("from-flag")
        );
        assert_eq!(resolve_voyage_api_key(None, &cfg, &env).as_deref(), Some("from-env"));

        let empty = FakeEnv::default();
        assert_eq!(resolve_voyage_api_key(None, &cfg, &empty).as_deref(), Some("from-config"));

        // An empty value at a level is absent and falls through to the next source.
        assert_eq!(resolve_voyage_api_key(Some(""), &cfg, &env).as_deref(), Some("from-env"));
        let env_empty = FakeEnv::default().set("VOYAGE_API_KEY", "");
        assert_eq!(resolve_voyage_api_key(None, &cfg, &env_empty).as_deref(), Some("from-config"));
        // All sources absent or empty → None.
        let cfg_none = ModelsConfig::default();
        assert_eq!(resolve_voyage_api_key(Some(""), &cfg_none, &env_empty), None);
    }

    #[test]
    fn resolve_voyage_timeout_prefers_flag_then_env_then_config() {
        let cfg = ModelsConfig {
            voyage_timeout_secs: 90,
            ..Default::default()
        };
        let env = FakeEnv::default().set("VOYAGE_TIMEOUT_SECS", "60");

        assert_eq!(resolve_voyage_timeout_secs(Some(45), &cfg, &env), 45);
        assert_eq!(resolve_voyage_timeout_secs(None, &cfg, &env), 60);

        let empty = FakeEnv::default();
        assert_eq!(resolve_voyage_timeout_secs(None, &cfg, &empty), 90);

        // Non-numeric and empty env values are ignored and fall through to config.
        let env_garbage = FakeEnv::default().set("VOYAGE_TIMEOUT_SECS", "not-a-number");
        assert_eq!(resolve_voyage_timeout_secs(None, &cfg, &env_garbage), 90);
        let env_empty = FakeEnv::default().set("VOYAGE_TIMEOUT_SECS", "");
        assert_eq!(resolve_voyage_timeout_secs(None, &cfg, &env_empty), 90);

        // Zero at any level is rejected (reqwest `timeout(0)` fails every
        // request) and falls through; all-zero yields the 120s default.
        let zero_cfg = ModelsConfig {
            voyage_timeout_secs: 0,
            ..Default::default()
        };
        assert_eq!(resolve_voyage_timeout_secs(Some(0), &zero_cfg, &empty), 120);
        assert_eq!(resolve_voyage_timeout_secs(Some(0), &cfg, &empty), 90);
        let env_zero = FakeEnv::default().set("VOYAGE_TIMEOUT_SECS", "0");
        assert_eq!(resolve_voyage_timeout_secs(None, &zero_cfg, &env_zero), 120);
    }

    #[test]
    fn rerank_config_parses_from_toml() {
        let toml = r#"
[rerank]
location = "server"
model = "rerank-2.5-lite"
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.rerank.location.as_deref(), Some("server"));
        assert_eq!(cfg.rerank.model.as_deref(), Some("rerank-2.5-lite"));
        // Absent section -> defaults (both None).
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.rerank.location.is_none() && cfg.rerank.model.is_none());
    }

    #[test]
    fn resolve_rerank_placement_precedence_and_auto() {
        let cfg = RerankConfig {
            location: Some("off".into()),
            model: None,
        };
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_RERANK", "server");
        // flag > env > config.
        assert_eq!(
            resolve_rerank_placement(Some("local"), &cfg, &env, false),
            RerankPlacement::Local
        );
        assert_eq!(resolve_rerank_placement(None, &cfg, &env, true), RerankPlacement::Server);
        let no_env = FakeEnv::default();
        assert_eq!(resolve_rerank_placement(None, &cfg, &no_env, true), RerankPlacement::Off);
        // Auto everywhere -> key detection: key => local, no key => server.
        let empty = RerankConfig::default();
        assert_eq!(resolve_rerank_placement(None, &empty, &no_env, true), RerankPlacement::Local);
        assert_eq!(resolve_rerank_placement(None, &empty, &no_env, false), RerankPlacement::Server);
        // Explicit "auto" at any level falls through to key detection.
        assert_eq!(
            resolve_rerank_placement(Some("auto"), &empty, &no_env, false),
            RerankPlacement::Server
        );
        // Unknown value falls through to the next level (lenient, like other resolvers).
        assert_eq!(
            resolve_rerank_placement(Some("bogus"), &empty, &no_env, true),
            RerankPlacement::Local
        );
    }

    #[test]
    fn rerank_placement_wire_strings() {
        // These wire strings are the single source of truth shared by the CLI
        // and MCP telemetry paths; they must match the telemetry validator's
        // expectations byte-for-byte.
        assert_eq!(RerankPlacement::Local.wire(), "local");
        assert_eq!(RerankPlacement::Server.wire(), "server");
        assert_eq!(RerankPlacement::Off.wire(), "off");
    }

    #[test]
    fn resolve_rerank_model_precedence_and_default() {
        use crate::rerank::RerankParam;
        let cfg = RerankConfig {
            location: None,
            model: Some("rerank-2.5-lite".into()),
        };
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_RERANK_MODEL", "rerank-2.5");
        assert_eq!(
            resolve_rerank_model(Some("rerank-2.5-lite"), &cfg, &env),
            RerankParam::Rerank25Lite
        );
        assert_eq!(resolve_rerank_model(None, &cfg, &env), RerankParam::Rerank25);
        let no_env = FakeEnv::default();
        assert_eq!(resolve_rerank_model(None, &cfg, &no_env), RerankParam::Rerank25Lite);
        // Nothing anywhere -> rerank-2.5; unknown strings fall through.
        assert_eq!(
            resolve_rerank_model(None, &RerankConfig::default(), &no_env),
            RerankParam::Rerank25
        );
        assert_eq!(
            resolve_rerank_model(Some("bogus"), &RerankConfig::default(), &no_env),
            RerankParam::Rerank25
        );
    }
}

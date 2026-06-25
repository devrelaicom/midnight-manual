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
    /// `[security]` section — MCP client injection-guarding level.
    #[serde(default)]
    pub security: SecurityConfig,
}

/// Compiled-in production cloud base URL. Single source of truth for the
/// default server endpoint — every other layer (the CLI resolver, the MCP
/// `ServerConfig` defaults) sources its fallback from here.
pub const DEFAULT_SERVER_URL: &str = "https://midnight-manual.midnightntwrk.expert";

/// `[server]` — cloud endpoint settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Cloud server base URL (defaults to the production Fly.io deployment).
    pub url: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { url: DEFAULT_SERVER_URL.into() }
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
    /// Override the Voyage API base URL (self-hosted proxy / regional endpoint).
    /// `None` uses the baked-in default. Resolved env > config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voyage_base_url: Option<String>,
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
            voyage_base_url: None,
        }
    }
}

/// Compiled-in default Gauge ingest endpoint (no trailing slash; the client
/// appends `/v1/logs`). Override with `MIDNIGHT_MANUAL_GAUGE_ENDPOINT` or
/// `[telemetry].endpoint` in config.toml.
pub const DEFAULT_TELEMETRY_ENDPOINT: &str = "https://gauge-telemetry.fly.dev";

fn default_telemetry_endpoint() -> String {
    DEFAULT_TELEMETRY_ENDPOINT.to_owned()
}

/// `[telemetry]` — opt-out telemetry knobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// Master enable flag. Three mechanisms (FR-107) — env, this flag, or `mnm
    /// telemetry disable` — turn the entire pipeline off.
    pub enabled: bool,
    /// Gauge ingest endpoint base URL. Defaults to [`DEFAULT_TELEMETRY_ENDPOINT`].
    #[serde(default = "default_telemetry_endpoint")]
    pub endpoint: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint: default_telemetry_endpoint(),
        }
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

/// `[security]` — MCP client prompt-injection guarding level (issue #103).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Guarding level: `"disabled"` | `"low"` | `"moderate"` | `"high"` |
    /// `"strict"`. Unknown/empty falls through to the resolver default.
    pub level: Option<String>,
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
/// Resolve the Voyage request timeout (seconds): flag > `VOYAGE_TIMEOUT_SECS`
/// env > `[models].voyage_timeout_secs` config.
///
/// A non-empty value that is non-numeric or zero is an error (zero is rejected
/// because reqwest treats `timeout(0)` as an immediately-expiring deadline).
/// Empty/absent env falls through; the config field carries the serde default
/// (120), so an unset config resolves to 120.
///
/// # Errors
/// [`ConfigError::InvalidValue`] for a non-numeric or zero value at the
/// authoritative layer.
pub fn resolve_voyage_timeout_secs(
    flag: Option<u64>,
    cfg: &ModelsConfig,
    env: &impl ConfigEnv,
) -> Result<u64, ConfigError> {
    let nonzero = |location: &str, n: u64| -> Result<u64, ConfigError> {
        if n == 0 {
            Err(ConfigError::InvalidValue {
                location: location.to_owned(),
                value: "0".to_owned(),
                expected: "a positive integer (seconds)".to_owned(),
            })
        } else {
            Ok(n)
        }
    };

    if let Some(n) = flag {
        return nonzero("the voyage-timeout flag", n);
    }
    if let Some(raw) = env.var("VOYAGE_TIMEOUT_SECS") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let n = trimmed.parse::<u64>().map_err(|_| ConfigError::InvalidValue {
                location: "VOYAGE_TIMEOUT_SECS".to_owned(),
                value: trimmed.to_owned(),
                expected: "a positive integer (seconds)".to_owned(),
            })?;
            return nonzero("VOYAGE_TIMEOUT_SECS", n);
        }
    }
    nonzero("[models].voyage_timeout_secs", cfg.voyage_timeout_secs)
}

/// Resolve the Voyage API base-URL override.
///
/// Precedence: `MIDNIGHT_MANUAL_VOYAGE_BASE_URL` env > `[models].voyage_base_url`
/// config > `None` (use the baked-in default). Empty values fall through.
/// Free-form URL — not validated here.
#[must_use]
pub fn resolve_voyage_base_url(cfg: &ModelsConfig, env: &impl ConfigEnv) -> Option<String> {
    env.var("MIDNIGHT_MANUAL_VOYAGE_BASE_URL")
        .filter(|s| !s.is_empty())
        .or_else(|| cfg.voyage_base_url.clone().filter(|s| !s.is_empty()))
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
    /// the rerank event's `placement` field. This is the single source of truth
    /// shared by the CLI and MCP clients so the placement label stays
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

/// Resolve rerank placement with precedence flag > env > config > key-detection.
///
/// `auto` is a recognized sentinel that defers to key detection (a Voyage key
/// present => local BYOK, absent => server).
///
/// # Errors
/// [`ConfigError::InvalidValue`] if the authoritative layer holds a value that
/// is neither `local`/`server`/`off`/`auto`.
pub fn resolve_rerank_placement(
    flag: Option<&str>,
    cfg: &RerankConfig,
    env: &impl ConfigEnv,
    has_voyage_key: bool,
) -> Result<RerankPlacement, ConfigError> {
    resolve_bounded(
        [
            ("the --rerank flag", flag.map(str::to_owned)),
            ("MIDNIGHT_MANUAL_RERANK", env.var("MIDNIGHT_MANUAL_RERANK")),
            ("[rerank].location", cfg.location.clone()),
        ],
        "local, server, off, auto",
        &["auto"],
        |s| match s {
            "local" => Some(RerankPlacement::Local),
            "server" => Some(RerankPlacement::Server),
            "off" => Some(RerankPlacement::Off),
            _ => None,
        },
        || {
            if has_voyage_key {
                RerankPlacement::Local
            } else {
                RerankPlacement::Server
            }
        },
    )
}

/// Resolve the rerank model: flag > `MIDNIGHT_MANUAL_RERANK_MODEL` env >
/// config > `rerank-2.5`. Returns a model variant only (placement handles
/// "off").
///
/// # Errors
/// [`ConfigError::InvalidValue`] if the authoritative layer holds a value that
/// is neither `rerank-2.5` nor `rerank-2.5-lite`.
pub fn resolve_rerank_model(
    flag: Option<&str>,
    cfg: &RerankConfig,
    env: &impl ConfigEnv,
) -> Result<crate::rerank::RerankParam, ConfigError> {
    use crate::rerank::RerankParam;
    resolve_bounded(
        [
            ("the --rerank-model flag", flag.map(str::to_owned)),
            ("MIDNIGHT_MANUAL_RERANK_MODEL", env.var("MIDNIGHT_MANUAL_RERANK_MODEL")),
            ("[rerank].model", cfg.model.clone()),
        ],
        "rerank-2.5, rerank-2.5-lite",
        &[],
        |s| match s {
            "rerank-2.5" => Some(RerankParam::Rerank25),
            "rerank-2.5-lite" => Some(RerankParam::Rerank25Lite),
            _ => None,
        },
        || RerankParam::Rerank25,
    )
}

/// Resolve a bounded (closed-set) string setting across flag > env > config.
///
/// The first layer whose trimmed value is non-empty and not a recognized
/// `sentinel` is authoritative: it is parsed (case-insensitively) and, on
/// failure, yields [`ConfigError::InvalidValue`] rather than falling through.
/// Empty/absent/sentinel layers are skipped. If every layer defers, `default`
/// is used.
fn resolve_bounded<T>(
    layers: [(&'static str, Option<String>); 3],
    expected: &str,
    sentinels: &[&str],
    parse: impl Fn(&str) -> Option<T>,
    default: impl FnOnce() -> T,
) -> Result<T, ConfigError> {
    for (layer_label, raw) in layers {
        let Some(raw) = raw else { continue };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if sentinels.contains(&lower.as_str()) {
            continue;
        }
        return parse(&lower).ok_or_else(|| ConfigError::InvalidValue {
            location: (*layer_label).to_owned(),
            value: trimmed.to_owned(),
            expected: expected.to_owned(),
        });
    }
    Ok(default())
}

/// Resolve the MCP client security level: flag > `MIDNIGHT_MANUAL_SECURITY`
/// env > `[security].level` config > default
/// [`crate::injection::SecurityLevel::Moderate`].
///
/// A non-empty unrecognized value at the authoritative layer is an error
/// (does not silently downgrade). Empty/absent layers fall through.
///
/// # Errors
/// Returns [`ConfigError::InvalidValue`] if the authoritative layer holds an
/// unrecognized value.
pub fn resolve_security_level(
    flag: Option<&str>,
    cfg: &SecurityConfig,
    env: &impl ConfigEnv,
) -> Result<crate::injection::SecurityLevel, ConfigError> {
    use std::str::FromStr as _;

    use crate::injection::SecurityLevel;

    resolve_bounded(
        [
            ("the --security flag", flag.map(str::to_owned)),
            ("MIDNIGHT_MANUAL_SECURITY", env.var("MIDNIGHT_MANUAL_SECURITY")),
            ("[security].level", cfg.level.clone()),
        ],
        "disabled, low, moderate, high, strict",
        &[],
        |s| SecurityLevel::from_str(s).ok(),
        SecurityLevel::default,
    )
}

/// Resolve the Gauge telemetry endpoint.
///
/// Precedence: `MIDNIGHT_MANUAL_GAUGE_ENDPOINT` env > `[telemetry].endpoint`
/// config > baked-in [`DEFAULT_TELEMETRY_ENDPOINT`]. An empty value at any
/// level falls through to the next source.
#[must_use]
pub fn resolve_telemetry_endpoint(cfg: &TelemetryConfig, env: &impl ConfigEnv) -> String {
    env.var("MIDNIGHT_MANUAL_GAUGE_ENDPOINT")
        .filter(|s| !s.is_empty())
        .or_else(|| Some(cfg.endpoint.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_else(default_telemetry_endpoint)
}

/// Resolve admin-command visibility (D23 / FR-066).
///
/// Precedence: `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS` env (truthy-set
/// `{1,true,TRUE,yes,YES}`) > `[cli].show_admin_cmds` config > hidden. A set
/// env var is authoritative — a non-truthy value (`0`, `no`, …) resolves to
/// `false` and does NOT fall through to config. This is the single source of
/// truth for both `mnm --help` visibility and the `doctor` report.
#[must_use]
pub fn resolve_show_admin_cmds(cfg: &CliConfig, env: &impl ConfigEnv) -> bool {
    if let Some(v) = env.var("MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS") {
        return matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES");
    }
    cfg.show_admin_cmds
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
    /// A recognized setting was given a non-empty value that is not valid.
    #[error("invalid value `{value}` for {location}: expected one of {expected}")]
    InvalidValue {
        /// Human label for where the bad value came from (env var, flag, or config field).
        location: String,
        /// The offending value (trimmed, original case).
        value: String,
        /// Comma-separated list of accepted values.
        expected: String,
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
    fn resolve_voyage_timeout_precedence_and_invalid() {
        let cfg = ModelsConfig { voyage_timeout_secs: 90, ..Default::default() };
        let env = FakeEnv::default().set("VOYAGE_TIMEOUT_SECS", "60");

        assert_eq!(resolve_voyage_timeout_secs(Some(45), &cfg, &env).unwrap(), 45);
        assert_eq!(resolve_voyage_timeout_secs(None, &cfg, &env).unwrap(), 60);

        let empty = FakeEnv::default();
        assert_eq!(resolve_voyage_timeout_secs(None, &cfg, &empty).unwrap(), 90);

        // Empty env falls through to config.
        let env_empty = FakeEnv::default().set("VOYAGE_TIMEOUT_SECS", "");
        assert_eq!(resolve_voyage_timeout_secs(None, &cfg, &env_empty).unwrap(), 90);

        // Non-numeric env is loud.
        let env_garbage = FakeEnv::default().set("VOYAGE_TIMEOUT_SECS", "abc");
        assert!(resolve_voyage_timeout_secs(None, &cfg, &env_garbage).is_err());

        // Zero is loud at every layer.
        assert!(resolve_voyage_timeout_secs(Some(0), &cfg, &empty).is_err());
        let env_zero = FakeEnv::default().set("VOYAGE_TIMEOUT_SECS", "0");
        assert!(resolve_voyage_timeout_secs(None, &cfg, &env_zero).is_err());
        let zero_cfg = ModelsConfig { voyage_timeout_secs: 0, ..Default::default() };
        assert!(resolve_voyage_timeout_secs(None, &zero_cfg, &empty).is_err());

        // Absent everywhere -> serde default (120) lives in the config field, so a
        // default ModelsConfig resolves to 120.
        assert_eq!(
            resolve_voyage_timeout_secs(None, &ModelsConfig::default(), &empty).unwrap(),
            120
        );
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
    fn resolve_rerank_placement_precedence_auto_and_invalid() {
        let cfg = RerankConfig { location: Some("off".into()), model: None };
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_RERANK", "server");
        assert_eq!(
            resolve_rerank_placement(Some("local"), &cfg, &env, false).unwrap(),
            RerankPlacement::Local
        );
        assert_eq!(resolve_rerank_placement(None, &cfg, &env, true).unwrap(), RerankPlacement::Server);

        // `auto` defers to key detection (key => Local, no key => Server).
        let empty = RerankConfig::default();
        let no_env = FakeEnv::default();
        assert_eq!(resolve_rerank_placement(Some("auto"), &empty, &no_env, true).unwrap(), RerankPlacement::Local);
        assert_eq!(resolve_rerank_placement(None, &empty, &no_env, false).unwrap(), RerankPlacement::Server);

        // A genuine typo is loud (does not silently pick a placement).
        let err = resolve_rerank_placement(Some("servr"), &empty, &no_env, true).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidValue { .. }));
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
    fn resolve_rerank_model_precedence_and_invalid() {
        use crate::rerank::RerankParam;
        let cfg = RerankConfig { location: None, model: Some("rerank-2.5-lite".into()) };
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_RERANK_MODEL", "rerank-2.5");
        assert_eq!(
            resolve_rerank_model(Some("rerank-2.5-lite"), &cfg, &env).unwrap(),
            RerankParam::Rerank25Lite
        );
        assert_eq!(resolve_rerank_model(None, &cfg, &env).unwrap(), RerankParam::Rerank25);
        let no_env = FakeEnv::default();
        assert_eq!(
            resolve_rerank_model(None, &RerankConfig::default(), &no_env).unwrap(),
            RerankParam::Rerank25
        );
        // Typo is loud.
        assert!(resolve_rerank_model(Some("rerank-3"), &RerankConfig::default(), &no_env).is_err());
    }

    #[test]
    fn resolve_security_level_precedence_and_default() {
        use crate::injection::SecurityLevel;

        let cfg = SecurityConfig { level: Some("high".into()) };
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_SECURITY", "strict");

        // flag > env > config.
        assert_eq!(resolve_security_level(Some("low"), &cfg, &env).unwrap(), SecurityLevel::Low);
        assert_eq!(resolve_security_level(None, &cfg, &env).unwrap(), SecurityLevel::Strict);

        let no_env = FakeEnv::default();
        assert_eq!(resolve_security_level(None, &cfg, &no_env).unwrap(), SecurityLevel::High);

        // An empty flag is treated as absent and falls through to config `high`.
        assert_eq!(resolve_security_level(Some(""), &cfg, &no_env).unwrap(), SecurityLevel::High);

        // Nothing anywhere -> default Moderate.
        let empty = SecurityConfig::default();
        assert_eq!(resolve_security_level(None, &empty, &no_env).unwrap(), SecurityLevel::Moderate);

        // Case-insensitive.
        let env_caps = FakeEnv::default().set("MIDNIGHT_MANUAL_SECURITY", "STRICT");
        assert_eq!(resolve_security_level(None, &empty, &env_caps).unwrap(), SecurityLevel::Strict);
    }

    #[test]
    fn resolve_security_level_invalid_value_is_loud() {
        let empty = SecurityConfig::default();
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_SECURITY", "strct");
        let err = resolve_security_level(None, &empty, &env).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidValue { .. }));
        let msg = err.to_string();
        assert!(msg.contains("strct") && msg.contains("MIDNIGHT_MANUAL_SECURITY"));

        // An invalid value does NOT fall through to config.
        let cfg = SecurityConfig { level: Some("high".into()) };
        assert!(resolve_security_level(None, &cfg, &env).is_err());

        // Empty env still falls through (treated as absent).
        let env_empty = FakeEnv::default().set("MIDNIGHT_MANUAL_SECURITY", "");
        assert_eq!(
            resolve_security_level(None, &cfg, &env_empty).unwrap(),
            crate::injection::SecurityLevel::High
        );
    }

    #[test]
    fn telemetry_endpoint_defaults_to_baked_in() {
        let cfg = TelemetryConfig::default();
        assert_eq!(cfg.endpoint, DEFAULT_TELEMETRY_ENDPOINT);
        assert_eq!(cfg.endpoint, "https://gauge-telemetry.fly.dev");
    }

    #[test]
    fn telemetry_endpoint_env_overrides_config() {
        struct E;
        impl ConfigEnv for E {
            fn var(&self, name: &str) -> Option<String> {
                (name == "MIDNIGHT_MANUAL_GAUGE_ENDPOINT")
                    .then(|| "https://localhost:9000".to_owned())
            }
        }
        let cfg = TelemetryConfig {
            enabled: true,
            endpoint: "https://from-config".into(),
        };
        assert_eq!(resolve_telemetry_endpoint(&cfg, &E), "https://localhost:9000");
    }

    #[test]
    fn telemetry_endpoint_falls_through_empty_env_to_config() {
        struct E;
        impl ConfigEnv for E {
            fn var(&self, _: &str) -> Option<String> {
                Some(String::new())
            }
        }
        let cfg = TelemetryConfig {
            enabled: true,
            endpoint: "https://from-config".into(),
        };
        assert_eq!(resolve_telemetry_endpoint(&cfg, &E), "https://from-config");
    }

    #[test]
    fn resolve_voyage_base_url_env_over_config() {
        let cfg = ModelsConfig { voyage_base_url: Some("https://from-config".into()), ..Default::default() };
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_VOYAGE_BASE_URL", "https://from-env");
        assert_eq!(resolve_voyage_base_url(&cfg, &env).as_deref(), Some("https://from-env"));

        let no_env = FakeEnv::default();
        assert_eq!(resolve_voyage_base_url(&cfg, &no_env).as_deref(), Some("https://from-config"));

        // Empty env falls through to config; nothing anywhere -> None.
        let env_empty = FakeEnv::default().set("MIDNIGHT_MANUAL_VOYAGE_BASE_URL", "");
        assert_eq!(resolve_voyage_base_url(&cfg, &env_empty).as_deref(), Some("https://from-config"));
        assert_eq!(resolve_voyage_base_url(&ModelsConfig::default(), &no_env), None);
    }

    #[test]
    fn models_config_voyage_base_url_roundtrips() {
        let toml_src =
            "embedding = \"voyage-context-3\"\nvoyage_base_url = \"https://proxy.example/v1\"\n";
        let m: ModelsConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(m.voyage_base_url.as_deref(), Some("https://proxy.example/v1"));
        assert!(ModelsConfig::default().voyage_base_url.is_none());
    }

    #[test]
    fn resolve_show_admin_cmds_env_truthy_then_config() {
        let cfg_on = CliConfig { show_admin_cmds: true };
        let cfg_off = CliConfig { show_admin_cmds: false };

        // Env set to a truthy token wins regardless of config.
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS", "1");
        assert!(resolve_show_admin_cmds(&cfg_off, &env));

        // Env set to a non-truthy token is authoritative (does NOT fall through).
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS", "0");
        assert!(!resolve_show_admin_cmds(&cfg_on, &env));

        // Env unset -> config field decides.
        let empty = FakeEnv::default();
        assert!(resolve_show_admin_cmds(&cfg_on, &empty));
        assert!(!resolve_show_admin_cmds(&cfg_off, &empty));
    }
}

//! Server-side configuration loaded from env at startup (D22, FR-009).

use std::env;

use mn_core::scoring_policy::ScoringPolicy;
use thiserror::Error;

/// All the knobs the server reads at boot.
// A config struct legitimately carries many independent on/off knobs; grouping
// them into sub-structs would add ceremony without clarifying anything.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// libpq-style URL for the Postgres connection.
    pub database_url: String,
    /// Listen port. Defaults to 8080.
    pub port: u16,
    /// When `false`, skip the automatic migration runner at startup (D22).
    pub auto_migrate: bool,
    /// `MIDNIGHT_MANUAL_USER_STORE` — the in-memory TOML body of the user
    /// store. When `None`, the auth endpoints (FR-056 challenge / verify)
    /// return 503 `service_unavailable` rather than refusing boot.
    pub user_store_body: Option<String>,
    /// `MIDNIGHT_MANUAL_JWT_SECRET` — HS256 signing key (32+ bytes).
    /// Same optionality as `user_store_body`.
    pub jwt_secret: Option<Vec<u8>>,
    /// `MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_ID` — GitHub OAuth App client id
    /// (FR-062 / FR-117). When any of the three GitHub OAuth env values is
    /// absent the `/v1/auth/github/*` endpoints return 503.
    pub github_oauth_client_id: Option<String>,
    /// `MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET` — GitHub OAuth App secret.
    pub github_oauth_client_secret: Option<String>,
    /// `MIDNIGHT_MANUAL_GITHUB_OAUTH_REDIRECT_URL` — the public URL of this
    /// server's `/v1/auth/github/callback` endpoint, as registered in the
    /// GitHub OAuth App. Required when the other GitHub OAuth env vars are
    /// set.
    pub github_oauth_redirect_url: Option<String>,
    /// `MIDNIGHT_MANUAL_GITHUB_ORG` — only members of this org receive a
    /// read-uplift bearer (FR-062). When unset the GitHub OAuth endpoints
    /// also 503 even if the OAuth credentials are set: an org-bound flow
    /// without an org gate is a foot-gun.
    pub github_org: Option<String>,
    /// `MIDNIGHT_MANUAL_READ_TOKEN_TTL_DAYS` — read-uplift JWT TTL in days
    /// (FR-117). Defaults to 30; clamped to `[1, 90]`.
    pub read_token_ttl_days: i64,
    /// `MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS` — rolling window after
    /// which `telemetry_event_raw` rows are rolled up into
    /// `telemetry_aggregate_daily` and deleted (FR-110). Defaults to 7;
    /// clamped to `[1, 365]`.
    pub telemetry_raw_retention_days: i64,
    /// GitHub API base URL. Defaults to `https://api.github.com`. Tests
    /// override this to point at a mock server.
    pub github_api_base_url: String,
    /// GitHub OAuth authorize URL base. Defaults to
    /// `https://github.com/login/oauth/authorize`. Tests override.
    pub github_authorize_url: String,
    /// GitHub OAuth token-exchange URL. Defaults to
    /// `https://github.com/login/oauth/access_token`. Tests override.
    pub github_token_url: String,
    /// `MIDNIGHT_MANUAL_SOURCE_RETIREMENT_ENABLED` — when true, spawns the
    /// background source-retention sweep that hard-deletes sources whose
    /// `retired_at` is older than the grace window (Phase 13). Defaults to
    /// `true` in production. Tests set this to `false` so the sweep never
    /// races against fixture data, and instead drive `sweep_once` directly.
    pub source_retirement_enabled: bool,
    /// `MIDNIGHT_MANUAL_SOURCE_RETIREMENT_GRACE_HOURS` — how long a soft-
    /// retired source lingers before the sweep is allowed to hard-delete it.
    /// Defaults to 24 hours (the same default the spec uses for the
    /// source-version sweep). Clamped to `[1, 24 * 365]`.
    pub source_retirement_grace_hours: i64,
    /// `MIDNIGHT_MANUAL_SOURCE_RETIREMENT_INTERVAL_MINUTES` — how often the
    /// background daemon ticks. Defaults to 60 minutes; clamped to
    /// `[1, 24 * 60]`.
    pub source_retirement_interval_minutes: u64,
    /// `MIDNIGHT_MANUAL_SOURCE_VERSION_SWEEP_GRACE_HOURS` — how long a
    /// demoted (`inactive` or `retired`) source_version that falls outside
    /// its source's `retention_count` window must wait before the sweep is
    /// allowed to hard-delete it (FR-063, Phase 14). Defaults to 24 hours;
    /// clamped to `[1, 24 * 365]`. The daemon runs all three passes on
    /// the same tick.
    pub source_version_sweep_grace_hours: i64,
    /// `MIDNIGHT_MANUAL_ABORT_GRACE_HOURS` — how long an `aborted` ingest
    /// run lingers before the sweep is allowed to hard-delete it (FR-063,
    /// Phase 15). Defaults to 1 hour (the spec's
    /// `MIDNIGHT_MANUAL_ABORT_GRACE` default). Clamped to `[1, 24 * 365]`.
    pub abort_grace_hours: i64,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_ENABLED` — master switch (Phase 17).
    /// Default `false` so `Default::default()` (used by tests) never
    /// throttles; production opts in.
    pub rate_limit_enabled: bool,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_ANONYMOUS_RPS` — per-IP requests/sec for
    /// the anonymous tier. Default 10.
    pub rate_limit_anonymous_rps: u32,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_UPLIFT_RPS` — per-user requests/sec for
    /// GitHub-SSO read-uplift tokens. Default 60.
    pub rate_limit_uplift_rps: u32,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_ADMIN_RPS` — per-user requests/sec for
    /// admin-tier tokens. Default 1000.
    pub rate_limit_admin_rps: u32,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_CLIENT_IP_HEADER` — header carrying the
    /// real client IP behind the proxy. Default `fly-client-ip`.
    pub rate_limit_client_ip_header: String,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_OVERRIDE_REFRESH_SECS` — override-cache
    /// refresh interval. Default 30.
    pub rate_limit_override_refresh_secs: u64,
    /// `MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST` — the per-request cap on
    /// `queries.length` for `POST /v1/search` (D25, EC-88). Default 10;
    /// clamped to the hard ceiling `[1, 50]`.
    pub max_queries_per_request: u32,
    /// Resolved confidence-scoring policy (D24 / US6). Loaded once at boot
    /// from the TOML file at `MIDNIGHT_MANUAL_SCORING_POLICY`; the compiled-in
    /// [`ScoringPolicy::default`] is used when the env var is unset. An invalid
    /// file fails startup (Constitution VI / VIII).
    pub scoring_policy: ScoringPolicy,
    /// `VOYAGE_API_KEY` — VoyageAI API key for server-side embedding. When
    /// `None`, `POST /v1/embeddings` returns 503; there is no local embedder
    /// fallback (the fastembed corpus embedder was retired when the corpus
    /// moved to VoyageAI).
    pub voyage_api_key: Option<String>,
    /// `MIDNIGHT_MANUAL_VOYAGE_MODEL` — VoyageAI embedding model name for the
    /// flat `/v1/embeddings` API (the `type=code` embedder). Defaults to
    /// `"voyage-code-3"`.
    pub voyage_model: String,
    /// `MIDNIGHT_MANUAL_VOYAGE_CONTEXT_MODEL` — VoyageAI contextualized
    /// embedding model name (the `type=general` embedder, served via
    /// `/v1/contextualizedembeddings`). Defaults to `"voyage-context-3"`.
    pub voyage_context_model: String,
    /// `MIDNIGHT_MANUAL_CODE_MODEL` — registry wire id (`name@revision`) of
    /// the corpus's code-embedding model, resolved at boot against the
    /// `embedding_model` registry. Defaults to `"voyage-code-3@1"`.
    pub code_model_wire: String,
    /// `MIDNIGHT_MANUAL_VOYAGE_DIM` — output dimension for VoyageAI embeddings.
    /// Defaults to 1024.
    pub voyage_output_dimension: u32,
    /// `MIDNIGHT_MANUAL_VOYAGE_DTYPE` — output dtype for VoyageAI embeddings.
    /// Defaults to `"float"`.
    pub voyage_output_dtype: String,
    /// `MIDNIGHT_MANUAL_TOKEN_LIMIT_ANON_HOURLY` — hourly token budget for
    /// anonymous callers. Defaults to 2 000.
    pub token_limit_anon_hourly: u64,
    /// `MIDNIGHT_MANUAL_TOKEN_LIMIT_ANON_DAILY` — daily token budget for
    /// anonymous callers. Defaults to 20 000.
    pub token_limit_anon_daily: u64,
    /// `MIDNIGHT_MANUAL_TOKEN_LIMIT_UPLIFT_HOURLY` — hourly token budget for
    /// read-uplift (GitHub SSO) callers. Defaults to 4 000.
    pub token_limit_uplift_hourly: u64,
    /// `MIDNIGHT_MANUAL_TOKEN_LIMIT_UPLIFT_DAILY` — daily token budget for
    /// read-uplift callers. Defaults to 40 000.
    pub token_limit_uplift_daily: u64,
    /// `MIDNIGHT_MANUAL_TOKEN_LIMIT_ADMIN_HOURLY` — hourly token budget for
    /// admin-tier callers. Defaults to 500 000.
    pub token_limit_admin_hourly: u64,
    /// `MIDNIGHT_MANUAL_TOKEN_LIMIT_ADMIN_DAILY` — daily token budget for
    /// admin-tier callers. Defaults to 100 000 000.
    pub token_limit_admin_daily: u64,
    /// `MIDNIGHT_MANUAL_TOKEN_SNAPSHOT_SECS` — interval at which the token-
    /// usage counters are flushed to the store. Defaults to 300 s (5 min);
    /// clamped to `[1, ∞)`.
    pub token_snapshot_secs: u64,
    /// `MIDNIGHT_MANUAL_TOKEN_LIMIT_GLOBAL` — site-wide token ceiling over
    /// `token_limit_global_window_secs`, an anti-Sybil backstop applied to
    /// non-admin tiers. Defaults to 10_000_000. `u64::MAX` disables it.
    pub token_limit_global: u64,
    /// `MIDNIGHT_MANUAL_TOKEN_LIMIT_GLOBAL_WINDOW_SECS` — rolling window for the
    /// global cap. Defaults to 10_800 s (3 h).
    pub token_limit_global_window_secs: u64,
}

impl Default for ServerConfig {
    /// Test-friendly default. Production callers MUST go through
    /// [`ServerConfig::from_env`] — the default's `database_url` is empty
    /// (intentionally invalid) so it can't accidentally boot a server.
    fn default() -> Self {
        Self {
            database_url: String::new(),
            port: 8080,
            auto_migrate: false,
            user_store_body: None,
            jwt_secret: None,
            github_oauth_client_id: None,
            github_oauth_client_secret: None,
            github_oauth_redirect_url: None,
            github_org: None,
            read_token_ttl_days: 30,
            telemetry_raw_retention_days: 7,
            github_api_base_url: "https://api.github.com".into(),
            github_authorize_url: "https://github.com/login/oauth/authorize".into(),
            github_token_url: "https://github.com/login/oauth/access_token".into(),
            source_retirement_enabled: false,
            source_retirement_grace_hours: 24,
            source_retirement_interval_minutes: 60,
            source_version_sweep_grace_hours: 24,
            abort_grace_hours: 1,
            rate_limit_enabled: false,
            rate_limit_anonymous_rps: 10,
            rate_limit_uplift_rps: 60,
            rate_limit_admin_rps: 1000,
            rate_limit_client_ip_header: "fly-client-ip".into(),
            rate_limit_override_refresh_secs: 30,
            max_queries_per_request: 10,
            scoring_policy: ScoringPolicy::default(),
            voyage_api_key: None,
            voyage_model: "voyage-code-3".into(),
            voyage_context_model: "voyage-context-3".into(),
            code_model_wire: "voyage-code-3@1".into(),
            voyage_output_dimension: 1024,
            voyage_output_dtype: "float".into(),
            token_limit_anon_hourly: 2_000,
            token_limit_anon_daily: 20_000,
            token_limit_uplift_hourly: 4_000,
            token_limit_uplift_daily: 40_000,
            token_limit_admin_hourly: 500_000,
            token_limit_admin_daily: 100_000_000,
            token_snapshot_secs: 300,
            token_limit_global: 10_000_000,
            token_limit_global_window_secs: 10_800,
        }
    }
}

impl ServerConfig {
    /// Read the config from process env. `DATABASE_URL` is required; everything
    /// else has a default.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Missing`] if `DATABASE_URL` is unset.
    // A flat sequence of `env::var(...)` reads — splitting it up would only
    // scatter the env-var surface across helpers.
    #[allow(clippy::too_many_lines)]
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url =
            env::var("DATABASE_URL").map_err(|_| ConfigError::Missing("DATABASE_URL"))?;
        let port = env::var("PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8080);
        let auto_migrate = env::var("MIDNIGHT_MANUAL_AUTO_MIGRATE")
            .map(|v| !matches!(v.as_str(), "0" | "false" | "no"))
            .unwrap_or(true);
        let user_store_body = env::var("MIDNIGHT_MANUAL_USER_STORE").ok();
        let jwt_secret = env::var("MIDNIGHT_MANUAL_JWT_SECRET")
            .ok()
            .map(String::into_bytes);
        let github_oauth_client_id = env::var("MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_ID").ok();
        let github_oauth_client_secret =
            env::var("MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET").ok();
        let github_oauth_redirect_url = env::var("MIDNIGHT_MANUAL_GITHUB_OAUTH_REDIRECT_URL").ok();
        let github_org = env::var("MIDNIGHT_MANUAL_GITHUB_ORG").ok();
        let read_token_ttl_days = env::var("MIDNIGHT_MANUAL_READ_TOKEN_TTL_DAYS")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .map_or(30, |v| v.clamp(1, 90));
        let telemetry_raw_retention_days = env::var("MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .map_or(7, |v| v.clamp(1, 365));
        let source_retirement_enabled = env::var("MIDNIGHT_MANUAL_SOURCE_RETIREMENT_ENABLED")
            .map(|v| !matches!(v.as_str(), "0" | "false" | "no"))
            .unwrap_or(true);
        let source_retirement_grace_hours =
            env::var("MIDNIGHT_MANUAL_SOURCE_RETIREMENT_GRACE_HOURS")
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .map_or(24, |v| v.clamp(1, 24 * 365));
        let source_retirement_interval_minutes =
            env::var("MIDNIGHT_MANUAL_SOURCE_RETIREMENT_INTERVAL_MINUTES")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map_or(60, |v| v.clamp(1, 24 * 60));
        let source_version_sweep_grace_hours =
            env::var("MIDNIGHT_MANUAL_SOURCE_VERSION_SWEEP_GRACE_HOURS")
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .map_or(24, |v| v.clamp(1, 24 * 365));
        let abort_grace_hours = env::var("MIDNIGHT_MANUAL_ABORT_GRACE_HOURS")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .map_or(1, |v| v.clamp(1, 24 * 365));
        let rate_limit_enabled = env::var("MIDNIGHT_MANUAL_RATE_LIMIT_ENABLED")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false);
        let rate_limit_anonymous_rps = env::var("MIDNIGHT_MANUAL_RATE_LIMIT_ANONYMOUS_RPS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .map_or(10, |v| v.max(1));
        let rate_limit_uplift_rps = env::var("MIDNIGHT_MANUAL_RATE_LIMIT_UPLIFT_RPS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .map_or(60, |v| v.max(1));
        let rate_limit_admin_rps = env::var("MIDNIGHT_MANUAL_RATE_LIMIT_ADMIN_RPS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .map_or(1000, |v| v.max(1));
        let rate_limit_client_ip_header = env::var("MIDNIGHT_MANUAL_RATE_LIMIT_CLIENT_IP_HEADER")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "fly-client-ip".to_owned());
        let rate_limit_override_refresh_secs =
            env::var("MIDNIGHT_MANUAL_RATE_LIMIT_OVERRIDE_REFRESH_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map_or(30, |v| v.max(1));
        let max_queries_per_request = env::var("MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .map_or(10, |v| v.clamp(1, 50));
        let scoring_policy = load_scoring_policy()?;
        let voyage_api_key = env::var("VOYAGE_API_KEY").ok().filter(|s| !s.is_empty());
        let voyage_model = env::var("MIDNIGHT_MANUAL_VOYAGE_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "voyage-code-3".into());
        let voyage_context_model = env::var("MIDNIGHT_MANUAL_VOYAGE_CONTEXT_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "voyage-context-3".into());
        let code_model_wire = env::var("MIDNIGHT_MANUAL_CODE_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "voyage-code-3@1".into());
        let voyage_output_dimension: u32 = env::var("MIDNIGHT_MANUAL_VOYAGE_DIM")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1024);
        let voyage_output_dtype = env::var("MIDNIGHT_MANUAL_VOYAGE_DTYPE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "float".into());
        let tl = |name: &str, default: u64| -> u64 {
            env::var(name)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default)
        };
        let token_limit_anon_hourly = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_ANON_HOURLY", 2_000);
        let token_limit_anon_daily = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_ANON_DAILY", 20_000);
        let token_limit_uplift_hourly = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_UPLIFT_HOURLY", 4_000);
        let token_limit_uplift_daily = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_UPLIFT_DAILY", 40_000);
        let token_limit_admin_hourly = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_ADMIN_HOURLY", 500_000);
        let token_limit_admin_daily = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_ADMIN_DAILY", 100_000_000);
        let token_snapshot_secs = env::var("MIDNIGHT_MANUAL_TOKEN_SNAPSHOT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map_or(300, |v: u64| v.max(1));
        let token_limit_global = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_GLOBAL", 10_000_000);
        let token_limit_global_window_secs =
            env::var("MIDNIGHT_MANUAL_TOKEN_LIMIT_GLOBAL_WINDOW_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .map_or(10_800, |v: u64| v.max(60));
        Ok(Self {
            database_url,
            port,
            auto_migrate,
            user_store_body,
            jwt_secret,
            github_oauth_client_id,
            github_oauth_client_secret,
            github_oauth_redirect_url,
            github_org,
            read_token_ttl_days,
            telemetry_raw_retention_days,
            github_api_base_url: "https://api.github.com".into(),
            github_authorize_url: "https://github.com/login/oauth/authorize".into(),
            github_token_url: "https://github.com/login/oauth/access_token".into(),
            source_retirement_enabled,
            source_retirement_grace_hours,
            source_retirement_interval_minutes,
            source_version_sweep_grace_hours,
            abort_grace_hours,
            rate_limit_enabled,
            rate_limit_anonymous_rps,
            rate_limit_uplift_rps,
            rate_limit_admin_rps,
            rate_limit_client_ip_header,
            rate_limit_override_refresh_secs,
            max_queries_per_request,
            scoring_policy,
            voyage_api_key,
            voyage_model,
            voyage_context_model,
            code_model_wire,
            voyage_output_dimension,
            voyage_output_dtype,
            token_limit_anon_hourly,
            token_limit_anon_daily,
            token_limit_uplift_hourly,
            token_limit_uplift_daily,
            token_limit_admin_hourly,
            token_limit_admin_daily,
            token_snapshot_secs,
            token_limit_global,
            token_limit_global_window_secs,
        })
    }
}

/// Resolve the confidence-scoring policy from `MIDNIGHT_MANUAL_SCORING_POLICY`.
///
/// The env var, when set, names a TOML file path. Absent → compiled-in
/// [`ScoringPolicy::default`]. A path that can't be read or whose TOML is
/// invalid fails startup (fail-fast, Constitution VIII).
fn load_scoring_policy() -> Result<ScoringPolicy, ConfigError> {
    let path = env::var("MIDNIGHT_MANUAL_SCORING_POLICY")
        .ok()
        .filter(|s| !s.is_empty());
    resolve_scoring_policy(path)
}

/// Resolve a scoring policy from an optional file path. Factored out of
/// [`load_scoring_policy`] so it can be unit-tested without mutating process env.
fn resolve_scoring_policy(path: Option<String>) -> Result<ScoringPolicy, ConfigError> {
    let Some(path) = path else {
        return Ok(ScoringPolicy::default());
    };
    let body = std::fs::read_to_string(&path)
        .map_err(|e| ConfigError::ScoringPolicyRead { path: path.clone(), source: e })?;
    ScoringPolicy::parse(&body)
        .map_err(|e| ConfigError::ScoringPolicyParse { path, message: e.to_string() })
}

/// All the ways config loading can fail.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A required env var was unset.
    #[error("required env var `{0}` is not set")]
    Missing(&'static str),
    /// The scoring-policy file named by `MIDNIGHT_MANUAL_SCORING_POLICY` could
    /// not be read.
    #[error("could not read scoring policy at `{path}`: {source}")]
    ScoringPolicyRead {
        /// The path that failed to read.
        path: String,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The scoring-policy file was read but failed validation.
    #[error("invalid scoring policy at `{path}`: {message}")]
    ScoringPolicyParse {
        /// The path that failed to parse.
        path: String,
        /// The parse/validation error.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_defaults_are_disabled() {
        let c = ServerConfig::default();
        assert!(!c.rate_limit_enabled);
        assert_eq!(c.rate_limit_anonymous_rps, 10);
        assert_eq!(c.rate_limit_uplift_rps, 60);
        assert_eq!(c.rate_limit_admin_rps, 1000);
        assert_eq!(c.rate_limit_client_ip_header, "fly-client-ip");
        assert_eq!(c.rate_limit_override_refresh_secs, 30);
        assert_eq!(c.max_queries_per_request, 10);
    }

    #[test]
    fn token_limit_defaults() {
        let c = ServerConfig::default();
        assert_eq!(c.token_limit_anon_hourly, 2_000);
        assert_eq!(c.token_limit_anon_daily, 20_000);
        assert_eq!(c.token_limit_uplift_hourly, 4_000);
        assert_eq!(c.token_limit_uplift_daily, 40_000);
        assert_eq!(c.token_limit_admin_hourly, 500_000);
        assert_eq!(c.token_limit_admin_daily, 100_000_000);
        assert_eq!(c.token_snapshot_secs, 300);
        assert_eq!(c.token_limit_global, 10_000_000);
        assert_eq!(c.token_limit_global_window_secs, 10_800);
        assert_eq!(c.voyage_model, "voyage-code-3");
        assert_eq!(c.voyage_output_dimension, 1024);
        assert_eq!(c.voyage_output_dtype, "float");
        assert!(c.voyage_api_key.is_none());
    }

    #[test]
    fn dual_embedding_model_defaults() {
        let c = ServerConfig::default();
        assert_eq!(c.voyage_context_model, "voyage-context-3");
        assert_eq!(c.code_model_wire, "voyage-code-3@1");
    }

    #[test]
    fn scoring_policy_defaults_when_unset() {
        assert_eq!(resolve_scoring_policy(None).unwrap(), ScoringPolicy::default());
        assert_eq!(ServerConfig::default().scoring_policy, ScoringPolicy::default());
    }

    #[test]
    fn scoring_policy_loads_from_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mnm-scoring-{}.toml", uuid::Uuid::new_v4()));
        let mut policy = ScoringPolicy::default();
        policy.blend.trust_weight = 0.7;
        policy.blend.relevance_weight = 0.3;
        std::fs::write(&path, toml::to_string(&policy).unwrap()).unwrap();
        let loaded = resolve_scoring_policy(Some(path.display().to_string())).unwrap();
        assert!((loaded.blend.trust_weight - 0.7).abs() < 1e-12);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn scoring_policy_invalid_fails_startup() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mnm-scoring-bad-{}.toml", uuid::Uuid::new_v4()));
        std::fs::write(&path, "schema_version = 1\nbogus_key = 9\n").unwrap();
        let err = resolve_scoring_policy(Some(path.display().to_string())).unwrap_err();
        assert!(matches!(err, ConfigError::ScoringPolicyParse { .. }));
        std::fs::remove_file(&path).ok();

        let missing = resolve_scoring_policy(Some("/nonexistent/mnm/policy.toml".to_owned()));
        assert!(matches!(missing.unwrap_err(), ConfigError::ScoringPolicyRead { .. }));
    }
}

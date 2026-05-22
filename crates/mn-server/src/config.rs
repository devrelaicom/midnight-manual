//! Server-side configuration loaded from env at startup (D22, FR-009).

use std::env;

use thiserror::Error;

/// All the knobs the server reads at boot.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// libpq-style URL for the Postgres connection.
    pub database_url: String,
    /// Listen port. Defaults to 8080.
    pub port: u16,
    /// When `false`, skip the automatic migration runner at startup (D22).
    pub auto_migrate: bool,
    /// When set, every search-or-chunk endpoint refuses unless the caller's
    /// `client_embedding_model` matches this canonical model identifier.
    /// Server resolves this from the active embedding_model row on boot.
    pub corpus_model: Option<String>,
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
    /// `MIDNIGHT_MANUAL_EMBEDDER_ENABLED` — when true, spawns the
    /// background embedder worker (Phase 11a). Defaults to true on
    /// production; tests opt out with `false` so the ONNX bundle never
    /// downloads inside CI.
    pub embedder_enabled: bool,
    /// `MIDNIGHT_MANUAL_EMBEDDER_INTERVAL_MS` — poll interval for the
    /// embedder worker. Defaults to 30000 (30s); clamped to `[1_000, 600_000]`.
    pub embedder_interval_ms: u64,
    /// `MIDNIGHT_MANUAL_EMBEDDER_BATCH_SIZE` — chunks per worker tick.
    /// Defaults to 16; clamped to `[1, 128]`.
    pub embedder_batch_size: i64,
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
            corpus_model: None,
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
            embedder_enabled: false,
            embedder_interval_ms: 30_000,
            embedder_batch_size: 16,
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
        let embedder_enabled = env::var("MIDNIGHT_MANUAL_EMBEDDER_ENABLED")
            .map(|v| !matches!(v.as_str(), "0" | "false" | "no"))
            .unwrap_or(true);
        let embedder_interval_ms = env::var("MIDNIGHT_MANUAL_EMBEDDER_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map_or(30_000, |v| v.clamp(1_000, 600_000));
        let embedder_batch_size = env::var("MIDNIGHT_MANUAL_EMBEDDER_BATCH_SIZE")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .map_or(16, |v| v.clamp(1, 128));
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
        Ok(Self {
            database_url,
            port,
            auto_migrate,
            corpus_model: None,
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
            embedder_enabled,
            embedder_interval_ms,
            embedder_batch_size,
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
        })
    }
}

/// All the ways config loading can fail.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A required env var was unset.
    #[error("required env var `{0}` is not set")]
    Missing(&'static str),
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
    }
}

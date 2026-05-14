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
    /// GitHub API base URL. Defaults to `https://api.github.com`. Tests
    /// override this to point at a mock server.
    pub github_api_base_url: String,
    /// GitHub OAuth authorize URL base. Defaults to
    /// `https://github.com/login/oauth/authorize`. Tests override.
    pub github_authorize_url: String,
    /// GitHub OAuth token-exchange URL. Defaults to
    /// `https://github.com/login/oauth/access_token`. Tests override.
    pub github_token_url: String,
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
            github_api_base_url: "https://api.github.com".into(),
            github_authorize_url: "https://github.com/login/oauth/authorize".into(),
            github_token_url: "https://github.com/login/oauth/access_token".into(),
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
            github_api_base_url: "https://api.github.com".into(),
            github_authorize_url: "https://github.com/login/oauth/authorize".into(),
            github_token_url: "https://github.com/login/oauth/access_token".into(),
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

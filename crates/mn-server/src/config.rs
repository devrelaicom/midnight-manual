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
        Ok(Self {
            database_url,
            port,
            auto_migrate,
            corpus_model: None,
            user_store_body,
            jwt_secret,
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

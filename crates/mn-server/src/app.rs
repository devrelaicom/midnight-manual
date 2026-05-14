//! axum app builder — wires routes, middleware, and shared state.

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::Router;
use mn_auth::{ChallengeStore, SigningSecret, UserStore};
use sqlx::PgPool;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

use crate::config::ServerConfig;
use crate::middleware::request_id;

/// Cap for inbound request bodies. The largest legitimate body today is
/// `POST /v1/search` carrying up to 50 query pairs × 768 dims × 4 bytes per
/// f32 ≈ 150 KiB plus JSON overhead. 1 MiB gives us headroom for future
/// fields without enabling a memory-exhaustion vector.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Per-handler shared state — clonable cheaply.
#[derive(Clone)]
pub struct AppState {
    /// Database pool.
    pub pool: PgPool,
    /// Server-side config snapshot.
    pub cfg: Arc<ServerConfig>,
    /// Auth subsystem — `None` when `MIDNIGHT_MANUAL_USER_STORE` and
    /// `MIDNIGHT_MANUAL_JWT_SECRET` are not both configured. Auth-bearing
    /// routes return 503 in that case rather than letting boot fail (so
    /// read-only deployments without an admin user-store still serve search).
    pub auth: Option<Arc<AuthState>>,
}

/// Resolved auth subsystem state — set once at boot when both the user
/// store and the JWT secret are present.
#[derive(Debug)]
pub struct AuthState {
    /// Parsed user store loaded from `MIDNIGHT_MANUAL_USER_STORE`.
    pub user_store: UserStore,
    /// HS256 signing secret from `MIDNIGHT_MANUAL_JWT_SECRET`.
    pub jwt_secret: SigningSecret,
    /// In-memory challenge nonce store (FR-056). One per process.
    pub challenges: ChallengeStore,
}

/// All the ways `AuthState` construction can fail at boot.
#[derive(Debug, thiserror::Error)]
pub enum AuthStateError {
    /// `MIDNIGHT_MANUAL_USER_STORE` parse failure.
    #[error("user store parse failed: {0}")]
    UserStore(#[from] mn_auth::UserStoreError),
    /// `MIDNIGHT_MANUAL_JWT_SECRET` was shorter than the 32-byte floor.
    #[error("jwt secret invalid: {0}")]
    JwtSecret(#[from] mn_auth::JwtError),
}

impl AuthState {
    /// Build the auth subsystem from raw env values. Returns `None` when
    /// either input is absent — auth endpoints then 503 cleanly.
    ///
    /// # Errors
    ///
    /// Returns [`AuthStateError`] when one input is present but malformed.
    /// "Missing" is allowed (read-only deployments); "garbage" is not.
    pub fn from_env_values(
        user_store_body: Option<&str>,
        jwt_secret_bytes: Option<&[u8]>,
    ) -> Result<Option<Self>, AuthStateError> {
        let (Some(body), Some(bytes)) = (user_store_body, jwt_secret_bytes) else {
            return Ok(None);
        };
        let user_store = UserStore::parse(body)?;
        let jwt_secret = SigningSecret::from_bytes(bytes.to_vec())?;
        Ok(Some(Self {
            user_store,
            jwt_secret,
            challenges: ChallengeStore::new(),
        }))
    }
}

/// Build the full axum app: routes + middleware + state.
///
/// # Errors
///
/// Returns [`AuthStateError`] if the auth env values are present but malformed
/// (`MIDNIGHT_MANUAL_USER_STORE` fails to parse, or
/// `MIDNIGHT_MANUAL_JWT_SECRET` is shorter than 32 bytes). When BOTH are
/// absent the server boots with `auth = None`.
pub fn build(pool: PgPool, cfg: ServerConfig) -> Result<Router, AuthStateError> {
    let auth =
        AuthState::from_env_values(cfg.user_store_body.as_deref(), cfg.jwt_secret.as_deref())?
            .map(Arc::new);
    let state = AppState { pool, cfg: Arc::new(cfg), auth };

    Ok(Router::new()
        .merge(crate::routes::health::router())
        .merge(crate::routes::sources::router())
        .merge(crate::routes::models::router())
        .merge(crate::routes::search::router())
        .merge(crate::routes::chunks::router())
        .merge(crate::routes::auth::router())
        // Bound the body size at the boundary — refuses oversize payloads
        // before any handler-side validation runs.
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::bearer::layer,
        ))
        .layer(axum::middleware::from_fn(request_id::layer))
        .layer(TraceLayer::new_for_http())
        .with_state(state))
}

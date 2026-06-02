//! axum app builder — wires routes, middleware, and shared state.

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::Router;
use mn_auth::{ChallengeStore, OAuthStateStore, SigningSecret, UserStore};
use sqlx::PgPool;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

use crate::config::ServerConfig;
use crate::middleware::request_id;

/// Cap for inbound request bodies. The largest legitimate body today is the
/// admin ingest `PUT .../documents` upload, which carries documents + chunk
/// text for one batch — comfortably bounded by the CLI's batching logic but
/// can reach several MiB on a one-shot upload of a docs site. 16 MiB gives
/// the CLI headroom while staying well below a memory-exhaustion threshold.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

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
    /// In-process rate limiter, or `None` when rate limiting is disabled
    /// (Phase 17). When `None` the rate-limit middleware is a pass-through.
    pub rate_limiter: Option<Arc<crate::ratelimit::RateLimiter>>,
    /// Confidence-scoring policy resolved at boot (US6, D24). Shared read-only
    /// across requests.
    pub scoring_policy: Arc<mn_core::scoring_policy::ScoringPolicy>,
    /// The corpus's active embedding model, re-resolvable without a restart.
    /// `None` until resolved — production resolves at boot; some tests leave it
    /// unresolved and search's existing `None`-handling covers that.
    pub corpus_model: crate::corpus_model::Shared,
    /// Server-side Voyage embedder for `POST /v1/embeddings`. `None` when
    /// `VOYAGE_API_KEY` is unset — the endpoint then 503s rather than failing
    /// boot, so a deployment that only proxies client-side vectors still serves.
    pub voyage: Option<std::sync::Arc<mn_embedding::voyage::VoyageEmbedder>>,
    /// In-process embedding-token accounting (tiered hourly/daily ceilings).
    /// Always present — token accounting has no disable switch.
    pub token_limiter: std::sync::Arc<crate::tokenlimit::TokenUsageLimiter>,
    /// On-disk model-cache directory, used by the embeddings handler's
    /// best-effort token pre-count (to locate a tokenizer when one is present).
    pub cache_dir: std::path::PathBuf,
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
    /// GitHub OAuth subsystem (FR-062, FR-115, FR-117). `None` when the
    /// GitHub-OAuth env vars are not all present — `/v1/auth/github/*`
    /// returns 503 in that case.
    pub github_oauth: Option<GithubOAuthState>,
}

/// Configured GitHub OAuth subsystem. Held inside [`AuthState`] so it
/// inherits the JWT-secret + user-store boot gate.
#[derive(Debug)]
pub struct GithubOAuthState {
    /// GitHub OAuth App client id.
    pub client_id: String,
    /// GitHub OAuth App client secret.
    pub client_secret: String,
    /// Public callback URL registered with the GitHub OAuth App.
    pub redirect_url: String,
    /// Required GitHub org. Only `active` members of this org receive a
    /// read-uplift bearer (FR-062).
    pub org: String,
    /// Read-uplift JWT TTL.
    pub read_token_ttl: time::Duration,
    /// Authorize URL base (production: `https://github.com/login/oauth/authorize`).
    pub authorize_url: String,
    /// Token-exchange URL (production: `https://github.com/login/oauth/access_token`).
    pub token_url: String,
    /// GitHub REST API base URL (production: `https://api.github.com`).
    pub api_base_url: String,
    /// CSRF / cli-port state store.
    pub states: OAuthStateStore,
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
    /// Build the auth subsystem from a `ServerConfig`. Returns `None` when
    /// the user-store body or JWT secret env is absent — auth endpoints then
    /// 503 cleanly. The GitHub OAuth subsystem is layered on top: it's only
    /// populated when all four GitHub env vars (client id, secret, redirect
    /// URL, org) are present alongside the JWT secret + user store.
    ///
    /// # Errors
    ///
    /// Returns [`AuthStateError`] when one input is present but malformed.
    /// "Missing" is allowed (read-only deployments); "garbage" is not.
    pub fn from_config(cfg: &ServerConfig) -> Result<Option<Self>, AuthStateError> {
        let (Some(body), Some(bytes)) = (cfg.user_store_body.as_deref(), cfg.jwt_secret.as_deref())
        else {
            return Ok(None);
        };
        let user_store = UserStore::parse(body)?;
        let jwt_secret = SigningSecret::from_bytes(bytes.to_vec())?;
        let github_oauth = build_github_oauth(cfg);
        Ok(Some(Self {
            user_store,
            jwt_secret,
            challenges: ChallengeStore::new(),
            github_oauth,
        }))
    }
}

fn build_github_oauth(cfg: &ServerConfig) -> Option<GithubOAuthState> {
    let client_id = cfg.github_oauth_client_id.as_ref()?.clone();
    let client_secret = cfg.github_oauth_client_secret.as_ref()?.clone();
    let redirect_url = cfg.github_oauth_redirect_url.as_ref()?.clone();
    let org = cfg.github_org.as_ref()?.clone();
    let read_token_ttl = time::Duration::days(cfg.read_token_ttl_days);
    Some(GithubOAuthState {
        client_id,
        client_secret,
        redirect_url,
        org,
        read_token_ttl,
        authorize_url: cfg.github_authorize_url.clone(),
        token_url: cfg.github_token_url.clone(),
        api_base_url: cfg.github_api_base_url.clone(),
        states: OAuthStateStore::new(),
    })
}

/// Build the full axum app: routes + middleware + state.
///
/// Constructs an unresolved (`None`) corpus model handle — callers that need a
/// resolved model (production boot) use [`build_with_limiter`] directly. Search
/// tolerates the unresolved state via its existing `None`-handling.
///
/// # Errors
///
/// Returns [`AuthStateError`] if the auth env values are present but malformed
/// (`MIDNIGHT_MANUAL_USER_STORE` fails to parse, or
/// `MIDNIGHT_MANUAL_JWT_SECRET` is shorter than 32 bytes). When BOTH are
/// absent the server boots with `auth = None`.
pub fn build(pool: PgPool, cfg: ServerConfig) -> Result<Router, AuthStateError> {
    let limiter = crate::ratelimit::RateLimiter::from_config(&cfg);
    let corpus_model = std::sync::Arc::new(std::sync::RwLock::new(None));
    let token_limiter = crate::tokenlimit::TokenUsageLimiter::from_config(&cfg);
    let voyage = voyage_from_config(&cfg);
    build_with_limiter(pool, cfg, limiter, corpus_model, token_limiter, voyage)
}

/// Construct the server-side Voyage embedder from config, or `None` when
/// `VOYAGE_API_KEY` is unset (BYOK / server-side embedding not configured).
fn voyage_from_config(
    cfg: &ServerConfig,
) -> Option<std::sync::Arc<mn_embedding::voyage::VoyageEmbedder>> {
    cfg.voyage_api_key.as_ref().map(|k| {
        std::sync::Arc::new(mn_embedding::voyage::VoyageEmbedder::new(
            k,
            &cfg.voyage_model,
            cfg.voyage_output_dimension,
            &cfg.voyage_output_dtype,
        ))
    })
}

/// Build the app with the corpus model auto-resolved from the DB. Convenience
/// for integration tests (and any caller that wants boot-time resolution
/// without threading the handle manually). Resolution failure yields an
/// unresolved (`None`) corpus model — search then 503s, matching prod's
/// "no model" path.
///
/// # Errors
/// Returns [`AuthStateError`] if the auth env values are present but malformed.
pub async fn build_resolved(pool: PgPool, cfg: ServerConfig) -> Result<Router, AuthStateError> {
    let cm = crate::corpus_model::resolve(&pool).await.ok();
    let corpus_model = std::sync::Arc::new(std::sync::RwLock::new(cm));
    let limiter = crate::ratelimit::RateLimiter::from_config(&cfg);
    let token_limiter = crate::tokenlimit::TokenUsageLimiter::from_config(&cfg);
    let voyage = voyage_from_config(&cfg);
    build_with_limiter(pool, cfg, limiter, corpus_model, token_limiter, voyage)
}

/// Build the app with an explicit rate limiter and corpus-model handle, so
/// `main` can share the limiter with its background tasks, pass a corpus model
/// resolved at boot, and integration tests can pre-seed overrides. [`build`]
/// delegates here after constructing the limiter from config and an unresolved
/// (`None`) corpus model.
///
/// # Errors
///
/// Returns [`AuthStateError`] if the auth env values are present but malformed.
pub fn build_with_limiter(
    pool: PgPool,
    cfg: ServerConfig,
    rate_limiter: Option<Arc<crate::ratelimit::RateLimiter>>,
    corpus_model: crate::corpus_model::Shared,
    token_limiter: Arc<crate::tokenlimit::TokenUsageLimiter>,
    voyage: Option<Arc<mn_embedding::voyage::VoyageEmbedder>>,
) -> Result<Router, AuthStateError> {
    let auth = AuthState::from_config(&cfg)?.map(Arc::new);
    let scoring_policy = Arc::new(cfg.scoring_policy.clone());
    // Resolve the model-cache dir the same way the embedder worker does; fall
    // back to a tempdir in a sandbox with no HOME/XDG so boot never fails on it.
    let cache_dir = mn_embedding::cache::resolve(&mn_embedding::cache::StdEnv)
        .unwrap_or_else(std::env::temp_dir);
    let state = AppState {
        pool,
        cfg: Arc::new(cfg),
        auth,
        rate_limiter,
        scoring_policy,
        corpus_model,
        voyage,
        token_limiter,
        cache_dir,
    };

    Ok(Router::new()
        .merge(crate::routes::health::router())
        .merge(crate::routes::sources::router())
        .merge(crate::routes::models::router())
        .merge(crate::routes::search::router())
        .merge(crate::routes::embeddings::router())
        .merge(crate::routes::chunks::router())
        .merge(crate::routes::documents::router())
        .merge(crate::routes::auth::router())
        .merge(crate::routes::admin_ingest::router())
        .merge(crate::routes::admin_ratelimits::router())
        .merge(crate::routes::admin_sources::router())
        .merge(crate::routes::admin_status::router())
        .merge(crate::routes::admin_versions::router())
        .merge(crate::routes::versions::router())
        .merge(crate::routes::github::router())
        .merge(crate::routes::telemetry::router())
        .merge(crate::routes::metrics::router())
        // Bound the body size at the boundary — refuses oversize payloads
        // before any handler-side validation runs.
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::rate_limit::layer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::bearer::layer,
        ))
        .layer(axum::middleware::from_fn(request_id::layer))
        .layer(TraceLayer::new_for_http())
        .with_state(state))
}

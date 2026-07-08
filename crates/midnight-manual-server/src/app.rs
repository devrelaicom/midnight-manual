//! axum app builder — wires routes, middleware, and shared state.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request};
use axum::Router;
use mnm_auth::{ChallengeStore, OAuthStateStore, SigningSecret, UserStore};
use sentry::integrations::tower::{NewSentryLayer, SentryHttpLayer};
use sqlx::PgPool;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

use crate::config::ServerConfig;
use crate::middleware::request_id;

/// Cap for inbound request bodies. The largest legitimate body today is the
/// admin ingest `PUT .../documents` upload, which carries documents + chunk
/// text + embedding vectors for one batch — comfortably bounded by the CLI's
/// size-aware batching logic but can reach many MiB on a dense repo. 25 MiB
/// gives the CLI headroom while staying well below a memory-exhaustion
/// threshold. Sourced from [`mnm_core::limits::MAX_INGEST_BODY_BYTES`] so the
/// server cap and the CLI's batch-size target stay in lockstep.
pub const MAX_BODY_BYTES: usize = mnm_core::limits::MAX_INGEST_BODY_BYTES;

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
    pub scoring_policy: Arc<mnm_core::scoring_policy::ScoringPolicy>,
    /// The corpus's active embedding model. Re-resolvable without a restart for
    /// the wire-id LABEL search stamps and the `/v1/models/active` response — but
    /// NOT for the proxy embedders below, which stay pinned to the boot model
    /// (see [`AppState::voyage`] and [`crate::corpus_model::refresh`]). `None`
    /// until resolved — production resolves at boot; some tests leave it
    /// unresolved and search's existing `None`-handling covers that.
    pub corpus_model: crate::corpus_model::Shared,
    /// Server-side Voyage CODE embedder for `POST /v1/embeddings`. `None` when
    /// `VOYAGE_API_KEY` is unset — the endpoint then 503s rather than failing
    /// boot, so a deployment that only proxies client-side vectors still serves.
    ///
    /// PINNED AT BOOT: built from the boot-resolved code model (see
    /// [`resolved_embedders`]) and never re-resolved. A runtime model swap
    /// (`code_model` is not refreshed at all; `corpus_model` is) leaves this
    /// computing with the boot model — a restart is required to re-align it with
    /// the wire id `/v1/embeddings` stamps. See [`crate::corpus_model::refresh`].
    pub voyage: Option<std::sync::Arc<mnm_embedding::voyage::VoyageEmbedder>>,
    /// Server-side contextualized (general) Voyage embedder for
    /// `POST /v1/embeddings` with `type=general`. `None` when `VOYAGE_API_KEY`
    /// is unset (endpoint 503s).
    ///
    /// PINNED AT BOOT, like [`AppState::voyage`]: built from the boot-resolved
    /// corpus model and never re-resolved, so a runtime promotion onto a
    /// different general model requires a restart for this to follow.
    pub voyage_ctx:
        Option<std::sync::Arc<mnm_embedding::contextualized::ContextualizedVoyageEmbedder>>,
    /// The corpus's code-embedding model, resolved at boot from config.
    /// `None` when unresolved — code_mode searches then 503.
    pub code_model: crate::code_model::Shared,
    /// In-process embedding-token accounting (tiered hourly/daily ceilings).
    /// Always present — token accounting has no disable switch.
    pub token_limiter: std::sync::Arc<crate::tokenlimit::TokenUsageLimiter>,
    /// On-disk model-cache directory, used by the embeddings handler's
    /// best-effort token pre-count (to locate a tokenizer when one is present).
    pub cache_dir: std::path::PathBuf,
    /// Shared TTL cache for the `/v1/facets` response (see `routes::facets`).
    /// Held per-app (not a module-global static) so each constructed app —
    /// including each integration test's app — gets an isolated cache.
    pub facets_cache: crate::routes::facets::FacetsCache,
    /// Ingest-time prompt-injection scanning state (issue #103). Holds the
    /// resolved policy and the optional hosted-model client. Pattern-only when
    /// the model leg is unconfigured; the whole scan is a no-op when
    /// `injection_enabled = false`.
    pub injection: std::sync::Arc<crate::injection::scan::InjectionState>,
    /// Active corpus model's per-category topic centroids, cached read-mostly.
    /// `None` until the boot load resolves (best-effort) and whenever a load
    /// fails; refreshed on corpus-version promotion. Feeds the bounded
    /// `search.topic` classification (wired in a later task).
    pub topic_centroids: crate::observability::topic::Shared,
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
    UserStore(#[from] mnm_auth::UserStoreError),
    /// `MIDNIGHT_MANUAL_JWT_SECRET` was shorter than the 32-byte floor.
    #[error("jwt secret invalid: {0}")]
    JwtSecret(#[from] mnm_auth::JwtError),
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
    let voyage_ctx = voyage_ctx_from_config(&cfg);
    let code_model = std::sync::Arc::new(std::sync::RwLock::new(None));
    let topic_centroids = std::sync::Arc::new(std::sync::RwLock::new(None));
    build_with_limiter(
        pool,
        cfg,
        limiter,
        corpus_model,
        token_limiter,
        voyage,
        voyage_ctx,
        code_model,
        topic_centroids,
    )
}

/// The two server-side Voyage embedders for `POST /v1/embeddings`: the flat
/// code client (`type=code`) and the contextualized general client
/// (`type=general`). Both are `None` when `VOYAGE_API_KEY` is unset.
type ServerEmbedders = (
    Option<std::sync::Arc<mnm_embedding::voyage::VoyageEmbedder>>,
    Option<std::sync::Arc<mnm_embedding::contextualized::ContextualizedVoyageEmbedder>>,
);

/// Resolve the model NAME the server-side embedder should compute with.
///
/// The authority is `resolved_name` — the bare name of the corpus/code model
/// resolved from the registry (the same model whose wire id the
/// `/v1/embeddings` route stamps on the response). When the operator set an
/// explicit `MIDNIGHT_MANUAL_VOYAGE_MODEL` / `_CONTEXT_MODEL` override
/// (`override_name = Some`), that wins — but a `warn!` is emitted if it
/// disagrees with the registry, because embedding with an overriding name while
/// labelling vectors with the registry wire id re-introduces the very drift this
/// fix prevents. `which` is `"general"` / `"code"` for the log line.
fn resolve_embed_model_name<'a>(
    which: &str,
    override_name: Option<&'a str>,
    resolved_name: &'a str,
) -> &'a str {
    let Some(name) = override_name else {
        return resolved_name;
    };
    if name != resolved_name {
        tracing::warn!(
            which,
            override_model = name,
            registry_model = resolved_name,
            "configured embedding-model override disagrees with the corpus's active model; the \
             proxy will embed with the override but stamp vectors with the registry wire id — \
             re-align them to avoid silently-mismatched vectors"
        );
    }
    name
}

/// Build the server-side embedders from the RESOLVED corpus/code model (the
/// fix for the cross-element drift bug). The general embedder uses the corpus
/// active model's name/dim; the code embedder uses the resolved code model's
/// name/dim, falling back to the config override / model wire name when no code
/// model resolved (the code embedder is unreachable in that case — code
/// searches 503 before embedding — so the fallback only keeps boot simple).
///
/// Dtype comes from the SAME `CORPUS_DTYPE` constant (in `routes::models`)
/// that `/v1/models/active` reports, so the dtype the proxy COMPUTES with cannot
/// diverge from the dtype the server LABELS the corpus with. The config's
/// `voyage_output_dtype` is only consulted to warn when an operator set it to
/// something other than the corpus dtype (a misconfiguration). Returns
/// `(code_embedder, general_embedder)`.
///
/// IMPORTANT: the returned embedders are PINNED at the corpus/code model passed
/// here. They are built once at boot and never re-resolved, so a runtime model
/// swap (an ingest finalize that promotes a different model — see
/// [`crate::corpus_model::refresh`]) requires a server restart for the proxy to
/// compute with the new model. The refresh path fails loud (warns) when the new
/// model disagrees with what these embedders were built with.
#[must_use]
pub fn resolved_embedders(
    cfg: &ServerConfig,
    corpus: &crate::corpus_model::CorpusModel,
    code: Option<&crate::code_model::CodeModel>,
) -> ServerEmbedders {
    let dtype = proxy_dtype(cfg);
    let general_name =
        resolve_embed_model_name("general", cfg.voyage_context_model.as_deref(), &corpus.name);
    let general_dim = u32::try_from(corpus.dim).unwrap_or(cfg.voyage_output_dimension);
    let voyage_ctx = cfg.voyage_api_key.as_ref().map(|k| {
        // `apply_voyage_base_url` honours a configured Voyage base URL (proxy/
        // mirror, or a wiremock in tests) on the /v1/embeddings proxy path —
        // otherwise it stays pinned to the hardcoded api.voyageai.com while the
        // reranker uses the override (issue #170: split-brain config).
        let e = mnm_embedding::contextualized::ContextualizedVoyageEmbedder::new(
            k,
            general_name,
            general_dim,
            dtype,
        );
        std::sync::Arc::new(apply_voyage_base_url(e, cfg))
    });

    // Code: resolved code-model name (authority) when present; otherwise the
    // explicit override, else the configured `code_model_wire`'s bare name.
    let code_fallback = cfg
        .voyage_model
        .as_deref()
        .unwrap_or_else(|| code_wire_name(&cfg.code_model_wire));
    let code_name = code.map_or(code_fallback, |c| {
        resolve_embed_model_name("code", cfg.voyage_model.as_deref(), &c.name)
    });
    let code_dim = code
        .and_then(|c| u32::try_from(c.dim).ok())
        .unwrap_or(cfg.voyage_output_dimension);
    let voyage = cfg.voyage_api_key.as_ref().map(|k| {
        // Same base-URL override as the general embedder above (issue #170).
        let e = mnm_embedding::voyage::VoyageEmbedder::new(k, code_name, code_dim, dtype);
        std::sync::Arc::new(apply_voyage_base_url(e, cfg))
    });

    (voyage, voyage_ctx)
}

/// The dtype the server-side proxy embedders compute with: the SAME
/// [`crate::routes::models::CORPUS_DTYPE`] the `/v1/models/active` route reports,
/// so the server's two halves (the bytes it produces vs. the dtype it advertises)
/// cannot drift. The config's `voyage_output_dtype` only earns a `warn!` when an
/// operator set it to something other than the corpus dtype — the corpus is
/// encoded at `CORPUS_DTYPE`, so honouring a divergent config dtype would
/// produce vectors that mismatch the corpus.
fn proxy_dtype(cfg: &ServerConfig) -> &'static str {
    if cfg.voyage_output_dtype != crate::routes::models::CORPUS_DTYPE {
        tracing::warn!(
            config_dtype = %cfg.voyage_output_dtype,
            corpus_dtype = crate::routes::models::CORPUS_DTYPE,
            "MIDNIGHT_MANUAL_VOYAGE_DTYPE differs from the corpus dtype; the proxy embeds at the \
             corpus dtype regardless (the corpus is encoded at that dtype) — remove the override \
             to silence this"
        );
    }
    crate::routes::models::CORPUS_DTYPE
}

/// Bare model name from a `name@revision` wire id, or the whole string when it
/// has no `@` (defensive — the configured default is always well-formed).
fn code_wire_name(wire: &str) -> &str {
    wire.split_once('@').map_or(wire, |(name, _)| name)
}

/// Apply the operator's configured Voyage base-URL override (a proxy/mirror in
/// production, or a wiremock in tests) to a freshly-constructed server-side
/// embedder, or return it unchanged when no override is set (the embedder keeps
/// its hardcoded `api.voyageai.com`).
///
/// All four server-side embedder construction sites — the two in
/// [`resolved_embedders`] and the two config-only fallbacks
/// ([`voyage_from_config`] / [`voyage_ctx_from_config`], reached by [`build`] and
/// the unresolved-corpus branch of [`build_resolved`]) — route through this one
/// helper so a future constructor cannot silently reintroduce the split-brain gap
/// where the reranker honours the override but the `/v1/embeddings` proxy still
/// embeds against the hardcoded host (issue #170).
fn apply_voyage_base_url<E: VoyageBaseUrl>(embedder: E, cfg: &ServerConfig) -> E {
    match cfg.voyage_base_url.as_deref() {
        Some(base) => embedder.set_base_url(base),
        None => embedder,
    }
}

/// The server-side Voyage embedders that expose a base-URL override, letting
/// [`apply_voyage_base_url`] treat the flat code client and the contextualized
/// general client uniformly through one bound.
trait VoyageBaseUrl: Sized {
    /// Consume `self`, returning it with `base` as its request host.
    #[must_use]
    fn set_base_url(self, base: &str) -> Self;
}

impl VoyageBaseUrl for mnm_embedding::voyage::VoyageEmbedder {
    fn set_base_url(self, base: &str) -> Self {
        self.with_base_url(base)
    }
}

impl VoyageBaseUrl for mnm_embedding::contextualized::ContextualizedVoyageEmbedder {
    fn set_base_url(self, base: &str) -> Self {
        self.with_base_url(base)
    }
}

/// Construct the server-side embedders from config alone, used by [`build`]
/// (the unresolved/offline path — no DB round-trip). The model name falls back
/// to the explicit override, else the configured `code_model_wire` / a
/// `voyage-context-3` literal, because no resolved model is available here. This
/// path is offline/test-only; the production boot path uses
/// [`resolved_embedders`], which derives the name from the registry.
fn voyage_from_config(
    cfg: &ServerConfig,
) -> Option<std::sync::Arc<mnm_embedding::voyage::VoyageEmbedder>> {
    cfg.voyage_api_key.as_ref().map(|k| {
        let name = cfg
            .voyage_model
            .as_deref()
            .unwrap_or_else(|| code_wire_name(&cfg.code_model_wire));
        let e = mnm_embedding::voyage::VoyageEmbedder::new(
            k,
            name,
            cfg.voyage_output_dimension,
            proxy_dtype(cfg),
        );
        // Same base-URL override as the resolved path (issue #170): this offline
        // fallback still serves /v1/embeddings on a fresh (unresolved) server.
        std::sync::Arc::new(apply_voyage_base_url(e, cfg))
    })
}

/// Construct the server-side contextualized (general) embedder from config
/// alone (the unresolved/offline path used by [`build`]). Falls back to the
/// explicit override, else a `voyage-context-3` literal.
fn voyage_ctx_from_config(
    cfg: &ServerConfig,
) -> Option<std::sync::Arc<mnm_embedding::contextualized::ContextualizedVoyageEmbedder>> {
    cfg.voyage_api_key.as_ref().map(|k| {
        let name = cfg
            .voyage_context_model
            .as_deref()
            .unwrap_or("voyage-context-3");
        let e = mnm_embedding::contextualized::ContextualizedVoyageEmbedder::new(
            k,
            name,
            cfg.voyage_output_dimension,
            proxy_dtype(cfg),
        );
        // Same base-URL override as the resolved path (issue #170).
        std::sync::Arc::new(apply_voyage_base_url(e, cfg))
    })
}

/// Build the app with the corpus model (and the config-pinned code model)
/// auto-resolved from the DB. Convenience for integration tests (and any
/// caller that wants boot-time resolution without threading the handles
/// manually). Resolution failure yields an unresolved (`None`) model —
/// search then 503s, matching prod's "no model" path.
///
/// # Errors
/// Returns [`AuthStateError`] if the auth env values are present but malformed.
pub async fn build_resolved(pool: PgPool, cfg: ServerConfig) -> Result<Router, AuthStateError> {
    let cm = crate::corpus_model::resolve(&pool).await.ok();
    let km = crate::code_model::resolve(&pool, &cfg.code_model_wire)
        .await
        .ok();
    let topic_centroids = {
        let loaded = match cm.as_ref() {
            Some(c) => crate::observability::topic::load_centroids(&pool, c.id)
                .await
                .ok(),
            None => None,
        };
        std::sync::Arc::new(std::sync::RwLock::new(loaded))
    };
    let limiter = crate::ratelimit::RateLimiter::from_config(&cfg);
    let token_limiter = crate::tokenlimit::TokenUsageLimiter::from_config(&cfg);
    // Derive embedders from the resolved models when available (the production
    // path), so the proxy's embed model matches the stamped wire id AT BOOT (it
    // stays pinned to this model thereafter — see `resolved_embedders`); fall
    // back to config-only construction when the corpus model is unresolved.
    let (voyage, voyage_ctx) = cm.as_ref().map_or_else(
        || (voyage_from_config(&cfg), voyage_ctx_from_config(&cfg)),
        |corpus| resolved_embedders(&cfg, corpus, km.as_ref()),
    );
    let corpus_model = std::sync::Arc::new(std::sync::RwLock::new(cm));
    let code_model = std::sync::Arc::new(std::sync::RwLock::new(km));
    build_with_limiter(
        pool,
        cfg,
        limiter,
        corpus_model,
        token_limiter,
        voyage,
        voyage_ctx,
        code_model,
        topic_centroids,
    )
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
// Boot wiring threads each shared handle explicitly (so `main` and tests can
// share/seed them); a params struct would only add ceremony.
#[allow(clippy::too_many_arguments)]
pub fn build_with_limiter(
    pool: PgPool,
    cfg: ServerConfig,
    rate_limiter: Option<Arc<crate::ratelimit::RateLimiter>>,
    corpus_model: crate::corpus_model::Shared,
    token_limiter: Arc<crate::tokenlimit::TokenUsageLimiter>,
    voyage: Option<Arc<mnm_embedding::voyage::VoyageEmbedder>>,
    voyage_ctx: Option<Arc<mnm_embedding::contextualized::ContextualizedVoyageEmbedder>>,
    code_model: crate::code_model::Shared,
    topic_centroids: crate::observability::topic::Shared,
) -> Result<Router, AuthStateError> {
    let auth = AuthState::from_config(&cfg)?.map(Arc::new);
    let scoring_policy = Arc::new(cfg.scoring_policy.clone());
    // Resolve the model-cache dir (used for the optional token pre-count
    // tokenizer); fall back to a tempdir in a sandbox with no HOME/XDG so boot
    // never fails on it.
    let cache_dir = mnm_embedding::cache::resolve(&mnm_embedding::cache::StdEnv)
        .unwrap_or_else(std::env::temp_dir);
    // Build from `&cfg` BEFORE the struct literal moves `cfg` into `Arc::new`.
    let injection = std::sync::Arc::new(crate::injection::scan::InjectionState::from_config(&cfg));
    let state = AppState {
        pool,
        cfg: Arc::new(cfg),
        auth,
        rate_limiter,
        scoring_policy,
        corpus_model,
        voyage,
        voyage_ctx,
        code_model,
        token_limiter,
        cache_dir,
        facets_cache: crate::routes::facets::new_cache(),
        injection,
        topic_centroids,
    };

    Ok(Router::new()
        .merge(crate::routes::health::router())
        .merge(crate::routes::me::router())
        .merge(crate::routes::sources::router())
        .merge(crate::routes::models::router())
        .merge(crate::routes::search::router())
        .merge(crate::routes::facets::router())
        .merge(crate::routes::embeddings::router())
        .merge(crate::routes::chunks::router())
        .merge(crate::routes::documents::router())
        .merge(crate::routes::auth::router())
        .merge(crate::routes::admin_ingest::router())
        .merge(crate::routes::admin_injection::router())
        .merge(crate::routes::admin_ratelimits::router())
        .merge(crate::routes::admin_sources::router())
        .merge(crate::routes::admin_status::router())
        .merge(crate::routes::admin_tokenlimits::router())
        .merge(crate::routes::admin_versions::router())
        .merge(crate::routes::versions::router())
        .merge(crate::routes::github::router())
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
        // Sentry: continue/start a transaction per request (outermost), each on
        // its own Hub, so request<->error correlation and distributed tracing
        // (incoming `sentry-trace`/`baggage`) work correctly. Bound directly on
        // the Router (not via `ServiceBuilder`), so these bind in the OPPOSITE
        // order to a `ServiceBuilder`: `SentryHttpLayer` first, then
        // `NewSentryLayer`, which ends up outermost. Inert when Sentry's master
        // gate is closed (default) — a Hub with no client bound is a no-op.
        .layer(SentryHttpLayer::new().enable_transaction())
        .layer(NewSentryLayer::<Request<Body>>::new_from_top())
        .with_state(state))
}

#[cfg(test)]
mod tests {
    use super::{
        code_wire_name, proxy_dtype, resolve_embed_model_name, resolved_embedders,
        voyage_ctx_from_config, voyage_from_config,
    };
    use crate::code_model::CodeModel;
    use crate::config::ServerConfig;
    use crate::corpus_model::CorpusModel;
    use crate::routes::models::CORPUS_DTYPE;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The proxy embedder dtype is sourced from the SAME `CORPUS_DTYPE` constant
    /// the `/v1/models/active` route reports, so the bytes the proxy produces and
    /// the dtype the server advertises cannot drift. The default config's
    /// `voyage_output_dtype` agrees, so no warn is warranted.
    #[test]
    fn proxy_dtype_sources_from_corpus_constant() {
        let cfg = ServerConfig::default();
        assert_eq!(proxy_dtype(&cfg), CORPUS_DTYPE);
    }

    /// Even when the operator sets a divergent `voyage_output_dtype`, the proxy
    /// still computes at the corpus dtype (the corpus is encoded at it) — the
    /// config value only earns a warn, it never changes what the proxy produces.
    #[test]
    fn proxy_dtype_ignores_divergent_config_dtype() {
        let cfg = ServerConfig {
            voyage_output_dtype: "int8".to_owned(),
            ..ServerConfig::default()
        };
        assert_eq!(proxy_dtype(&cfg), CORPUS_DTYPE);
    }

    /// No override → the resolved (registry) name is returned verbatim. This is
    /// the production default: the proxy computes with the corpus's active model.
    #[test]
    fn resolve_embed_model_name_no_override_returns_resolved() {
        let name = resolve_embed_model_name("general", None, "voyage-context-3");
        assert_eq!(name, "voyage-context-3");
    }

    /// Override that AGREES with the registry → the override is returned and no
    /// drift warning is warranted (the two names are identical).
    #[test]
    fn resolve_embed_model_name_agreeing_override_returns_override() {
        let name = resolve_embed_model_name("code", Some("voyage-code-3"), "voyage-code-3");
        assert_eq!(name, "voyage-code-3");
    }

    /// Override that DISAGREES with the registry → the override still wins (the
    /// operator asked for it) and a drift `warn!` fires. We can't easily assert
    /// on the log line here, but the return value is the override, which is the
    /// behaviour the embeddings route binds the embedder to.
    #[test]
    fn resolve_embed_model_name_disagreeing_override_returns_override() {
        let name = resolve_embed_model_name("general", Some("voyage-3"), "voyage-context-3");
        assert_eq!(name, "voyage-3");
        assert_ne!(name, "voyage-context-3");
    }

    /// A well-formed `name@revision` wire id yields the bare name.
    #[test]
    fn code_wire_name_strips_revision() {
        assert_eq!(code_wire_name("voyage-code-3@1"), "voyage-code-3");
    }

    /// A wire id with no `@` is returned whole (defensive fallback).
    #[test]
    fn code_wire_name_without_at_returns_whole() {
        assert_eq!(code_wire_name("voyage-code-3"), "voyage-code-3");
    }

    /// Regression for #170: the `/v1/embeddings` proxy embedders must honour a
    /// configured `voyage_base_url`, not stay pinned to `api.voyageai.com` while
    /// only the reranker respects the override. Both proxy embedders are pointed
    /// at a wiremock; each mock `.expect(1)` fails on drop if the request never
    /// arrives (i.e. if the base URL wasn't threaded, the calls would hit the
    /// hardcoded default host instead).
    #[tokio::test]
    async fn resolved_embedders_honor_configured_base_url() {
        let server = MockServer::start().await;
        // General (contextualized) embedder path.
        Mock::given(method("POST"))
            .and(path("/v1/contextualizedembeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [{ "object": "list", "index": 0, "data": [
                    { "object": "embedding", "index": 0, "embedding": [1.0, 1.0] }
                ]}],
                "model": "voyage-context-3",
                "usage": { "total_tokens": 1 }
            })))
            .expect(1)
            .mount(&server)
            .await;
        // Code (flat) embedder path.
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "embedding": [2.0, 2.0], "index": 0 }],
                "model": "voyage-code-3",
                "usage": { "total_tokens": 1 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = ServerConfig {
            voyage_api_key: Some("test-key".to_owned()),
            voyage_base_url: Some(server.uri()),
            ..ServerConfig::default()
        };
        let corpus = CorpusModel {
            wire: "voyage-context-3@1".to_owned(),
            name: "voyage-context-3".to_owned(),
            id: uuid::Uuid::nil(),
            dim: 2,
        };
        let code = CodeModel {
            wire: "voyage-code-3@1".to_owned(),
            name: "voyage-code-3".to_owned(),
            id: uuid::Uuid::nil(),
            dim: 2,
        };

        let (voyage, voyage_ctx) = resolved_embedders(&cfg, &corpus, Some(&code));
        let voyage = voyage.expect("code embedder built when api key is set");
        let voyage_ctx = voyage_ctx.expect("general embedder built when api key is set");

        voyage_ctx
            .embed_queries(vec!["q".to_owned()])
            .await
            .expect("general embedder hits the configured base url");
        voyage
            .embed(vec!["q".to_owned()], mnm_embedding::voyage::InputType::Document)
            .await
            .expect("code embedder hits the configured base url");
        // Mock `.expect(1)` assertions are verified when `server` drops here.
    }

    /// Regression for #170: the config-only embedder fallbacks
    /// (`voyage_from_config` / `voyage_ctx_from_config`) must ALSO honour a
    /// configured `voyage_base_url`. These are the constructors `build()` and the
    /// unresolved-corpus branch of `build_resolved` use — so a fresh server
    /// (corpus not yet resolved) with a base-URL override still has to route
    /// `/v1/embeddings` at the mirror, not the hardcoded api.voyageai.com. Each
    /// mock `.expect(1)` fails on drop if the base URL wasn't threaded.
    #[tokio::test]
    async fn config_embedders_honor_configured_base_url() {
        let server = MockServer::start().await;
        // General (contextualized) embedder path.
        Mock::given(method("POST"))
            .and(path("/v1/contextualizedembeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [{ "object": "list", "index": 0, "data": [
                    { "object": "embedding", "index": 0, "embedding": [1.0, 1.0] }
                ]}],
                "model": "voyage-context-3",
                "usage": { "total_tokens": 1 }
            })))
            .expect(1)
            .mount(&server)
            .await;
        // Code (flat) embedder path.
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "embedding": [2.0, 2.0], "index": 0 }],
                "model": "voyage-code-3",
                "usage": { "total_tokens": 1 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = ServerConfig {
            voyage_api_key: Some("test-key".to_owned()),
            voyage_base_url: Some(server.uri()),
            ..ServerConfig::default()
        };

        let voyage = voyage_from_config(&cfg).expect("code embedder built when api key is set");
        let voyage_ctx =
            voyage_ctx_from_config(&cfg).expect("general embedder built when api key is set");

        voyage_ctx
            .embed_queries(vec!["q".to_owned()])
            .await
            .expect("general embedder hits the configured base url");
        voyage
            .embed(vec!["q".to_owned()], mnm_embedding::voyage::InputType::Document)
            .await
            .expect("code embedder hits the configured base url");
        // Mock `.expect(1)` assertions are verified when `server` drops here.
    }
}

//! axum app builder — wires routes, middleware, and shared state.

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::Router;
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
}

/// Build the full axum app: routes + middleware + state.
#[must_use]
pub fn build(pool: PgPool, cfg: ServerConfig) -> Router {
    let state = AppState { pool, cfg: Arc::new(cfg) };

    Router::new()
        .merge(crate::routes::health::router())
        .merge(crate::routes::sources::router())
        .merge(crate::routes::models::router())
        .merge(crate::routes::search::router())
        .merge(crate::routes::chunks::router())
        // Bound the body size at the boundary — refuses oversize payloads
        // before any handler-side validation runs.
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(axum::middleware::from_fn(request_id::layer))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

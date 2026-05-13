//! axum app builder — wires routes, middleware, and shared state.

use std::sync::Arc;

use axum::Router;
use sqlx::PgPool;
use tower_http::trace::TraceLayer;

use crate::config::ServerConfig;
use crate::middleware::request_id;

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
        .layer(axum::middleware::from_fn(request_id::layer))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

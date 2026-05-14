//! `GET /v1/models/active` — return the corpus's active embedding model
//! identifier (US4 acceptance #12, FR-039). Clients use this to detect they
//! need to pull a different model before issuing queries.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::entities::embedding_model;
use serde::Serialize;

use crate::app::AppState;
use crate::error;

/// Mount the models routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/models/active", get(active_model))
}

/// Response shape for `/v1/models/active`.
#[derive(Debug, Serialize)]
struct ActiveModelResponse {
    name: String,
    revision: i32,
    dim: i32,
    provider: String,
}

async fn active_model(State(state): State<AppState>) -> Response {
    // Resolve via the canonical active-row lookup so this endpoint stays in
    // sync with the model id the search handler enforces. When a future
    // migration introduces multi-model corpora, `get_active` is where the
    // per-source-version selection lands.
    match embedding_model::get_active(&state.pool).await {
        Ok(m) => Json(ActiveModelResponse {
            name: m.name,
            revision: m.revision,
            dim: m.dim,
            provider: m.provider,
        })
        .into_response(),
        Err(e) => error::service_unavailable(format!("active model lookup failed: {e}"), ""),
    }
}

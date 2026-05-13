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
    // For v1 there is exactly one active model (the bge-base-en-v1.5@1 row
    // seeded by migration 0006). When a future migration introduces multi-
    // model corpora, the server will need to honor per-source-version model
    // selection. For now we pick the first row by created_at.
    match embedding_model::get_by_name_revision(&state.pool, "bge-base-en-v1.5", 1).await {
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

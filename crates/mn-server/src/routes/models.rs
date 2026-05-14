//! `GET /v1/models/active` — return the corpus's active embedding model
//! identifier (US4 acceptance #12, FR-039). Clients use this to detect they
//! need to pull a different model before issuing queries.

use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::entities::embedding_model;
use serde::Serialize;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

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

async fn active_model(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match embedding_model::get_active(&state.pool).await {
        Ok(m) => Json(ActiveModelResponse {
            name: m.name,
            revision: m.revision,
            dim: m.dim,
            provider: m.provider,
        })
        .into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "active model lookup failed");
            error::service_unavailable("active model lookup failed", rid)
        }
    }
}

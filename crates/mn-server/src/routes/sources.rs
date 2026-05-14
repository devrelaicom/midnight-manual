//! `/v1/sources` — list and show. (Write endpoints land in Phase 7.)

use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::{entities::source, StoreError};

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Mount the sources read routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sources", get(list_sources))
        .route("/v1/sources/:slug", get(get_source))
}

async fn list_sources(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match source::list_active(&state.pool).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "list_sources", error = %e, "store error");
            error::service_unavailable("list_sources failed", rid)
        }
    }
}

async fn get_source(
    Path(slug): Path<String>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match source::get_by_slug(&state.pool, &slug).await {
        Ok(row) => Json(row).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("source `{slug}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "get_source", error = %e, "store error");
            error::service_unavailable("source lookup failed", rid)
        }
    }
}

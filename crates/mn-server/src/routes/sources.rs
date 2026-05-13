//! `/v1/sources` — list and show. (Write endpoints land in Phase 7.)

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::{entities::source, StoreError};

use crate::app::AppState;
use crate::error;

/// Mount the sources read routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sources", get(list_sources))
        .route("/v1/sources/:slug", get(get_source))
}

async fn list_sources(State(state): State<AppState>) -> Response {
    match source::list_active(&state.pool).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => map_store_err(e, "list sources"),
    }
}

async fn get_source(Path(slug): Path<String>, State(state): State<AppState>) -> Response {
    match source::get_by_slug(&state.pool, &slug).await {
        Ok(row) => Json(row).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("source `{slug}` not found"), ""),
        Err(e) => map_store_err(e, "get source"),
    }
}

fn map_store_err(err: StoreError, op: &str) -> Response {
    tracing::warn!(op = %op, error = %err, "store error");
    error::service_unavailable(format!("{op} failed: {err}"), "")
}

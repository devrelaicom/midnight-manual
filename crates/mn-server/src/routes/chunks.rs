//! `GET /v1/chunks/:id` + `/siblings` + `/parents` (US4 acceptance #3-5).

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::{entities::chunk, entities::node, StoreError};
use uuid::Uuid;

use crate::app::AppState;
use crate::error;

/// Mount the chunk read routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/chunks/:id", get(get_chunk))
        .route("/v1/chunks/:id/siblings", get(get_siblings))
        .route("/v1/chunks/:id/parents", get(get_parents))
}

async fn get_chunk(Path(id): Path<Uuid>, State(state): State<AppState>) -> Response {
    match chunk::get_by_id_ready(&state.pool, id).await {
        Ok(c) => Json(c).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("chunk `{id}` not found"), ""),
        Err(e) => error::service_unavailable(format!("get_chunk: {e}"), ""),
    }
}

async fn get_siblings(Path(id): Path<Uuid>, State(state): State<AppState>) -> Response {
    // Fetch the chunk first to discover its document_id.
    let parent_chunk = match chunk::get_by_id_ready(&state.pool, id).await {
        Ok(c) => c,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("chunk `{id}` not found"), "")
        }
        Err(e) => return error::service_unavailable(format!("get_chunk: {e}"), ""),
    };
    match chunk::list_siblings(&state.pool, parent_chunk.document_id).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => error::service_unavailable(format!("list_siblings: {e}"), ""),
    }
}

async fn get_parents(Path(id): Path<Uuid>, State(state): State<AppState>) -> Response {
    // Resolve chunk -> node_id, then walk parent_chain.
    let parent_chunk = match chunk::get_by_id_ready(&state.pool, id).await {
        Ok(c) => c,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("chunk `{id}` not found"), "")
        }
        Err(e) => return error::service_unavailable(format!("get_chunk: {e}"), ""),
    };
    match node::parent_chain(&state.pool, parent_chunk.node_id).await {
        Ok(chain) => Json(chain).into_response(),
        Err(e) => error::service_unavailable(format!("parent_chain: {e}"), ""),
    }
}

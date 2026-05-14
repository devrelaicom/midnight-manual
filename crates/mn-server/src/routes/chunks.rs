//! `GET /v1/chunks/:id` + `/siblings` + `/parents` (US4 acceptance #3-5).

use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::{entities::chunk, entities::node, StoreError};
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Mount the chunk read routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/chunks/:id", get(get_chunk))
        .route("/v1/chunks/:id/siblings", get(get_siblings))
        .route("/v1/chunks/:id/parents", get(get_parents))
}

async fn get_chunk(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match chunk::get_by_id_ready(&state.pool, id).await {
        Ok(c) => Json(c).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("chunk `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_chunk failed");
            error::service_unavailable("chunk lookup failed", rid)
        }
    }
}

async fn get_siblings(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let parent_chunk = match chunk::get_by_id_ready(&state.pool, id).await {
        Ok(c) => c,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("chunk `{id}` not found"), rid)
        }
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_chunk (for siblings) failed");
            return error::service_unavailable("chunk lookup failed", rid);
        }
    };
    match chunk::list_siblings(&state.pool, parent_chunk.document_id).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "list_siblings failed");
            error::service_unavailable("sibling lookup failed", rid)
        }
    }
}

async fn get_parents(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let parent_chunk = match chunk::get_by_id_ready(&state.pool, id).await {
        Ok(c) => c,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("chunk `{id}` not found"), rid)
        }
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_chunk (for parents) failed");
            return error::service_unavailable("chunk lookup failed", rid);
        }
    };
    match node::parent_chain(&state.pool, parent_chunk.node_id).await {
        Ok(chain) => Json(chain).into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "parent_chain failed");
            error::service_unavailable("parent-chain lookup failed", rid)
        }
    }
}

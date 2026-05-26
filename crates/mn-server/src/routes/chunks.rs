//! `GET /v1/chunks/:id` + `/next` + `/prev` + `/parents`.
//!
//! Each endpoint returns a chunk row with its document and source context
//! bundled. The `/next` and `/prev` endpoints walk in `chunk_index` order;
//! `embed_failed` chunks are skipped. `/siblings` (unbounded) was removed
//! in favor of position-windowed `/v1/documents/:id/chunks`.

use axum::extract::{Extension, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::{entities::chunk, entities::node, StoreError};
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Mount the chunk read routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/chunks/:id", get(get_chunk))
        .route("/v1/chunks/:id/next", get(get_next))
        .route("/v1/chunks/:id/prev", get(get_prev))
        .route("/v1/chunks/:id/parents", get(get_parents))
}

#[derive(Debug, Deserialize)]
struct CountQuery {
    #[serde(default = "default_count")]
    count: usize,
}

const fn default_count() -> usize {
    5
}

async fn get_chunk(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match chunk::get_with_context(&state.pool, id).await {
        Ok(c) => Json(c).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("chunk `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_chunk failed");
            error::service_unavailable("chunk lookup failed", rid)
        }
    }
}

async fn get_next(
    Path(id): Path<Uuid>,
    Query(q): Query<CountQuery>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match chunk::list_next(&state.pool, id, q.count).await {
        Ok(chunks) => Json(serde_json::json!({ "chunks": chunks })).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("chunk `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_next failed");
            error::service_unavailable("next-chunks lookup failed", rid)
        }
    }
}

async fn get_prev(
    Path(id): Path<Uuid>,
    Query(q): Query<CountQuery>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match chunk::list_prev(&state.pool, id, q.count).await {
        Ok(chunks) => Json(serde_json::json!({ "chunks": chunks })).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("chunk `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_prev failed");
            error::service_unavailable("prev-chunks lookup failed", rid)
        }
    }
}

async fn get_parents(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    // get_with_context fetches node_id alongside the chunk; reuse for simplicity.
    let parent_chunk = match chunk::get_with_context(&state.pool, id).await {
        Ok(c) => c,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("chunk `{id}` not found"), rid)
        }
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_chunk (for parents) failed");
            return error::service_unavailable("chunk lookup failed", rid);
        }
    };
    match node::parent_chain(&state.pool, parent_chunk.chunk.node_id).await {
        Ok(chain) => Json(chain).into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "parent_chain failed");
            error::service_unavailable("parent-chain lookup failed", rid)
        }
    }
}

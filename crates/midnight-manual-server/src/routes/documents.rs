//! `GET /v1/documents/:id` + `/chunks`.
//!
//! Read-only document navigation endpoints. Both are public reads;
//! bearer token only affects rate-limit tier.

use axum::extract::{Extension, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mnm_store::{entities::document, StoreError};
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

#[derive(Debug, Deserialize)]
struct WindowQuery {
    #[serde(default)]
    from: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}

const fn default_limit() -> usize {
    20
}

/// Mount the document read routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/documents/:id", get(get_document))
        .route("/v1/documents/:id/chunks", get(get_document_chunks))
}

async fn get_document(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match document::get_overview(&state.pool, id).await {
        Ok(ov) => Json(ov).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("document `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_document failed");
            error::service_unavailable("document lookup failed", rid)
        }
    }
}

async fn get_document_chunks(
    Path(id): Path<Uuid>,
    Query(q): Query<WindowQuery>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match document::list_chunks_window(&state.pool, id, q.from, q.limit).await {
        Ok(w) => Json(w).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("document `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_document_chunks failed");
            error::service_unavailable("document window failed", rid)
        }
    }
}

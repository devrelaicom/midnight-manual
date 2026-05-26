//! `GET /v1/documents/:id` + `/full` + `/chunks`.
//!
//! Read-only document navigation endpoints. All three are public reads;
//! bearer token only affects rate-limit tier.

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::{entities::document, StoreError};
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Hard cap on the number of chunks `/v1/documents/:id/full` will return
/// in one response. Above this, the endpoint returns 412 with a hint
/// pointing at the window endpoint.
///
/// Override for testing via the `MIDNIGHT_MANUAL_DOCUMENT_FULL_CHUNK_CAP`
/// environment variable.
pub const DOCUMENT_FULL_CHUNK_CAP: usize = 500;

/// Read the effective cap from the environment (test override) or the constant.
fn effective_cap() -> usize {
    std::env::var("MIDNIGHT_MANUAL_DOCUMENT_FULL_CHUNK_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DOCUMENT_FULL_CHUNK_CAP)
}

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
        .route("/v1/documents/:id/full", get(get_document_full))
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

async fn get_document_full(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let cap = effective_cap();
    match document::get_full(&state.pool, id, cap).await {
        Ok(document::FullResult::Document(d)) => Json(d).into_response(),
        Ok(document::FullResult::TooManyChunks { count, cap }) => (
            StatusCode::PRECONDITION_FAILED,
            Json(serde_json::json!({
                "error": "too_many_chunks",
                "chunk_count": count,
                "cap": cap,
                "hint": format!("Use GET /v1/documents/{id}/chunks?from=K&limit=L (default L=20)"),
            })),
        )
            .into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("document `{id}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_document_full failed");
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

//! `GET /v1/chunks?ids=` + `/v1/chunks/:id` + `/next` + `/prev` + `/parents`.
//!
//! Each endpoint returns a chunk row with its document and source context
//! bundled. The `/next` and `/prev` endpoints walk in `chunk_index` order;
//! `embed_failed` chunks are skipped. `/siblings` (unbounded) was removed
//! in favor of position-windowed `/v1/documents/:id/chunks`.

use std::collections::HashMap;

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
        .route("/v1/chunks", get(get_chunks_batch))
        .route("/v1/chunks/:id", get(get_chunk))
        .route("/v1/chunks/:id/next", get(get_next))
        .route("/v1/chunks/:id/prev", get(get_prev))
        .route("/v1/chunks/:id/parents", get(get_parents))
}

/// Hard cap on ids per batch request (matches the MCP `get_chunks` cap).
const BATCH_IDS_CAP: usize = 20;

/// Parse + validate the comma-separated id list. Exposed for unit tests.
fn parse_batch_ids(raw: &str) -> std::result::Result<Vec<Uuid>, String> {
    let mut ids = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        ids.push(
            part.parse::<Uuid>()
                .map_err(|_| format!("`{part}` is not a valid UUID"))?,
        );
    }
    if ids.is_empty() || ids.len() > BATCH_IDS_CAP {
        return Err(format!("ids must contain 1..={BATCH_IDS_CAP} UUIDs"));
    }
    Ok(ids)
}

#[derive(Debug, Deserialize)]
struct CountQuery {
    #[serde(default = "default_count")]
    count: usize,
}

const fn default_count() -> usize {
    5
}

#[derive(Debug, Deserialize)]
struct BatchQuery {
    /// Comma-separated chunk UUIDs.
    ids: String,
}

async fn get_chunks_batch(
    Query(q): Query<BatchQuery>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let ids = match parse_batch_ids(&q.ids) {
        Ok(ids) => ids,
        Err(message) => {
            return error::bad_request(
                message,
                format!("pass `ids` as 1..={BATCH_IDS_CAP} comma-separated chunk UUIDs"),
                rid,
            )
        }
    };
    match chunk::get_many_with_context(&state.pool, &ids).await {
        Ok(found) => {
            // Re-order to input order; collect ids that came back empty.
            // Duplicate input ids resolve to the first occurrence; repeats
            // land in `missing` (acceptable, documented behavior).
            let mut by_id: HashMap<Uuid, _> = found.into_iter().map(|c| (c.chunk.id, c)).collect();
            let mut chunks = Vec::with_capacity(ids.len());
            let mut missing = Vec::new();
            for id in &ids {
                match by_id.remove(id) {
                    Some(c) => chunks.push(c),
                    None => missing.push(*id),
                }
            }
            Json(serde_json::json!({ "chunks": chunks, "missing": missing })).into_response()
        }
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_chunks_batch failed");
            error::service_unavailable("batch chunk lookup failed", rid)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_batch_ids_accepts_valid_list() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let ids = parse_batch_ids(&format!("{a}, {b}")).unwrap();
        assert_eq!(ids, vec![a, b]);
    }

    #[test]
    fn parse_batch_ids_skips_empty_segments() {
        let a = Uuid::new_v4();
        let ids = parse_batch_ids(&format!(",{a},,")).unwrap();
        assert_eq!(ids, vec![a]);
    }

    #[test]
    fn parse_batch_ids_rejects_garbage_empty_and_overflow() {
        assert!(parse_batch_ids("not-a-uuid").is_err());
        assert!(parse_batch_ids("").is_err());
        assert!(parse_batch_ids(",, ,").is_err());
        let many = vec![Uuid::new_v4().to_string(); BATCH_IDS_CAP + 1].join(",");
        assert!(parse_batch_ids(&many).is_err());
    }

    #[test]
    fn parse_batch_ids_accepts_exactly_cap() {
        let many = vec![Uuid::new_v4().to_string(); BATCH_IDS_CAP].join(",");
        assert_eq!(parse_batch_ids(&many).unwrap().len(), BATCH_IDS_CAP);
    }
}

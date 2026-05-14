//! `/v1/sources/:slug/versions` — public source-version inspection.
//!
//! Anonymous reads: list every version (newest first) and show a single
//! version by revision. Admin promote / retire writes live in
//! [`super::admin_versions`].

use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_store::entities::{source, source_version};
use mn_store::StoreError;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Mount the public version-inspection routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sources/:slug/versions", get(list_versions))
        .route("/v1/sources/:slug/versions/:revision", get(get_version))
}

async fn list_versions(
    Path(slug): Path<String>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let src = match source::get_by_slug(&state.pool, &slug).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("source `{slug}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "list_versions", error = %e, "source lookup failed");
            return error::service_unavailable("source lookup failed", rid);
        }
    };
    match source_version::list_for_source(&state.pool, src.id).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "list_versions", error = %e, "store error");
            error::service_unavailable("list_versions failed", rid)
        }
    }
}

async fn get_version(
    Path((slug, revision)): Path<(String, i32)>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let src = match source::get_by_slug(&state.pool, &slug).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("source `{slug}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "get_version", error = %e, "source lookup failed");
            return error::service_unavailable("source lookup failed", rid);
        }
    };
    match source_version::get_by_revision(&state.pool, src.id, revision).await {
        Ok(row) => Json(row).into_response(),
        Err(StoreError::NotFound) => {
            error::not_found(format!("source `{slug}` has no revision {revision}"), rid)
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "get_version", error = %e, "store error");
            error::service_unavailable("get_version failed", rid)
        }
    }
}

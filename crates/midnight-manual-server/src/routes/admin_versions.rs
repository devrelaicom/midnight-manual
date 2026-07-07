//! Admin source-version write protocol (Phase 14).
//!
//! Two endpoints, both admin-tier gated (FR-058 + FR-117):
//!
//! 1. `POST /v1/admin/sources/{slug}/versions/{revision}/promote` — promote
//!    a previously-active version back to active (rollback per FR-072).
//!    Returns `{promoted_revision, demoted_revision}`.
//! 2. `POST /v1/admin/sources/{slug}/versions/{revision}/retire` — mark a
//!    single historical version retired so the source-version retention
//!    sweep can hard-delete it on its next tick.

use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mnm_core::error::{Error as CoreError, ErrorCode};
use mnm_store::entities::{source, source_version};
use mnm_store::StoreError;
use serde::Serialize;

use crate::app::AppState;
use crate::error;
use crate::middleware::bearer::AuthContext;
use crate::middleware::request_id::RequestId;

/// Mount the admin source-version routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/sources/{slug}/versions/{revision}/promote", post(promote_version))
        .route("/v1/admin/sources/{slug}/versions/{revision}/retire", post(retire_version))
}

/// Response shape for `POST .../promote`. Contains the promoted revision
/// and the revision that was demoted from active to inactive, if any.
#[derive(Debug, Serialize)]
pub struct PromoteResult {
    /// Revision that was just promoted to `active`.
    pub promoted_revision: i32,
    /// Revision that was demoted from `active` to `inactive`, if any.
    pub demoted_revision: Option<i32>,
}

async fn promote_version(
    Path((slug, revision)): Path<(String, i32)>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    let src = match source::get_by_slug(&state.pool, &slug).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("source `{slug}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "promote_version", error = %e, "source lookup failed");
            return error::service_unavailable("source lookup failed", rid);
        }
    };

    match source_version::promote_by_revision(&state.pool, src.id, revision).await {
        Ok((promoted, demoted)) => Json(PromoteResult {
            promoted_revision: promoted,
            demoted_revision: demoted,
        })
        .into_response(),
        Err(StoreError::NotFound) => {
            error::not_found(format!("source `{slug}` has no revision {revision}"), rid)
        }
        Err(StoreError::CheckViolation(msg)) => error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(msg)
                .remediation("supply a revision that is currently in `inactive` state")
                .build(),
            rid,
        ),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "promote_version", error = %e, "promote failed");
            error::service_unavailable("promote failed", rid)
        }
    }
}

async fn retire_version(
    Path((slug, revision)): Path<(String, i32)>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    let src = match source::get_by_slug(&state.pool, &slug).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("source `{slug}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "retire_version", error = %e, "source lookup failed");
            return error::service_unavailable("source lookup failed", rid);
        }
    };

    let existing = match source_version::get_by_revision(&state.pool, src.id, revision).await {
        Ok(row) => row,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("source `{slug}` has no revision {revision}"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "retire_version", error = %e, "sv lookup failed");
            return error::service_unavailable("version lookup failed", rid);
        }
    };
    if existing.is_active {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!("refusing to retire the active version (`{slug}` rev {revision})"))
                .remediation(
                    "promote another revision first, then retire — \
                     the active version is never retired directly",
                )
                .build(),
            rid,
        );
    }

    match source_version::retire(&state.pool, existing.id).await {
        Ok(()) => match source_version::get_by_id(&state.pool, existing.id).await {
            Ok(row) => Json(row).into_response(),
            Err(e) => {
                tracing::warn!(request_id = rid, op = "retire_version", error = %e, "readback failed");
                error::service_unavailable("version readback failed", rid)
            }
        },
        Err(StoreError::NotFound) => {
            error::not_found(format!("source `{slug}` has no revision {revision}"), rid)
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "retire_version", error = %e, "retire failed");
            error::service_unavailable("retire failed", rid)
        }
    }
}

fn admin_reject(rid: &str, auth: Option<&Extension<AuthContext>>) -> Option<Response> {
    match auth {
        None => Some(error::into_response(
            CoreError::builder(ErrorCode::Unauthorized)
                .message("admin bearer required")
                .remediation("obtain an admin token via `mnm login` and retry")
                .build(),
            rid,
        )),
        Some(Extension(ctx)) if ctx.can_admin() => None,
        Some(_) => Some(error::into_response(
            CoreError::builder(ErrorCode::Forbidden)
                .message("admin tier required for version writes")
                .remediation("read-uplift tokens may not write — request admin tier")
                .build(),
            rid,
        )),
    }
}

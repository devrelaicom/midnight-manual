//! Admin embedding-token-limit override CRUD (Phase 4 / Task 4.8).
//!
//! Four endpoints for operator-side management of per-subject embedding-token
//! ceilings; all admin-tier gated (FR-031, FR-058). A subject is either a CIDR
//! block (applied to anonymous, IP-keyed requests) or a user id (applied to a
//! JWT holder's `sub`). After any successful mutation the in-process override
//! cache is refreshed so the change takes effect within the request rather than
//! waiting for the next periodic refresh tick.
//!
//! 1. `POST   /v1/admin/tokenlimits` — create an override.
//! 2. `GET    /v1/admin/tokenlimits` — list overrides still in effect.
//! 3. `PATCH  /v1/admin/tokenlimits/:id` — extend / adjust one.
//! 4. `DELETE /v1/admin/tokenlimits/:id` — hard-delete one.
//!
//! The validation helpers (`admin_reject`, `sub_of`, `parse_future_timestamp`,
//! `validate_cidr`) are shared with [`crate::routes::admin_ratelimits`] to keep
//! the two admin-override surfaces behaving identically; the 400 envelope comes
//! from [`crate::error::bad_request`].

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{patch, post};
use axum::{Json, Router};
use mnm_store::entities::token_limit_override::{self, Patch};
use mnm_store::StoreError;
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::error::{self, bad_request};
use crate::middleware::bearer::AuthContext;
use crate::middleware::request_id::RequestId;
use crate::routes::admin_ratelimits::{
    admin_reject, parse_future_timestamp, sub_of, validate_cidr,
};

/// Mount the admin token-limit routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/tokenlimits", post(create).get(list))
        .route("/v1/admin/tokenlimits/:id", patch(update).delete(remove))
}

/// Body of `POST /v1/admin/tokenlimits`.
#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    /// Either `"cidr"` (network block, applied to anonymous IP-keyed requests)
    /// or `"user"` (applied to a JWT holder's `sub`).
    pub subject_kind: String,
    /// The subject value: a CIDR block (e.g. `203.0.113.0/24`) when
    /// `subject_kind == "cidr"`, otherwise a user id.
    pub subject: String,
    /// Per-hour token ceiling. Must be non-negative (`0` denies all spend).
    pub hourly: i64,
    /// Per-day token ceiling. Must be non-negative (`0` denies all spend).
    pub daily: i64,
    /// RFC 3339 timestamp at which the override stops applying. Must be in the
    /// future.
    pub expires_at: String,
    /// Optional operator note.
    #[serde(default)]
    pub note: Option<String>,
}

/// Body of `PATCH /v1/admin/tokenlimits/:id`. All fields optional; an empty
/// body returns the current row unchanged.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateRequest {
    /// New RFC 3339 expiry. Must be in the future when set.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// New per-hour token ceiling. Must be non-negative when set.
    #[serde(default)]
    pub hourly: Option<i64>,
    /// New per-day token ceiling. Must be non-negative when set.
    #[serde(default)]
    pub daily: Option<i64>,
    /// New operator note.
    #[serde(default)]
    pub note: Option<String>,
}

/// Validate a `subject_kind` + `subject` pair. Returns `Some(response)` on a
/// bad kind or (for CIDR) a malformed network block.
fn validate_subject(subject_kind: &str, subject: &str, rid: &str) -> Option<Response> {
    match subject_kind {
        "cidr" => validate_cidr(subject, rid),
        "user" => None,
        _ => Some(bad_request(
            format!("subject_kind `{subject_kind}` is not valid"),
            "supply subject_kind `cidr` or `user`",
            rid,
        )),
    }
}

/// Reject a negative `hourly`/`daily`. Zero is allowed (denies all spend).
fn validate_ceiling(label: &str, value: i64, rid: &str) -> Option<Response> {
    if value < 0 {
        Some(bad_request(
            format!("{label} must be a non-negative integer"),
            format!("supply {label} >= 0"),
            rid,
        ))
    } else {
        None
    }
}

/// Refresh the in-process override cache after a mutation so it takes effect
/// within this request. Best-effort: a refresh failure is logged and the
/// periodic refresh job will reconcile on its next tick.
async fn refresh_after_mutation(state: &AppState, rid: &str, op: &str) {
    if let Err(e) = state.token_limiter.refresh_overrides_now(&state.pool).await {
        tracing::warn!(request_id = rid, op, error = %e, "token-limit override refresh failed");
    }
}

async fn create(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    Json(req): Json<CreateRequest>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }
    let created_by = sub_of(auth.as_ref());

    if let Some(resp) = validate_subject(&req.subject_kind, &req.subject, rid) {
        return resp;
    }
    if let Some(resp) = validate_ceiling("hourly", req.hourly, rid) {
        return resp;
    }
    if let Some(resp) = validate_ceiling("daily", req.daily, rid) {
        return resp;
    }
    let expires_at = match parse_future_timestamp(&req.expires_at) {
        Ok(ts) => ts,
        Err(e) => return e.into_response(rid),
    };

    match token_limit_override::insert(
        &state.pool,
        &req.subject_kind,
        &req.subject,
        req.hourly,
        req.daily,
        expires_at,
        req.note.as_deref(),
        &created_by,
    )
    .await
    {
        Ok(row) => {
            refresh_after_mutation(&state, rid, "create_tokenlimit").await;
            (StatusCode::CREATED, Json(row)).into_response()
        }
        Err(StoreError::CheckViolation(msg)) => {
            bad_request(msg, "check the override against the token_limit_override constraints", rid)
        }
        Err(StoreError::Database(msg)) => {
            // A malformed CIDR that slipped past validation surfaces here.
            tracing::warn!(request_id = rid, op = "create_tokenlimit", error = %msg, "insert rejected");
            bad_request(
                format!("could not store override: {msg}"),
                "supply a well-formed CIDR such as `203.0.113.0/24` for a `cidr` subject",
                rid,
            )
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "create_tokenlimit", error = %e, "insert failed");
            error::service_unavailable("token-limit override insert failed", rid)
        }
    }
}

async fn list(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }
    match token_limit_override::list_active(&state.pool).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "list_tokenlimits", error = %e, "list failed");
            error::service_unavailable("token-limit override list failed", rid)
        }
    }
}

async fn update(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    Json(req): Json<UpdateRequest>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    if let Some(h) = req.hourly {
        if let Some(resp) = validate_ceiling("hourly", h, rid) {
            return resp;
        }
    }
    if let Some(d) = req.daily {
        if let Some(resp) = validate_ceiling("daily", d, rid) {
            return resp;
        }
    }
    let expires_at = match req.expires_at.as_deref() {
        Some(s) => match parse_future_timestamp(s) {
            Ok(ts) => Some(ts),
            Err(e) => return e.into_response(rid),
        },
        None => None,
    };

    let patch = Patch {
        expires_at,
        hourly: req.hourly,
        daily: req.daily,
        note: req.note,
    };

    match token_limit_override::update(&state.pool, id, patch).await {
        Ok(row) => {
            refresh_after_mutation(&state, rid, "update_tokenlimit").await;
            Json(row).into_response()
        }
        Err(StoreError::NotFound) => {
            error::not_found(format!("token-limit override `{id}` not found"), rid)
        }
        Err(StoreError::CheckViolation(msg)) => {
            bad_request(msg, "check the patch against the override constraints", rid)
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "update_tokenlimit", error = %e, "update failed");
            error::service_unavailable("token-limit override update failed", rid)
        }
    }
}

async fn remove(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }
    match token_limit_override::delete(&state.pool, id).await {
        Ok(row) => {
            refresh_after_mutation(&state, rid, "delete_tokenlimit").await;
            Json(row).into_response()
        }
        Err(StoreError::NotFound) => {
            error::not_found(format!("token-limit override `{id}` not found"), rid)
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "delete_tokenlimit", error = %e, "delete failed");
            error::service_unavailable("token-limit override delete failed", rid)
        }
    }
}

//! Admin rate-limit override CRUD (Phase 16).
//!
//! Four endpoints for operator-side management of per-CIDR rate-limit
//! ceilings; all admin-tier gated (FR-031, FR-058). This phase manages the
//! `rate_limit_override` rows only — enforcement (token buckets, headers,
//! 429s, longest-prefix matching) is a separate concern.
//!
//! 1. `POST   /v1/admin/ratelimits` — create an override.
//! 2. `GET    /v1/admin/ratelimits` — list overrides still in effect.
//! 3. `PATCH  /v1/admin/ratelimits/:id` — extend / adjust one.
//! 4. `DELETE /v1/admin/ratelimits/:id` — hard-delete one.

use std::net::IpAddr;

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{patch, post};
use axum::{Json, Router};
use mn_core::error::{Error as CoreError, ErrorCode};
use mn_store::entities::rate_limit_override::{self, RateLimitPatch};
use mn_store::StoreError;
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::bearer::AuthContext;
use crate::middleware::request_id::RequestId;

/// Mount the admin rate-limit routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/ratelimits", post(create_override).get(list_overrides))
        .route("/v1/admin/ratelimits/:id", patch(update_override).delete(delete_override))
}

/// Body of `POST /v1/admin/ratelimits`.
#[derive(Debug, Deserialize)]
pub struct CreateOverrideRequest {
    /// Network block in `addr/prefix` form (host bits are masked off).
    pub cidr: String,
    /// Requests-per-second ceiling. Must be positive.
    pub limit_rps: i32,
    /// RFC 3339 timestamp at which the override stops applying. Must be in
    /// the future.
    pub expires_at: String,
    /// Optional operator note.
    #[serde(default)]
    pub note: Option<String>,
}

/// Body of `PATCH /v1/admin/ratelimits/:id`. All fields optional; an empty
/// body returns the current row unchanged.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateOverrideRequest {
    /// New RFC 3339 expiry. Must be in the future when set.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// New requests-per-second ceiling. Must be positive when set.
    #[serde(default)]
    pub limit_rps: Option<i32>,
    /// New operator note.
    #[serde(default)]
    pub note: Option<String>,
}

async fn create_override(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    Json(req): Json<CreateOverrideRequest>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }
    let created_by = sub_of(auth.as_ref());

    if let Some(resp) = validate_cidr(&req.cidr, rid) {
        return resp;
    }
    if req.limit_rps <= 0 {
        return bad_request("limit_rps must be a positive integer", "supply limit_rps >= 1", rid);
    }
    let expires_at = match parse_future_timestamp(&req.expires_at) {
        Ok(ts) => ts,
        Err(e) => return e.into_response(rid),
    };

    match rate_limit_override::insert(
        &state.pool,
        &req.cidr,
        req.limit_rps,
        expires_at,
        req.note.as_deref(),
        &created_by,
    )
    .await
    {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        Err(StoreError::CheckViolation(msg)) => {
            bad_request(msg, "check the override against the rate_limit_override constraints", rid)
        }
        Err(StoreError::Database(msg)) => {
            // A malformed CIDR that slipped past validation surfaces here.
            tracing::warn!(request_id = rid, op = "create_override", error = %msg, "insert rejected");
            bad_request(
                format!("could not store override: {msg}"),
                "supply a well-formed CIDR such as `203.0.113.0/24`",
                rid,
            )
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "create_override", error = %e, "insert failed");
            error::service_unavailable("override insert failed", rid)
        }
    }
}

async fn list_overrides(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }
    match rate_limit_override::list_active(&state.pool).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "list_overrides", error = %e, "list failed");
            error::service_unavailable("override list failed", rid)
        }
    }
}

async fn update_override(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    Json(req): Json<UpdateOverrideRequest>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    if let Some(rps) = req.limit_rps {
        if rps <= 0 {
            return bad_request(
                "limit_rps must be a positive integer",
                "supply limit_rps >= 1",
                rid,
            );
        }
    }
    let expires_at = match req.expires_at.as_deref() {
        Some(s) => match parse_future_timestamp(s) {
            Ok(ts) => Some(ts),
            Err(e) => return e.into_response(rid),
        },
        None => None,
    };

    let patch = RateLimitPatch {
        expires_at,
        limit_rps: req.limit_rps,
        note: req.note,
    };

    match rate_limit_override::update(&state.pool, id, patch).await {
        Ok(row) => Json(row).into_response(),
        Err(StoreError::NotFound) => {
            error::not_found(format!("rate-limit override `{id}` not found"), rid)
        }
        Err(StoreError::CheckViolation(msg)) => {
            bad_request(msg, "check the patch against the override constraints", rid)
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "update_override", error = %e, "update failed");
            error::service_unavailable("override update failed", rid)
        }
    }
}

async fn delete_override(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }
    match rate_limit_override::delete(&state.pool, id).await {
        Ok(row) => Json(row).into_response(),
        Err(StoreError::NotFound) => {
            error::not_found(format!("rate-limit override `{id}` not found"), rid)
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "delete_override", error = %e, "delete failed");
            error::service_unavailable("override delete failed", rid)
        }
    }
}

/// Returns `Some(response)` to short-circuit the handler with an auth-failure
/// response, or `None` when the caller is admin-authorised.
///
/// The 403 names both the caller's role and the required role (EC-61).
///
/// Shared with [`crate::routes::admin_tokenlimits`] so both admin-override CRUD
/// surfaces enforce auth identically.
pub(crate) fn admin_reject(rid: &str, auth: Option<&Extension<AuthContext>>) -> Option<Response> {
    match auth {
        None => Some(error::into_response(
            CoreError::builder(ErrorCode::Unauthorized)
                .message("admin bearer required")
                .remediation("obtain an admin token via `mnm login` and retry")
                .build(),
            rid,
        )),
        Some(Extension(ctx)) if ctx.can_admin() => None,
        Some(Extension(ctx)) => Some(error::into_response(
            CoreError::builder(ErrorCode::Forbidden)
                .message(format!("your role `{}` lacks permission for this endpoint", ctx.role))
                .remediation("required role: admin — request admin tier")
                .build(),
            rid,
        )),
    }
}

/// Extract the caller's `sub` claim (the admin user_id) for `created_by`.
/// Only meaningful after [`admin_reject`] has confirmed an admin caller.
pub(crate) fn sub_of(auth: Option<&Extension<AuthContext>>) -> String {
    auth.map(|Extension(ctx)| ctx.sub.clone())
        .unwrap_or_default()
}

/// Validate a `addr/prefix` (or bare address) CIDR string without pulling in an
/// IP-network dependency. Returns `Some(response)` describing the problem.
///
/// Shared with [`crate::routes::admin_tokenlimits`], which validates the
/// `subject` of a `subject_kind == "cidr"` token-limit override the same way.
pub(crate) fn validate_cidr(cidr: &str, rid: &str) -> Option<Response> {
    let bad = |reason: &str| -> Response {
        bad_request(
            format!("invalid cidr `{cidr}`: {reason}"),
            "supply an address/prefix such as `203.0.113.0/24` or `2001:db8::/32`",
            rid,
        )
    };
    let (addr, prefix) = match cidr.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (cidr, None),
    };
    let ip: IpAddr = match addr.parse() {
        Ok(ip) => ip,
        Err(_) => return Some(bad("address is not a valid IPv4 or IPv6 address")),
    };
    if let Some(p) = prefix {
        let Ok(bits) = p.parse::<u8>() else {
            return Some(bad("prefix length is not an integer"));
        };
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if bits > max {
            return Some(bad(&format!(
                "prefix length must be in 0..={max} for this address family"
            )));
        }
    }
    None
}

/// Why a supplied `expires_at` was rejected. Kept small so the parse helper's
/// `Result` stays cheap (avoids `clippy::result_large_err` from carrying a
/// whole `Response`).
///
/// Shared with [`crate::routes::admin_tokenlimits`] via [`parse_future_timestamp`].
pub(crate) enum TimestampError {
    /// Not a parseable RFC 3339 timestamp.
    Malformed(String),
    /// Parses, but is not in the future.
    NotFuture,
}

impl TimestampError {
    pub(crate) fn into_response(self, rid: &str) -> Response {
        match self {
            Self::Malformed(s) => bad_request(
                format!("`{s}` is not a valid RFC 3339 timestamp"),
                "supply an RFC 3339 timestamp such as `2026-06-01T00:00:00Z`",
                rid,
            ),
            Self::NotFuture => bad_request(
                "expires_at must be in the future",
                "supply a timestamp later than now",
                rid,
            ),
        }
    }
}

/// Parse an RFC 3339 timestamp and require it be in the future.
pub(crate) fn parse_future_timestamp(
    s: &str,
) -> std::result::Result<OffsetDateTime, TimestampError> {
    let ts =
        OffsetDateTime::parse(s, &Rfc3339).map_err(|_| TimestampError::Malformed(s.to_owned()))?;
    if ts <= OffsetDateTime::now_utc() {
        return Err(TimestampError::NotFuture);
    }
    Ok(ts)
}

/// Build a `400 invalid_request` response.
///
/// Shared with [`crate::routes::admin_tokenlimits`].
pub(crate) fn bad_request(
    message: impl Into<String>,
    remediation: impl Into<String>,
    rid: &str,
) -> Response {
    error::into_response(
        CoreError::builder(ErrorCode::InvalidRequest)
            .message(message)
            .remediation(remediation)
            .build(),
        rid,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_validator_accepts_valid_blocks() {
        assert!(validate_cidr("203.0.113.0/24", "rid").is_none());
        assert!(validate_cidr("169.155.237.15/25", "rid").is_none());
        assert!(validate_cidr("10.0.0.1", "rid").is_none());
        assert!(validate_cidr("2001:db8::/32", "rid").is_none());
        assert!(validate_cidr("::1/128", "rid").is_none());
    }

    #[test]
    fn cidr_validator_rejects_garbage() {
        assert!(validate_cidr("", "rid").is_some());
        assert!(validate_cidr("not-an-ip", "rid").is_some());
        assert!(validate_cidr("203.0.113.0/33", "rid").is_some());
        assert!(validate_cidr("2001:db8::/129", "rid").is_some());
        assert!(validate_cidr("10.0.0.0/abc", "rid").is_some());
    }
}

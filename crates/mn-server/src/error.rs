//! Convert mn-core's typed error envelope into an axum response.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mn_core::error::{Error as CoreError, ErrorCode};
use serde::Serialize;

/// Wire shape of an HTTP error response: `{ error: {...}, request_id }`.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// The typed error envelope.
    pub error: CoreError,
    /// Request id for log correlation (FR-029 / FR-106).
    pub request_id: String,
}

/// Build an [`IntoResponse`] from a [`CoreError`]. The actual `request_id` is
/// substituted by the middleware-extracted id at response time; the value here
/// is a placeholder for callers that don't have access to the request.
#[must_use]
pub fn into_response(err: CoreError, request_id: impl Into<String>) -> Response {
    let status =
        StatusCode::from_u16(err.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = ErrorBody {
        error: err,
        request_id: request_id.into(),
    };
    (status, Json(body)).into_response()
}

/// Convenience for the common 404 path.
#[must_use]
pub fn not_found(message: impl Into<String>, request_id: impl Into<String>) -> Response {
    let err = CoreError::builder(ErrorCode::NotFound)
        .message(message)
        .remediation("verify the resource id and try again")
        .build();
    into_response(err, request_id)
}

/// Convenience for the 503 path used when the DB is briefly unavailable.
#[must_use]
pub fn service_unavailable(reason: impl Into<String>, request_id: impl Into<String>) -> Response {
    let err = CoreError::builder(ErrorCode::ServiceUnavailable)
        .message(reason)
        .remediation("retry with backoff; see Retry-After header")
        .build();
    into_response(err, request_id)
}

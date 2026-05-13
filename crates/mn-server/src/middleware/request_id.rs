//! Request-ID middleware (FR-029 / FR-106). Reads `X-Request-Id` from the
//! incoming request or mints a new UUID, then echoes it on the response.

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

/// Header name used to convey the request id end-to-end.
pub const HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// axum middleware layer that ensures every request has a stable `request_id`
/// available in tracing fields AND echoed back to the caller.
pub async fn layer(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map_or_else(|| Uuid::new_v4().to_string(), str::to_owned);

    // Surface the id into tracing so all log lines for this request carry it.
    let span = tracing::info_span!("http_request", request_id = %id);
    let _enter = span.enter();

    // Make sure the request retains the header so handlers can read it.
    if let Ok(value) = HeaderValue::from_str(&id) {
        req.headers_mut().insert(HEADER.clone(), value.clone());
    }

    let mut response = next.run(req).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(HEADER.clone(), value);
    }
    response
}

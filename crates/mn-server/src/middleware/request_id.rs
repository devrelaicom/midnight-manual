//! Request-ID middleware (FR-029 / FR-106). Reads `X-Request-Id` from the
//! incoming request or mints a new UUID, then echoes it on the response.

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument as _;
use uuid::Uuid;

/// Header name used to convey the request id end-to-end.
pub const HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Newtype carrying the per-request id through the handler extension stack so
/// route handlers can populate the `request_id` field of typed error bodies
/// (FR-029 / FR-106). Wrapped in `axum::Extension` by [`layer`].
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

impl RequestId {
    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// axum middleware layer that ensures every request has a stable `request_id`
/// available in tracing fields, in request extensions, and echoed back to the
/// caller as `X-Request-Id`.
pub async fn layer(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map_or_else(|| Uuid::new_v4().to_string(), str::to_owned);

    // Make sure the request retains the header so handlers can read it, and
    // expose the id via Extension so handlers don't need to parse headers.
    if let Ok(value) = HeaderValue::from_str(&id) {
        req.headers_mut().insert(HEADER.clone(), value);
    }
    req.extensions_mut().insert(RequestId(id.clone()));

    // Span instrumentation — NOTE: use `.instrument(span)` rather than
    // `span.enter()` because the entered guard is not Send/cannot safely
    // cross `.await` points. With `instrument` the span follows the future
    // wherever it gets polled.
    let span = tracing::info_span!("http_request", request_id = %id);
    let mut response = next.run(req).instrument(span).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(HEADER.clone(), value);
    }
    response
}

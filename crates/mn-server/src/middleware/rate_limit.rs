//! Rate-limit middleware (Phase 17). Resolves the caller's tier, charges one
//! token against the in-process [`RateLimiter`](crate::ratelimit::RateLimiter),
//! sets `X-RateLimit-*` headers,
//! and returns `429` with `Retry-After` when the bucket is empty. A no-op when
//! the limiter is absent (rate limiting disabled).

use axum::extract::{Extension, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use mn_core::error::{Error as CoreError, ErrorCode};

use crate::app::AppState;
use crate::error;
use crate::middleware::bearer::AuthContext;
use crate::middleware::request_id::RequestId;
use crate::ratelimit::{Decision, Key, Tier};

/// Resolved decision stashed in request extensions so handlers can charge
/// additional tokens against the same bucket (the multi-query D25 cost lands
/// here in a later story).
#[derive(Debug, Clone)]
pub struct RateLimitContext {
    /// The bucket key the request was charged against.
    pub key: Key,
    /// The tier the request resolved to.
    pub tier: Tier,
    /// The tier's limit in requests/sec.
    pub limit: u32,
}

const HDR_LIMIT: HeaderName = HeaderName::from_static("x-ratelimit-limit");
const HDR_REMAINING: HeaderName = HeaderName::from_static("x-ratelimit-remaining");
const HDR_RESET: HeaderName = HeaderName::from_static("x-ratelimit-reset");
const HDR_RETRY_AFTER: HeaderName = HeaderName::from_static("retry-after");

/// Paths that are never rate-limited (operational endpoints).
fn is_exempt(path: &str) -> bool {
    matches!(path, "/healthz" | "/readyz" | "/metrics")
}

/// Extract the client IP from the configured proxy header, falling back to the
/// first `X-Forwarded-For` entry, then a shared `"unknown"` bucket.
fn client_ip(headers: &HeaderMap, header_name: &str) -> String {
    if let Some(v) = headers.get(header_name).and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_owned();
        }
    }
    if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = v.split(',').next() {
            let first = first.trim();
            if !first.is_empty() {
                return first.to_owned();
            }
        }
    }
    "unknown".to_owned()
}

fn set_u32(headers: &mut HeaderMap, name: &HeaderName, value: u32) {
    if let Ok(v) = HeaderValue::from_str(&value.to_string()) {
        headers.insert(name.clone(), v);
    }
}

fn set_u64(headers: &mut HeaderMap, name: &HeaderName, value: u64) {
    if let Ok(v) = HeaderValue::from_str(&value.to_string()) {
        headers.insert(name.clone(), v);
    }
}

/// axum middleware. Wire via `from_fn_with_state(state.clone(), rate_limit::layer)`.
pub async fn layer(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(limiter) = state.rate_limiter.clone() else {
        return next.run(req).await;
    };
    if is_exempt(req.uri().path()) {
        return next.run(req).await;
    }

    let ip = client_ip(req.headers(), &state.cfg.rate_limit_client_ip_header);
    let auth_ctx = auth.as_ref().map(|Extension(c)| c);
    let (key, tier, limit) = limiter.resolve(&ip, auth_ctx);

    match limiter.charge(&key, limit, 1) {
        Decision::Rejected { retry_after_secs } => {
            tracing::info!(
                request_id = req_id.as_str(),
                rate_limit_decision = "rejected",
                tier = tier.as_str(),
                "rate limit exceeded"
            );
            let body = CoreError::builder(ErrorCode::RateLimited)
                .message(format!(
                    "rate limit exceeded for the {} tier ({limit} req/s)",
                    tier.as_str()
                ))
                .remediation(format!("retry after {retry_after_secs}s or request a higher tier"))
                .build();
            let mut resp = error::into_response(body, req_id.as_str());
            let h = resp.headers_mut();
            set_u32(h, &HDR_LIMIT, limit);
            set_u32(h, &HDR_REMAINING, 0);
            set_u64(h, &HDR_RESET, retry_after_secs);
            set_u64(h, &HDR_RETRY_AFTER, retry_after_secs);
            resp
        }
        Decision::Allowed { .. } => {
            tracing::info!(
                request_id = req_id.as_str(),
                rate_limit_decision = "allowed",
                tier = tier.as_str(),
                "rate limit ok"
            );
            req.extensions_mut()
                .insert(RateLimitContext { key: key.clone(), tier, limit });
            let mut resp = next.run(req).await;
            // Re-peek (charge 0) AFTER the handler so the headers reflect any
            // extra tokens the handler charged for a multi-query request
            // (D25) — acceptance #5 wants the post-charge balance.
            let (remaining, reset_secs) = match limiter.charge(&key, limit, 0) {
                Decision::Allowed { remaining, reset_secs } => (remaining, reset_secs),
                Decision::Rejected { retry_after_secs } => (0, retry_after_secs),
            };
            let h = resp.headers_mut();
            set_u32(h, &HDR_LIMIT, limit);
            set_u32(h, &HDR_REMAINING, remaining);
            set_u64(h, &HDR_RESET, reset_secs);
            resp
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_ip_prefers_configured_header_then_xff_then_unknown() {
        let mut h = HeaderMap::new();
        assert_eq!(client_ip(&h, "fly-client-ip"), "unknown");
        h.insert("x-forwarded-for", HeaderValue::from_static("1.2.3.4, 5.6.7.8"));
        assert_eq!(client_ip(&h, "fly-client-ip"), "1.2.3.4");
        h.insert("fly-client-ip", HeaderValue::from_static("9.9.9.9"));
        assert_eq!(client_ip(&h, "fly-client-ip"), "9.9.9.9");
    }

    #[test]
    fn exempt_paths() {
        assert!(is_exempt("/healthz"));
        assert!(is_exempt("/metrics"));
        assert!(!is_exempt("/v1/search"));
    }
}

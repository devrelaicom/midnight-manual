//! Rate-limit middleware (Phase 17). Resolves the caller's tier, charges one
//! token against the in-process [`RateLimiter`](crate::ratelimit::RateLimiter),
//! sets `X-RateLimit-*` headers,
//! and returns `429` with `Retry-After` when the bucket is empty. A no-op when
//! the limiter is absent (rate limiting disabled).

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Extension, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use mnm_core::error::{Error as CoreError, ErrorCode};

use crate::app::AppState;
use crate::error;
use crate::middleware::bearer::{unauthorized_response, AuthContext, BearerRejection};
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

/// Resolve the client IP used to key the rate limiter and token limiter.
///
/// Preference order (issue #176 L15):
/// 1. The configured trusted-proxy header (default `fly-client-ip`). Only a
///    trusted front proxy can set it, so it is authoritative behind fly.io.
/// 2. The socket peer address, used when the trusted header is absent
///    (misconfiguration or direct access). We deliberately do NOT fall back to
///    the client-controlled `X-Forwarded-For`: a caller could rotate it to mint
///    a fresh anonymous bucket per request and evade per-IP ceilings. The peer
///    address is the kernel-observed source and cannot be spoofed at this layer.
/// 3. A shared `"unknown"` bucket when neither is available (fail closed).
pub(crate) fn client_ip(headers: &HeaderMap, header_name: &str, peer: Option<IpAddr>) -> String {
    if let Some(v) = headers.get(header_name).and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_owned();
        }
    }
    if let Some(ip) = peer {
        return ip.to_string();
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
///
/// Runs INSIDE (after) the bearer layer, so a valid token's [`AuthContext`] is
/// present for tier resolution while an invalid token arrives as a deferred
/// [`BearerRejection`]. Invalid-bearer requests are charged here (so 401 floods
/// are rate-limited) and only then converted to 401 — the handler never runs.
pub async fn layer(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    rejection: Option<Extension<BearerRejection>>,
    // NOTE: reads only the connect-info Extension set by
    // `into_make_service_with_connect_info` (production); it does NOT observe
    // axum's `MockConnectInfo` test helper — inject a peer addr in tests via
    // `.layer(Extension(ConnectInfo(addr)))`, not `MockConnectInfo`.
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    mut req: Request,
    next: Next,
) -> Response {
    let rejected = rejection.map(|Extension(r)| r.remediation);
    let peer_ip = peer.map(|Extension(ConnectInfo(sa))| sa.ip());

    // Pass-through paths (limiter disabled, or an operational exempt path) still
    // honor a deferred bearer rejection — an invalid token must be refused even
    // when rate limiting is off (the default). There is simply no bucket to
    // charge it against in these paths.
    let Some(limiter) = state.rate_limiter.clone() else {
        if let Some(remediation) = rejected {
            return unauthorized_response(req_id.as_str(), remediation);
        }
        return next.run(req).await;
    };
    if is_exempt(req.uri().path()) {
        if let Some(remediation) = rejected {
            return unauthorized_response(req_id.as_str(), remediation);
        }
        return next.run(req).await;
    }

    let ip = client_ip(req.headers(), &state.cfg.rate_limit_client_ip_header, peer_ip);
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
            // An invalid bearer was deferred by the bearer layer: it has now
            // been charged against the bucket (the point of issue #176 L14), so
            // return the 401 WITHOUT running the handler. The rate-limit headers
            // reflect the post-charge balance.
            if let Some(remediation) = rejected {
                let mut resp = unauthorized_response(req_id.as_str(), remediation);
                let (remaining, reset_secs) = match limiter.charge(&key, limit, 0) {
                    Decision::Allowed { remaining, reset_secs } => (remaining, reset_secs),
                    Decision::Rejected { retry_after_secs } => (0, retry_after_secs),
                };
                let h = resp.headers_mut();
                set_u32(h, &HDR_LIMIT, limit);
                set_u32(h, &HDR_REMAINING, remaining);
                set_u64(h, &HDR_RESET, reset_secs);
                return resp;
            }
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
    fn client_ip_prefers_configured_header_then_peer_then_unknown() {
        let peer: Option<IpAddr> = Some("203.0.113.7".parse().unwrap());
        let mut h = HeaderMap::new();
        // No header, no peer → the shared fail-closed bucket.
        assert_eq!(client_ip(&h, "fly-client-ip", None), "unknown");
        // No trusted header → key off the socket peer, never a spoofable header.
        assert_eq!(client_ip(&h, "fly-client-ip", peer), "203.0.113.7");
        // The configured proxy header wins over the peer when present.
        h.insert("fly-client-ip", HeaderValue::from_static("9.9.9.9"));
        assert_eq!(client_ip(&h, "fly-client-ip", peer), "9.9.9.9");
    }

    /// A client-supplied `X-Forwarded-For` must NOT be trusted for keying: with
    /// the configured proxy header absent we key off the socket peer, so a
    /// rotating XFF cannot mint a fresh anonymous bucket per request.
    #[test]
    fn client_ip_ignores_spoofable_x_forwarded_for() {
        let peer: Option<IpAddr> = Some("198.51.100.9".parse().unwrap());
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("1.2.3.4, 5.6.7.8"));
        // XFF is ignored → peer is used.
        assert_eq!(client_ip(&h, "fly-client-ip", peer), "198.51.100.9");
        // XFF is ignored → without a peer we fail closed rather than trust it.
        assert_eq!(client_ip(&h, "fly-client-ip", None), "unknown");
    }

    #[test]
    fn exempt_paths() {
        assert!(is_exempt("/healthz"));
        assert!(is_exempt("/metrics"));
        assert!(!is_exempt("/v1/search"));
    }
}

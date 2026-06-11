//! `GET /v1/me` — auth + limit introspection for clients (MCP `status`,
//! `mnm status`). Anonymous calls succeed and report `authenticated: false`.
//!
//! Callers carry TWO independent limit systems and this endpoint reports both:
//! the request rate limit (req/s token bucket, `ratelimit.rs`) and the
//! embedding token budget (rolling hourly/daily windows, `tokenlimit.rs`,
//! charged by `POST /v1/embeddings`).

use axum::extract::{Extension, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use time::OffsetDateTime;

use crate::app::AppState;
use crate::middleware::bearer::AuthContext;
use crate::middleware::rate_limit::RateLimitContext;
use crate::ratelimit::Decision;
use crate::tokenlimit::TokenTier;

/// Mount the introspection route.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/me", get(me))
}

const fn token_tier_str(t: TokenTier) -> &'static str {
    match t {
        TokenTier::Anonymous => "anonymous",
        TokenTier::ReadUplift => "read_uplift",
        TokenTier::Admin => "admin",
    }
}

async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthContext>>,
    rl: Option<Extension<RateLimitContext>>,
) -> Response {
    let auth = auth.map(|Extension(a)| a);
    let (auth_type, identity, permission_level) =
        auth.as_ref().map_or(("anonymous", None, "read"), |a| {
            let t = match a.tier {
                mn_auth::Tier::Admin => "admin",
                mn_auth::Tier::ReadUplift => "github_oauth",
            };
            let p = if a.can_admin() {
                "admin"
            } else if a.can_write() {
                "write"
            } else {
                "read"
            };
            (t, Some(a.sub.clone()), p)
        });
    // Request rate limit: peek the caller's bucket without spending a token
    // (cost 0). The RateLimitContext extension exists whenever the limiter is
    // enabled.
    let rate_limit = rl.and_then(|Extension(ctx)| {
        state.rate_limiter.as_ref().map(|limiter| {
            let (remaining, reset_secs) = match limiter.charge(&ctx.key, ctx.limit, 0) {
                Decision::Allowed { remaining, reset_secs } => (remaining, reset_secs),
                Decision::Rejected { retry_after_secs } => (0, retry_after_secs),
            };
            json!({
                "tier": ctx.tier.as_str(),
                "limit": ctx.limit,
                "remaining": remaining,
                "reset_secs": reset_secs,
            })
        })
    });
    // Embedding token budget: same resolve + non-consuming snapshot the
    // embeddings route uses (see `routes::embeddings` step 5).
    let client_ip =
        crate::middleware::rate_limit::client_ip(&headers, &state.cfg.rate_limit_client_ip_header);
    let (subject, token_tier, limits) = state.token_limiter.resolve(&client_ip, auth.as_ref());
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let usage = state.token_limiter.snapshot_for(&subject, limits, now);
    let window = |w: crate::tokenlimit::WindowInfo| json!({ "limit": w.limit, "remaining": w.remaining, "reset_at_secs": w.reset_at_secs });
    let token_limits = json!({
        "tier": token_tier_str(token_tier),
        "hourly": window(usage.hour),
        "daily": window(usage.day),
    });
    Json(json!({
        "authenticated": auth.is_some(),
        "auth_type": auth_type,
        "identity": identity,
        "permission_level": permission_level,
        "rate_limit": rate_limit,
        "token_limits": token_limits,
        "server_version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

//! `GET /v1/me` — auth + limit introspection for clients (MCP `status`,
//! `mnm status`). Anonymous calls succeed and report `authenticated: false`.
//!
//! Callers carry TWO independent limit systems and this endpoint reports both:
//! the request rate limit (req/s token bucket, `ratelimit.rs`) and the
//! embedding token budget (rolling hourly/daily windows, `tokenlimit.rs`,
//! charged by `POST /v1/embeddings`).

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Extension, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mnm_core::introspect::{MeRateLimit, MeResponse, MeTokenLimits, MeTokenWindow};
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
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Response {
    let auth = auth.map(|Extension(a)| a);
    let (auth_type, identity, permission_level) =
        auth.as_ref().map_or(("anonymous", None, "read"), |a| {
            // Use the auth tier's own wire vocabulary (`admin` / `read_uplift`)
            // so `auth_type` matches the JWT tier claim and the adjacent
            // rate_limit / token_limits `tier` fields rather than inventing a
            // third spelling.
            let t = a.tier.as_wire();
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
            MeRateLimit {
                tier: ctx.tier.as_str().to_owned(),
                limit: ctx.limit,
                remaining,
                reset_secs,
            }
        })
    });
    // Embedding token budget: same resolve + non-consuming snapshot the
    // embeddings route uses (see `routes::embeddings` step 5).
    let client_ip = crate::middleware::rate_limit::client_ip(
        &headers,
        &state.cfg.rate_limit_client_ip_header,
        peer.map(|ConnectInfo(sa)| sa.ip()),
    );
    let (subject, token_tier, limits) = state.token_limiter.resolve(&client_ip, auth.as_ref());
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let usage = state.token_limiter.snapshot_for(&subject, limits, now);
    let window = |w: crate::tokenlimit::WindowInfo| MeTokenWindow {
        limit: w.limit,
        remaining: w.remaining,
        reset_at_secs: w.reset_at_secs,
    };
    let token_limits = MeTokenLimits {
        tier: token_tier_str(token_tier).to_owned(),
        hourly: window(usage.hour),
        daily: window(usage.day),
    };
    Json(MeResponse {
        authenticated: auth.is_some(),
        auth_type: auth_type.to_owned(),
        identity,
        permission_level: permission_level.to_owned(),
        rate_limit,
        token_limits,
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use mnm_core::introspect::{MeRateLimit, MeResponse, MeTokenLimits, MeTokenWindow};

    use super::token_tier_str;
    use crate::ratelimit;
    use crate::tokenlimit::TokenTier;

    /// `auth_type` (the auth Tier wire string) and the `rate_limit.tier` label
    /// (the ratelimit Tier label) MUST agree on the shared names, so a
    /// GitHub-SSO holder doesn't read `read_uplift` in one field and a
    /// different spelling in the next. The two enums stay distinct
    /// (ratelimit::Tier additionally has CidrOverride / Anonymous, which have
    /// no JWT equivalent) — this is a compile/test-time link, not a merge.
    #[test]
    fn ratelimit_tier_labels_match_auth_wire_strings() {
        assert_eq!(ratelimit::Tier::Admin.as_str(), mnm_auth::Tier::Admin.as_wire());
        assert_eq!(ratelimit::Tier::ReadUplift.as_str(), mnm_auth::Tier::ReadUplift.as_wire());
    }

    /// Likewise the `token_limits.tier` label (`token_tier_str`) shares the
    /// admin / read_uplift names with the auth Tier wire vocabulary. TokenTier
    /// keeps its own Anonymous variant (no JWT equivalent).
    #[test]
    fn token_tier_labels_match_auth_wire_strings() {
        assert_eq!(token_tier_str(TokenTier::Admin), mnm_auth::Tier::Admin.as_wire());
        assert_eq!(token_tier_str(TokenTier::ReadUplift), mnm_auth::Tier::ReadUplift.as_wire());
    }

    /// The body `me` produces is structurally a `MeResponse`; this pins the
    /// server producer to the shared `mnm_core::introspect` shape that every
    /// consumer deserializes. A field rename on either side breaks this.
    #[test]
    fn me_response_round_trips_through_shared_contract() {
        let body = MeResponse {
            authenticated: true,
            auth_type: mnm_auth::Tier::ReadUplift.as_wire().to_owned(),
            identity: Some("octocat".to_owned()),
            permission_level: "read".to_owned(),
            rate_limit: Some(MeRateLimit {
                tier: ratelimit::Tier::ReadUplift.as_str().to_owned(),
                limit: 120,
                remaining: 87,
                reset_secs: 31,
            }),
            token_limits: MeTokenLimits {
                tier: token_tier_str(TokenTier::ReadUplift).to_owned(),
                hourly: MeTokenWindow {
                    limit: 200_000,
                    remaining: 150_000,
                    reset_at_secs: 1_200,
                },
                daily: MeTokenWindow {
                    limit: 2_000_000,
                    remaining: 1_900_000,
                    reset_at_secs: 50_000,
                },
            },
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let wire = serde_json::to_value(&body).expect("serialize");
        // Field names the consumers read, spelled out so a rename is caught here.
        assert_eq!(wire["auth_type"], "read_uplift");
        assert_eq!(wire["rate_limit"]["tier"], "read_uplift");
        assert_eq!(wire["rate_limit"]["reset_secs"], 31);
        assert_eq!(wire["token_limits"]["hourly"]["reset_at_secs"], 1_200);
        let back: MeResponse = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(body, back);
    }
}

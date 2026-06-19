//! Shared typed contract for the `GET /v1/me` introspection endpoint.
//!
//! `GET /v1/me` reports auth state plus TWO independent limit systems: the
//! request rate limit (req/s token bucket) and the embedding token budget
//! (rolling hourly/daily windows). Historically the server produced this body
//! ad-hoc with `serde_json::json!{}` and every consumer (`mnm status`, the MCP
//! `status` tool, the MCP render summary) reached into it with stringly-typed
//! `.get()` / `.pointer()` lookups — so a server-side field rename silently
//! degraded every reader. These types are the single shared shape the server
//! serializes and the consumers deserialize, so a rename is a compile error
//! instead of a silent regression.
//!
//! Tier vocabularies differ deliberately and are kept as `String`:
//! [`MeResponse::auth_type`] uses the auth tier vocabulary (`anonymous` /
//! `read_uplift` / `admin`), while [`MeRateLimit::tier`] and
//! [`MeTokenLimits::tier`] carry the wider rate-limit / token-limit tier
//! vocabularies (e.g. `cidr_override`, which has no JWT equivalent).

use serde::{Deserialize, Serialize};

/// The full `GET /v1/me` body: auth identity plus both limit systems.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeResponse {
    /// `true` when a bearer was presented and accepted.
    pub authenticated: bool,
    /// Auth tier vocabulary: `anonymous` (no/invalid bearer), `read_uplift`
    /// (GitHub-SSO uplift JWT), or `admin` (challenge-response JWT). Matches the
    /// JWT tier claim and the adjacent rate-limit / token-limit tier fields.
    pub auth_type: String,
    /// Identity string (GitHub login or admin user id), when authenticated.
    pub identity: Option<String>,
    /// `read` / `write` / `admin`.
    pub permission_level: String,
    /// Request rate-limit bucket state. `None` (serialized as `null`) when the
    /// limiter is disabled.
    pub rate_limit: Option<MeRateLimit>,
    /// Embedding token-budget windows (hourly + daily).
    pub token_limits: MeTokenLimits,
    /// The server's package version.
    pub server_version: String,
}

/// Request rate-limit bucket snapshot (req/s token bucket).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeRateLimit {
    /// Rate-limit tier vocabulary: `cidr_override` / `admin` / `read_uplift` /
    /// `anonymous` (wider than the auth tier — kept as a string).
    pub tier: String,
    /// Bucket capacity (requests the tier may spend per refill window).
    pub limit: u32,
    /// Whole tokens left in the caller's bucket right now.
    pub remaining: u32,
    /// Seconds until the rate-limit bucket refills (RELATIVE duration). NOT a
    /// timestamp — contrast [`MeTokenWindow::reset_at_secs`], which is absolute.
    pub reset_secs: u64,
}

/// Embedding token-budget: tier label plus the two rolling windows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeTokenLimits {
    /// Token-limit tier vocabulary: `anonymous` / `read_uplift` / `admin`
    /// (kept as a string to stay decoupled from the auth tier enum).
    pub tier: String,
    /// Rolling one-hour window.
    pub hourly: MeTokenWindow,
    /// Rolling one-day window.
    pub daily: MeTokenWindow,
}

/// One rolling token-budget window (hourly or daily).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeTokenWindow {
    /// Configured token ceiling for this window.
    pub limit: u64,
    /// Tokens remaining before the window is exhausted.
    pub remaining: u64,
    /// Unix timestamp (seconds) when the oldest token-budget bucket in this
    /// window expires (ABSOLUTE wall-clock instant). NOT a duration — contrast
    /// [`MeRateLimit::reset_secs`], which is a relative number of seconds.
    pub reset_at_secs: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MeResponse {
        MeResponse {
            authenticated: true,
            auth_type: "read_uplift".to_owned(),
            identity: Some("octocat".to_owned()),
            permission_level: "read".to_owned(),
            rate_limit: Some(MeRateLimit {
                tier: "read_uplift".to_owned(),
                limit: 120,
                remaining: 87,
                reset_secs: 31,
            }),
            token_limits: MeTokenLimits {
                tier: "read_uplift".to_owned(),
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
            server_version: "0.4.2".to_owned(),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let me = sample();
        let json = serde_json::to_value(&me).expect("serialize");
        let back: MeResponse = serde_json::from_value(json).expect("deserialize");
        assert_eq!(me, back);
    }

    #[test]
    fn disabled_rate_limit_serializes_as_null_and_round_trips() {
        let mut me = sample();
        me.rate_limit = None;
        let json = serde_json::to_value(&me).expect("serialize");
        assert!(json["rate_limit"].is_null(), "disabled limiter must emit null, not absent");
        let back: MeResponse = serde_json::from_value(json).expect("deserialize");
        assert_eq!(me, back);
    }
}

//! In-process rate-limit engine (Phase 17): token buckets, tier resolution,
//! and CIDR-override matching.
//!
//! Pure logic — no axum types — so the bucket math and CIDR matching are
//! unit-testable in isolation. The HTTP glue lives in
//! `crate::middleware::rate_limit`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use mn_store::entities::rate_limit_override;
use mn_store::StoreError;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::config::ServerConfig;
use crate::middleware::bearer::AuthContext;

/// Floor a non-negative `f64` into a saturating `u32`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn floor_u32(v: f64) -> u32 {
    if v <= 0.0 {
        0
    } else if v >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        v as u32
    }
}

/// Ceil a non-negative `f64` into a saturating `u64`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ceil_u64(v: f64) -> u64 {
    let c = v.ceil();
    if c <= 0.0 {
        0
    } else {
        c as u64
    }
}

/// Outcome of charging a bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Allowed; carries remaining whole tokens and seconds until the bucket is
    /// full again.
    Allowed {
        /// Whole tokens left after the charge.
        remaining: u32,
        /// Seconds until the bucket refills to capacity.
        reset_secs: u64,
    },
    /// Rejected; carries seconds until the requested cost is available.
    Rejected {
        /// Seconds the caller should wait before retrying.
        retry_after_secs: u64,
    },
}

/// A continuously-refilling token bucket. Capacity equals the refill rate, so
/// a bucket tolerates a one-second burst.
#[derive(Debug, Clone, Copy)]
struct TokenBucket {
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(rps: f64, now: Instant) -> Self {
        Self { tokens: rps, last: now }
    }

    /// Refill by elapsed time (capped at `rps`), then take `cost` tokens.
    /// `rps` is re-applied each call so a tier change takes effect at once.
    fn charge(&mut self, rps: f64, cost: f64, now: Instant) -> Decision {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * rps).min(rps);
        if self.tokens >= cost {
            self.tokens -= cost;
            let reset_secs = if rps > 0.0 { ceil_u64((rps - self.tokens) / rps) } else { 0 };
            Decision::Allowed { remaining: floor_u32(self.tokens), reset_secs }
        } else {
            let needed = cost - self.tokens;
            let retry = if rps > 0.0 { ceil_u64(needed / rps) } else { u64::MAX };
            Decision::Rejected { retry_after_secs: retry.max(1) }
        }
    }
}

/// Which tier a request was charged against. The label appears in the
/// `rate_limit_decision` tracing field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// A matching active CIDR override.
    CidrOverride,
    /// Admin-tier JWT (challenge-response).
    Admin,
    /// GitHub-SSO read-uplift JWT.
    ReadUplift,
    /// No usable token — limited per client IP.
    Anonymous,
}

impl Tier {
    /// Stable lowercase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CidrOverride => "cidr_override",
            Self::Admin => "admin",
            Self::ReadUplift => "read_uplift",
            Self::Anonymous => "anonymous",
        }
    }
}

/// Bucket key. Distinct namespaces never collide.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// Anonymous, keyed by client IP string.
    Ip(String),
    /// Authenticated, keyed by JWT `sub`.
    User(String),
    /// CIDR override, keyed by the matched network string.
    Cidr(String),
}

/// A parsed, active CIDR override ready for longest-prefix matching.
#[derive(Debug, Clone)]
pub struct ParsedOverride {
    /// Network address (host bits already masked off by Postgres `network()`).
    pub net: IpAddr,
    /// Prefix length in bits.
    pub prefix: u8,
    /// Requests-per-second ceiling.
    pub limit_rps: u32,
    /// When the override row was created (tie-breaker per EC-63).
    pub created_at: OffsetDateTime,
    /// Original `addr/prefix` string — used as the bucket key + header value.
    pub raw: String,
}

/// In-process limiter: token buckets keyed by [`Key`], plus a refreshable
/// cache of parsed CIDR overrides.
pub struct RateLimiter {
    buckets: Mutex<HashMap<Key, TokenBucket>>,
    overrides: RwLock<Vec<ParsedOverride>>,
    anonymous_rps: u32,
    uplift_rps: u32,
    admin_rps: u32,
}

impl RateLimiter {
    /// Build from config; `None` (pass-through) when rate limiting is disabled.
    #[must_use]
    pub fn from_config(cfg: &ServerConfig) -> Option<Arc<Self>> {
        if !cfg.rate_limit_enabled {
            return None;
        }
        Some(Arc::new(Self {
            buckets: Mutex::new(HashMap::new()),
            overrides: RwLock::new(Vec::new()),
            anonymous_rps: cfg.rate_limit_anonymous_rps,
            uplift_rps: cfg.rate_limit_uplift_rps,
            admin_rps: cfg.rate_limit_admin_rps,
        }))
    }

    /// Resolve `(bucket key, tier, limit_rps)` for a request. Order per
    /// FR-031: CIDR override → admin → read-uplift → anonymous.
    #[must_use]
    pub fn resolve(&self, client_ip: &str, auth: Option<&AuthContext>) -> (Key, Tier, u32) {
        if let Ok(ip) = client_ip.parse::<IpAddr>() {
            let guard = self.overrides.read().expect("overrides lock poisoned");
            if let Some(o) = match_override(&guard, ip) {
                return (Key::Cidr(o.raw.clone()), Tier::CidrOverride, o.limit_rps);
            }
        }
        if let Some(ctx) = auth {
            return match ctx.tier {
                mn_auth::Tier::Admin => (Key::User(ctx.sub.clone()), Tier::Admin, self.admin_rps),
                mn_auth::Tier::ReadUplift => {
                    (Key::User(ctx.sub.clone()), Tier::ReadUplift, self.uplift_rps)
                }
            };
        }
        (Key::Ip(client_ip.to_owned()), Tier::Anonymous, self.anonymous_rps)
    }

    /// Charge `cost` tokens against `key`'s bucket at the given `rps`.
    #[must_use]
    pub fn charge(&self, key: &Key, rps: u32, cost: u32) -> Decision {
        let now = Instant::now();
        let mut map = self.buckets.lock().expect("buckets lock poisoned");
        let bucket =
            map.entry(key.clone()).or_insert_with(|| TokenBucket::new(f64::from(rps), now));
        bucket.charge(f64::from(rps), f64::from(cost), now)
    }

    /// Reload the active override set from Postgres. Exposed so the refresh
    /// task and integration tests can drive it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the query fails.
    pub async fn refresh_overrides_now(&self, pool: &PgPool) -> Result<usize, StoreError> {
        let rows = rate_limit_override::list_active(pool).await?;
        let mut parsed = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(p) = parse_override(&row.cidr, row.limit_rps, row.created_at) {
                parsed.push(p);
            } else {
                tracing::warn!(cidr = %row.cidr, "skipping unparseable rate_limit_override");
            }
        }
        warn_on_overlap(&parsed);
        let n = parsed.len();
        *self.overrides.write().expect("overrides lock poisoned") = parsed;
        Ok(n)
    }

    /// Evict buckets idle longer than `idle`. Bounds memory growth.
    pub fn reap(&self, idle: Duration) {
        let now = Instant::now();
        let mut map = self.buckets.lock().expect("buckets lock poisoned");
        map.retain(|_, b| now.saturating_duration_since(b.last) < idle);
    }
}

/// True when `ip` falls within `net/prefix` (and the families match).
fn ip_in(net: IpAddr, prefix: u8, ip: IpAddr) -> bool {
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(a)) => {
            bits_match(u128::from(u32::from(n)), u128::from(u32::from(a)), prefix, 32)
        }
        (IpAddr::V6(n), IpAddr::V6(a)) => bits_match(u128::from(n), u128::from(a), prefix, 128),
        _ => false,
    }
}

fn bits_match(net: u128, addr: u128, prefix: u8, max: u8) -> bool {
    let p = prefix.min(max);
    if p == 0 {
        return true;
    }
    let shift = u32::from(max - p);
    let mask = if shift >= 128 { 0 } else { u128::MAX << shift };
    (net & mask) == (addr & mask)
}

/// Select the override that best matches `ip`: longest prefix first, ties
/// broken by the newest `created_at` (EC-63).
#[must_use]
pub fn match_override(overrides: &[ParsedOverride], ip: IpAddr) -> Option<&ParsedOverride> {
    overrides
        .iter()
        .filter(|o| ip_in(o.net, o.prefix, ip))
        .max_by(|a, b| a.prefix.cmp(&b.prefix).then_with(|| a.created_at.cmp(&b.created_at)))
}

/// Parse a stored `addr/prefix` (canonical, host bits masked) into a
/// [`ParsedOverride`]. Returns `None` if the address or prefix is unparseable
/// or `limit_rps` is non-positive.
fn parse_override(cidr: &str, limit_rps: i32, created_at: OffsetDateTime) -> Option<ParsedOverride> {
    let (net_s, prefix_s) = cidr.split_once('/').unwrap_or((cidr, ""));
    let net: IpAddr = net_s.parse().ok()?;
    let prefix = if prefix_s.is_empty() {
        if net.is_ipv4() {
            32
        } else {
            128
        }
    } else {
        prefix_s.parse().ok()?
    };
    let rps = u32::try_from(limit_rps).ok()?;
    if rps == 0 {
        return None;
    }
    Some(ParsedOverride { net, prefix, limit_rps: rps, created_at, raw: cidr.to_owned() })
}

/// Log a one-line warning per refresh if any override's network contains
/// another's network address (EC-63 visibility).
fn warn_on_overlap(overrides: &[ParsedOverride]) {
    for (i, a) in overrides.iter().enumerate() {
        for b in overrides.iter().skip(i + 1) {
            if ip_in(a.net, a.prefix, b.net) || ip_in(b.net, b.prefix, a.net) {
                tracing::warn!(a = %a.raw, b = %b.raw, "overlapping rate-limit CIDR overrides");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use mn_auth::{Role, Tier as AuthTier};

    use super::*;

    #[test]
    fn bucket_allows_until_empty_then_rejects() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(3.0, t0);
        assert!(matches!(b.charge(3.0, 1.0, t0), Decision::Allowed { .. }));
        assert!(matches!(b.charge(3.0, 1.0, t0), Decision::Allowed { .. }));
        let third = b.charge(3.0, 1.0, t0);
        assert!(matches!(third, Decision::Allowed { remaining: 0, .. }), "{third:?}");
        match b.charge(3.0, 1.0, t0) {
            Decision::Rejected { retry_after_secs } => assert!(retry_after_secs >= 1),
            d => panic!("expected rejection, got {d:?}"),
        }
    }

    #[test]
    fn bucket_refills_over_time() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(2.0, t0);
        let _ = b.charge(2.0, 2.0, t0);
        assert!(matches!(b.charge(2.0, 1.0, t0), Decision::Rejected { .. }));
        let t1 = t0 + Duration::from_secs(1);
        assert!(matches!(b.charge(2.0, 1.0, t1), Decision::Allowed { .. }));
    }

    #[test]
    fn cidr_contains_v4_and_v6() {
        assert!(ip_in("203.0.113.0".parse().unwrap(), 24, "203.0.113.5".parse().unwrap()));
        assert!(!ip_in("203.0.113.0".parse().unwrap(), 24, "203.0.114.5".parse().unwrap()));
        assert!(ip_in("10.0.0.1".parse().unwrap(), 32, "10.0.0.1".parse().unwrap()));
        assert!(!ip_in("10.0.0.1".parse().unwrap(), 32, "10.0.0.2".parse().unwrap()));
        assert!(ip_in("2001:db8::".parse().unwrap(), 32, "2001:db8::1".parse().unwrap()));
        assert!(!ip_in("2001:db8::".parse().unwrap(), 32, "2001:dba::1".parse().unwrap()));
        assert!(!ip_in("203.0.113.0".parse().unwrap(), 24, "::1".parse().unwrap()));
    }

    fn parsed(raw: &str, prefix: u8, rps: u32, secs: i64) -> ParsedOverride {
        let (net_s, _) = raw.split_once('/').unwrap();
        ParsedOverride {
            net: net_s.parse().unwrap(),
            prefix,
            limit_rps: rps,
            created_at: OffsetDateTime::from_unix_timestamp(secs).unwrap(),
            raw: raw.to_owned(),
        }
    }

    #[test]
    fn match_picks_longest_prefix_then_newest() {
        let ovs = vec![
            parsed("203.0.0.0/8", 8, 10, 100),
            parsed("203.0.113.0/24", 24, 20, 100),
            parsed("203.0.113.0/24", 24, 30, 200),
        ];
        let m = match_override(&ovs, "203.0.113.9".parse().unwrap()).unwrap();
        assert_eq!(m.prefix, 24);
        assert_eq!(m.limit_rps, 30, "tie on prefix → newest created_at wins");
        let m2 = match_override(&ovs, "203.0.5.5".parse().unwrap()).unwrap();
        assert_eq!(m2.prefix, 8);
        assert!(match_override(&ovs, "8.8.8.8".parse().unwrap()).is_none());
    }

    fn limiter() -> RateLimiter {
        RateLimiter {
            buckets: Mutex::new(HashMap::new()),
            overrides: RwLock::new(Vec::new()),
            anonymous_rps: 5,
            uplift_rps: 50,
            admin_rps: 500,
        }
    }

    fn ctx(tier: AuthTier) -> AuthContext {
        AuthContext { sub: "u1".into(), role: Role::Admin, tier, jti: "j".into() }
    }

    #[test]
    fn resolve_prefers_override_then_admin_then_uplift_then_anon() {
        let l = limiter();
        let (k, t, rps) = l.resolve("9.9.9.9", None);
        assert_eq!((t, rps), (Tier::Anonymous, 5));
        assert!(matches!(k, Key::Ip(_)));
        let (_, t, rps) = l.resolve("9.9.9.9", Some(&ctx(AuthTier::ReadUplift)));
        assert_eq!((t, rps), (Tier::ReadUplift, 50));
        let (_, t, rps) = l.resolve("9.9.9.9", Some(&ctx(AuthTier::Admin)));
        assert_eq!((t, rps), (Tier::Admin, 500));
        *l.overrides.write().unwrap() = vec![ParsedOverride {
            net: "9.9.9.0".parse().unwrap(),
            prefix: 24,
            limit_rps: 200,
            created_at: OffsetDateTime::from_unix_timestamp(1).unwrap(),
            raw: "9.9.9.0/24".into(),
        }];
        let (k, t, rps) = l.resolve("9.9.9.9", Some(&ctx(AuthTier::Admin)));
        assert_eq!((t, rps), (Tier::CidrOverride, 200));
        assert_eq!(k, Key::Cidr("9.9.9.0/24".into()));
    }

    #[test]
    fn charge_decrements_and_rejects() {
        let l = limiter();
        let key = Key::Ip("1.1.1.1".into());
        for _ in 0..5 {
            assert!(matches!(l.charge(&key, 5, 1), Decision::Allowed { .. }));
        }
        assert!(matches!(l.charge(&key, 5, 1), Decision::Rejected { .. }));
    }
}

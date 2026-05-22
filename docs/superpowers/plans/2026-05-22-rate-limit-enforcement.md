# Rate-limit Enforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce tiered (CIDR-override → SSO read-uplift → anonymous) per-request rate limits on the read API, with `X-RateLimit-*` headers on every response and a typed `429 + Retry-After` when over budget.

**Architecture:** A pure, axum-free engine (`ratelimit.rs`) holds token buckets + a refreshed cache of parsed CIDR overrides; a thin axum middleware (`middleware/rate_limit.rs`) extracts the client IP, resolves the tier, charges one token, and sets headers. The limiter is built from config (disabled by default), injected into `AppState`, and refreshed by background tasks spawned in `main.rs`.

**Tech Stack:** Rust, axum middleware (`from_fn_with_state`), `std::sync::{Mutex, RwLock}` (no new deps), `time::OffsetDateTime`, sqlx via the Phase-16 `rate_limit_override` entity.

**Conventions (every cargo command uses the pinned MSRV toolchain):**
`PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH"` prefixes every `cargo` invocation. Branch is `034-phase17-ratelimit-enforcement` (already created). Clippy is pedantic+nursery `-D warnings`; every `pub` item needs a `///`.

---

## File structure

- Create `crates/mn-server/src/ratelimit.rs` — engine: `Tier`, `Key`, `TokenBucket`, `Decision`, `ParsedOverride`, `RateLimiter`, and the CIDR-match free functions. Pure logic + `#[cfg(test)]` unit tests.
- Create `crates/mn-server/src/middleware/rate_limit.rs` — `RateLimitContext`, client-IP extraction, the axum `layer`. `#[cfg(test)]` unit tests for IP extraction.
- Modify `crates/mn-server/src/lib.rs` — add `pub mod ratelimit;`.
- Modify `crates/mn-server/src/middleware/mod.rs` — add `pub mod rate_limit;`.
- Modify `crates/mn-server/src/config.rs` — six new fields, `Default`, `from_env`.
- Modify `crates/mn-server/src/app.rs` — `AppState.rate_limiter`, `build_with_limiter`, layer wiring.
- Modify `crates/mn-server/src/main.rs` — spawn override-refresh + reaper tasks.
- Create `crates/mn-server/tests/rate_limit_enforcement.rs` — integration suite (feature `integration`).

`ErrorCode::RateLimited` already exists (→ 429), so no `mn-core` change is needed.

---

## Task 1: Token bucket + Decision

**Files:**
- Create: `crates/mn-server/src/ratelimit.rs`
- Modify: `crates/mn-server/src/lib.rs`

- [ ] **Step 1: Register the module.** Add to `crates/mn-server/src/lib.rs` next to the other `pub mod` lines:

```rust
pub mod ratelimit;
```

- [ ] **Step 2: Write the engine scaffold + the bucket with a failing test.** Create `crates/mn-server/src/ratelimit.rs`:

```rust
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_allows_until_empty_then_rejects() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(3.0, t0);
        // capacity 3 → three immediate takes succeed.
        assert!(matches!(b.charge(3.0, 1.0, t0), Decision::Allowed { .. }));
        assert!(matches!(b.charge(3.0, 1.0, t0), Decision::Allowed { .. }));
        let third = b.charge(3.0, 1.0, t0);
        assert!(matches!(third, Decision::Allowed { remaining: 0, .. }), "{third:?}");
        // fourth within the same instant is rejected with a positive retry.
        match b.charge(3.0, 1.0, t0) {
            Decision::Rejected { retry_after_secs } => assert!(retry_after_secs >= 1),
            d => panic!("expected rejection, got {d:?}"),
        }
    }

    #[test]
    fn bucket_refills_over_time() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(2.0, t0);
        let _ = b.charge(2.0, 2.0, t0); // drain
        assert!(matches!(b.charge(2.0, 1.0, t0), Decision::Rejected { .. }));
        // one second later, 2 tokens refilled.
        let t1 = t0 + Duration::from_secs(1);
        assert!(matches!(b.charge(2.0, 1.0, t1), Decision::Allowed { .. }));
    }
}
```

- [ ] **Step 3: Verify the tests fail to compile/pass, then pass.** The module won't compile yet because `RateLimiter`/`Key`/etc. are referenced by later tasks but not here — that's fine; this task only defines `TokenBucket`/`Decision`. Run:

Run: `PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo test -p mn-server ratelimit::tests::bucket -- --nocapture`
Expected: both `bucket_*` tests PASS. (Unused-import warnings for `Arc`/`HashMap`/`RwLock`/`PgPool`/etc. are expected until Task 3; they become `-D warnings` failures only at the clippy gate, which we reach after Task 3.)

- [ ] **Step 4: Commit.**

```bash
git add crates/mn-server/src/ratelimit.rs crates/mn-server/src/lib.rs
git commit -m "feat: phase-17 token bucket engine"
```

---

## Task 2: CIDR containment + longest-prefix override match

**Files:**
- Modify: `crates/mn-server/src/ratelimit.rs`

- [ ] **Step 1: Write failing tests.** Add to the `tests` module in `ratelimit.rs`:

```rust
    #[test]
    fn cidr_contains_v4_and_v6() {
        assert!(ip_in("203.0.113.0/24".parse().unwrap(), 24, "203.0.113.5".parse().unwrap()));
        assert!(!ip_in("203.0.113.0/24".parse().unwrap(), 24, "203.0.114.5".parse().unwrap()));
        assert!(ip_in("10.0.0.1/32".parse().unwrap(), 32, "10.0.0.1".parse().unwrap()));
        assert!(!ip_in("10.0.0.1/32".parse().unwrap(), 32, "10.0.0.2".parse().unwrap()));
        assert!(ip_in("2001:db8::/32".parse().unwrap(), 32, "2001:db8::1".parse().unwrap()));
        assert!(!ip_in("2001:db8::/32".parse().unwrap(), 32, "2001:dba::1".parse().unwrap()));
        // mismatched families never match.
        assert!(!ip_in("203.0.113.0/24".parse().unwrap(), 24, "::1".parse().unwrap()));
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
            parsed("203.0.113.0/24", 24, 30, 200), // newer, same prefix
        ];
        let m = match_override(&ovs, "203.0.113.9".parse().unwrap()).unwrap();
        assert_eq!(m.prefix, 24);
        assert_eq!(m.limit_rps, 30, "tie on prefix → newest created_at wins");
        // an IP only inside the /8 falls back to it.
        let m2 = match_override(&ovs, "203.0.5.5".parse().unwrap()).unwrap();
        assert_eq!(m2.prefix, 8);
        // no match → None.
        assert!(match_override(&ovs, "8.8.8.8".parse().unwrap()).is_none());
    }
```

- [ ] **Step 2: Implement.** Add to `ratelimit.rs` (before the `tests` module):

```rust
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
```

- [ ] **Step 3: Run the tests.**

Run: `PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo test -p mn-server ratelimit::tests -- --nocapture`
Expected: `cidr_contains_v4_and_v6` and `match_picks_longest_prefix_then_newest` PASS.

- [ ] **Step 4: Commit.**

```bash
git add crates/mn-server/src/ratelimit.rs
git commit -m "feat: phase-17 cidr containment + longest-prefix override match"
```

---

## Task 3: RateLimiter (resolve / charge / refresh / reap / from_config)

**Files:**
- Modify: `crates/mn-server/src/ratelimit.rs`

- [ ] **Step 1: Write failing tests.** Add to the `tests` module:

```rust
    use crate::middleware::bearer::AuthContext;
    use mn_auth::{Role, Tier as AuthTier};

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
        // anonymous when no auth + no override.
        let (k, t, rps) = l.resolve("9.9.9.9", None);
        assert_eq!((t, rps), (Tier::Anonymous, 5));
        assert!(matches!(k, Key::Ip(_)));
        // read-uplift token.
        let (_, t, rps) = l.resolve("9.9.9.9", Some(&ctx(AuthTier::ReadUplift)));
        assert_eq!((t, rps), (Tier::ReadUplift, 50));
        // admin token.
        let (_, t, rps) = l.resolve("9.9.9.9", Some(&ctx(AuthTier::Admin)));
        assert_eq!((t, rps), (Tier::Admin, 500));
        // a matching override beats even an admin token.
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
        // anonymous_rps = 5 → five allowed, sixth rejected (same instant).
        for _ in 0..5 {
            assert!(matches!(l.charge(&key, 5, 1), Decision::Allowed { .. }));
        }
        assert!(matches!(l.charge(&key, 5, 1), Decision::Rejected { .. }));
    }
```

- [ ] **Step 2: Implement.** Add to `ratelimit.rs` (before `tests`):

```rust
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
        let bucket = map
            .entry(key.clone())
            .or_insert_with(|| TokenBucket::new(f64::from(rps), now));
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

/// Parse a stored `addr/prefix` (canonical, host bits masked) into a
/// [`ParsedOverride`]. Returns `None` if the address or prefix is unparseable
/// or `limit_rps` is non-positive.
fn parse_override(cidr: &str, limit_rps: i32, created_at: OffsetDateTime) -> Option<ParsedOverride> {
    let (net_s, prefix_s) = cidr.split_once('/').unwrap_or((cidr, ""));
    let net: IpAddr = net_s.parse().ok()?;
    let prefix = if prefix_s.is_empty() {
        if net.is_ipv4() { 32 } else { 128 }
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
```

- [ ] **Step 3: Run the tests.**

Run: `PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo test -p mn-server ratelimit::tests -- --nocapture`
Expected: all `ratelimit::tests` PASS (`resolve_*`, `charge_*`, plus Task 1/2 tests).

- [ ] **Step 4: Commit.**

```bash
git add crates/mn-server/src/ratelimit.rs
git commit -m "feat: phase-17 RateLimiter resolve/charge/refresh/reap"
```

---

## Task 4: Config fields

**Files:**
- Modify: `crates/mn-server/src/config.rs`

- [ ] **Step 1: Add the struct fields.** In `crates/mn-server/src/config.rs`, add to `pub struct ServerConfig` (after `abort_grace_hours`):

```rust
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_ENABLED` — master switch. Default `false`
    /// so `Default::default()` (used by tests) never throttles; production
    /// opts in.
    pub rate_limit_enabled: bool,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_ANONYMOUS_RPS` — per-IP requests/sec.
    pub rate_limit_anonymous_rps: u32,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_UPLIFT_RPS` — per-user requests/sec for
    /// GitHub-SSO read-uplift tokens.
    pub rate_limit_uplift_rps: u32,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_ADMIN_RPS` — per-user requests/sec for
    /// admin-tier tokens.
    pub rate_limit_admin_rps: u32,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_CLIENT_IP_HEADER` — header carrying the real
    /// client IP behind the proxy.
    pub rate_limit_client_ip_header: String,
    /// `MIDNIGHT_MANUAL_RATE_LIMIT_OVERRIDE_REFRESH_SECS` — override-cache
    /// refresh interval.
    pub rate_limit_override_refresh_secs: u64,
```

- [ ] **Step 2: Add the `Default` values.** In `impl Default for ServerConfig`'s returned struct literal, add:

```rust
            rate_limit_enabled: false,
            rate_limit_anonymous_rps: 10,
            rate_limit_uplift_rps: 60,
            rate_limit_admin_rps: 1000,
            rate_limit_client_ip_header: "fly-client-ip".to_owned(),
            rate_limit_override_refresh_secs: 30,
```

- [ ] **Step 3: Parse in `from_env`.** In `fn from_env`, before the final struct construction, add (follow the existing `unwrap_or`/`parse` style already used for `embedder_*`):

```rust
        let rate_limit_enabled = env::var("MIDNIGHT_MANUAL_RATE_LIMIT_ENABLED")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false);
        let rate_limit_anonymous_rps = env::var("MIDNIGHT_MANUAL_RATE_LIMIT_ANONYMOUS_RPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);
        let rate_limit_uplift_rps = env::var("MIDNIGHT_MANUAL_RATE_LIMIT_UPLIFT_RPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        let rate_limit_admin_rps = env::var("MIDNIGHT_MANUAL_RATE_LIMIT_ADMIN_RPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);
        let rate_limit_client_ip_header = env::var("MIDNIGHT_MANUAL_RATE_LIMIT_CLIENT_IP_HEADER")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "fly-client-ip".to_owned());
        let rate_limit_override_refresh_secs =
            env::var("MIDNIGHT_MANUAL_RATE_LIMIT_OVERRIDE_REFRESH_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30);
```

Then add these six fields to the struct literal `from_env` returns (matching field names).

- [ ] **Step 4: Add a test.** In the `#[cfg(test)] mod tests` of `config.rs` add:

```rust
    #[test]
    fn rate_limit_defaults_are_disabled() {
        let c = ServerConfig::default();
        assert!(!c.rate_limit_enabled);
        assert_eq!(c.rate_limit_anonymous_rps, 10);
        assert_eq!(c.rate_limit_client_ip_header, "fly-client-ip");
    }
```

- [ ] **Step 5: Run + commit.**

Run: `PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo test -p mn-server config:: -- --nocapture`
Expected: `rate_limit_defaults_are_disabled` PASS.

```bash
git add crates/mn-server/src/config.rs
git commit -m "feat: phase-17 rate-limit config fields"
```

---

## Task 5: Middleware (client-IP extraction + layer)

**Files:**
- Create: `crates/mn-server/src/middleware/rate_limit.rs`
- Modify: `crates/mn-server/src/middleware/mod.rs`

- [ ] **Step 1: Register the module.** Add to `crates/mn-server/src/middleware/mod.rs`:

```rust
pub mod rate_limit;
```

- [ ] **Step 2: Create the middleware with a failing IP-extraction test.** Create `crates/mn-server/src/middleware/rate_limit.rs`:

```rust
//! Rate-limit middleware (Phase 17). Resolves the caller's tier, charges one
//! token against the in-process [`RateLimiter`], sets `X-RateLimit-*` headers,
//! and returns `429` with `Retry-After` when the bucket is empty. A no-op when
//! the limiter is absent (rate limiting disabled).

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
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
    req: Request<axum::body::Body>,
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
                .message(format!("rate limit exceeded for the {} tier ({limit} req/s)", tier.as_str()))
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
        Decision::Allowed { remaining, reset_secs } => {
            tracing::info!(
                request_id = req_id.as_str(),
                rate_limit_decision = "allowed",
                tier = tier.as_str(),
                "rate limit ok"
            );
            let mut req = req;
            req.extensions_mut().insert(RateLimitContext { key, tier, limit });
            let mut resp = next.run(req).await;
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
```

- [ ] **Step 3: Run the unit tests.** (Will not fully compile until Task 6 adds `AppState.rate_limiter`; if so, do Task 6 first, then return here. The pure functions are what we assert.)

Run: `PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo test -p mn-server middleware::rate_limit -- --nocapture`
Expected: `client_ip_*` and `exempt_paths` PASS.

- [ ] **Step 4: Commit.**

```bash
git add crates/mn-server/src/middleware/rate_limit.rs crates/mn-server/src/middleware/mod.rs
git commit -m "feat: phase-17 rate-limit middleware + client-ip extraction"
```

---

## Task 6: Wire into AppState + app builder

**Files:**
- Modify: `crates/mn-server/src/app.rs`

- [ ] **Step 1: Add the field to `AppState`.** In `crates/mn-server/src/app.rs`, add to `pub struct AppState`:

```rust
    /// In-process rate limiter, or `None` when rate limiting is disabled.
    pub rate_limiter: Option<std::sync::Arc<crate::ratelimit::RateLimiter>>,
```

- [ ] **Step 2: Split `build` so a limiter can be injected.** Replace the existing `pub fn build(...)` signature/body start so that `build` delegates to a new `build_with_limiter`. Keep the entire route/layer body inside `build_with_limiter`:

```rust
/// Build the app, constructing the rate limiter from `cfg`.
///
/// # Errors
///
/// Returns [`AuthStateError`] if auth env values are present but malformed.
pub fn build(pool: PgPool, cfg: ServerConfig) -> Result<Router, AuthStateError> {
    let limiter = crate::ratelimit::RateLimiter::from_config(&cfg);
    build_with_limiter(pool, cfg, limiter)
}

/// Build the app with an explicit rate limiter (used by `main` so background
/// tasks share the instance, and by integration tests so they can seed
/// overrides).
///
/// # Errors
///
/// Returns [`AuthStateError`] if auth env values are present but malformed.
pub fn build_with_limiter(
    pool: PgPool,
    cfg: ServerConfig,
    rate_limiter: Option<std::sync::Arc<crate::ratelimit::RateLimiter>>,
) -> Result<Router, AuthStateError> {
    let auth = AuthState::from_config(&cfg)?.map(Arc::new);
    let state = AppState { pool, cfg: Arc::new(cfg), auth, rate_limiter };
    // ... existing Router::new()....with_state(state) body unchanged except the
    // added rate-limit layer in Step 3 ...
}
```

(Move the existing `let auth = ...; let state = ...; Ok(Router::new()...)` body verbatim into `build_with_limiter`, adding `rate_limiter` to the `AppState { .. }` literal.)

- [ ] **Step 3: Add the layer.** In the layer stack inside `build_with_limiter`, insert the rate-limit layer so it runs *after* `bearer` and `request_id` on the request path. Place it immediately before the `bearer` layer line:

```rust
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::rate_limit::layer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::bearer::layer,
        ))
```

(axum applies the last-added layer outermost, so `request_id` and `bearer` — added after — run before `rate_limit`, giving it the `RequestId` and `AuthContext` extensions.)

- [ ] **Step 4: Fix the other `AppState { .. }` construction sites.** Search and update any remaining literal:

Run: `PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo build -p mn-server 2>&1 | tail -20`
Expected: if any `AppState { ... }` literal is missing `rate_limiter`, the compiler names it; add `rate_limiter: None` there. Re-run until it builds.

- [ ] **Step 5: Commit.**

```bash
git add crates/mn-server/src/app.rs
git commit -m "feat: phase-17 wire rate limiter into AppState + app builder"
```

---

## Task 7: Spawn refresh + reaper tasks in main

**Files:**
- Modify: `crates/mn-server/src/main.rs`

- [ ] **Step 1: Build the limiter and inject it.** In `main.rs`, where `app::build(pool.clone(), cfg)` (or similar) is currently called, replace with an explicit limiter and use `build_with_limiter`. Add near the app construction:

```rust
    let rate_limiter = mn_server::ratelimit::RateLimiter::from_config(&cfg);
    let app = mn_server::app::build_with_limiter(pool.clone(), cfg.clone(), rate_limiter.clone())
        .expect("build app");
```

(`cfg` must be `Clone` and cloned before being moved into `build_with_limiter`; it already derives `Clone` — confirm and add `#[derive(Clone)]` to `ServerConfig` only if missing.)

- [ ] **Step 2: Spawn the background tasks.** After the app is built and before `axum::serve`, add:

```rust
    if let Some(limiter) = rate_limiter.clone() {
        // Initial load so overrides are effective immediately.
        if let Err(e) = limiter.refresh_overrides_now(&pool).await {
            tracing::warn!(error = %e, "initial rate-limit override load failed");
        }
        let refresh_pool = pool.clone();
        let refresh_secs = cfg.rate_limit_override_refresh_secs;
        let refresh_limiter = limiter.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(refresh_secs.max(1)));
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                if let Err(e) = refresh_limiter.refresh_overrides_now(&refresh_pool).await {
                    tracing::warn!(error = %e, "rate-limit override refresh failed");
                }
            }
        });
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tick.tick().await;
                limiter.reap(std::time::Duration::from_secs(300));
            }
        });
    }
```

- [ ] **Step 3: Build the binary.**

Run: `PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo build -p mn-server --bin midnight-manual-server 2>&1 | tail -20`
Expected: builds clean. (Fix any `cfg` move/borrow errors by cloning `cfg` fields before `cfg` is moved.)

- [ ] **Step 4: Commit.**

```bash
git add crates/mn-server/src/main.rs
git commit -m "feat: phase-17 spawn override-refresh + bucket reaper tasks"
```

---

## Task 8: Integration tests

**Files:**
- Create: `crates/mn-server/tests/rate_limit_enforcement.rs`

- [ ] **Step 1: Write the integration suite.** Create `crates/mn-server/tests/rate_limit_enforcement.rs`. It builds an app with an explicit limiter (rate limiting enabled), drives requests with a controlled `Fly-Client-IP`, and asserts headers / 429 / tiers. It reuses the auth helpers from the `admin_ratelimits_crud.rs` pattern.

```rust
//! End-to-end exercises for rate-limit enforcement (Phase 17).

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use mn_auth::Keypair;
use mn_server::config::ServerConfig;
use mn_server::ratelimit::RateLimiter;
use mn_server::app;
use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;

fn enabled_cfg(anonymous_rps: u32) -> ServerConfig {
    ServerConfig {
        corpus_model: Some("bge-base-en-v1.5@1".to_owned()),
        rate_limit_enabled: true,
        rate_limit_anonymous_rps: anonymous_rps,
        rate_limit_uplift_rps: 1000,
        rate_limit_admin_rps: 1000,
        ..Default::default()
    }
}

async fn call_ip(app: axum::Router, uri: &str, ip: &str) -> (StatusCode, axum::http::HeaderMap) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("fly-client-ip", ip)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    (resp.status(), resp.headers().clone())
}

#[tokio::test]
async fn success_carries_ratelimit_headers() {
    let h = common::boot().await;
    let limiter = RateLimiter::from_config(&enabled_cfg(100));
    let app = app::build_with_limiter(h.pool.clone(), enabled_cfg(100), limiter).expect("build");
    let (status, headers) = call_ip(app, "/v1/sources", &format!("{}", unique_ip())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.contains_key("x-ratelimit-limit"), "limit header present");
    assert!(headers.contains_key("x-ratelimit-remaining"), "remaining header present");
    assert!(headers.contains_key("x-ratelimit-reset"), "reset header present");
}

#[tokio::test]
async fn anonymous_over_budget_returns_429_with_retry_after() {
    let h = common::boot().await;
    let limiter = RateLimiter::from_config(&enabled_cfg(2));
    let app = app::build_with_limiter(h.pool.clone(), enabled_cfg(2), limiter).expect("build");
    let ip = format!("{}", unique_ip());
    // capacity 2 → first two OK, third 429.
    let (s1, _) = call_ip(app.clone(), "/v1/sources", &ip).await;
    let (s2, _) = call_ip(app.clone(), "/v1/sources", &ip).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    let req = Request::builder()
        .method("GET")
        .uri("/v1/sources")
        .header("fly-client-ip", &ip)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(resp.headers().contains_key("retry-after"));
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "rate_limited");
}

#[tokio::test]
async fn health_and_metrics_are_exempt() {
    let h = common::boot().await;
    let limiter = RateLimiter::from_config(&enabled_cfg(1));
    let app = app::build_with_limiter(h.pool.clone(), enabled_cfg(1), limiter).expect("build");
    let ip = "5.5.5.5";
    // Many requests to /healthz must never 429.
    for _ in 0..5 {
        let (s, _) = call_ip(app.clone(), "/healthz", ip).await;
        assert_ne!(s, StatusCode::TOO_MANY_REQUESTS);
    }
}

#[tokio::test]
async fn cidr_override_raises_the_limit() {
    let h = common::boot().await;
    let cfg = enabled_cfg(1); // anon floor = 1 rps
    let limiter = RateLimiter::from_config(&cfg).expect("enabled");
    // Seed an override for a /24 that contains our test IP, then refresh.
    let net = format!("203.0.{}.0/24", rand_octet());
    let ip = net.replace(".0/24", ".7");
    mn_store::entities::rate_limit_override::insert(
        &h.pool,
        &net,
        50,
        OffsetDateTime::now_utc() + Duration::hours(1),
        Some(&format!("test-{}", Uuid::new_v4())),
        "rl-test",
    )
    .await
    .expect("seed override");
    limiter.refresh_overrides_now(&h.pool).await.expect("refresh");

    let app = app::build_with_limiter(h.pool.clone(), cfg, Some(Arc::clone(&limiter))).expect("build");
    // With the /24 override (50 rps) the third request still succeeds, whereas
    // the anon floor of 1 would have 429'd it.
    for _ in 0..3 {
        let (s, headers) = call_ip(app.clone(), "/v1/sources", &ip).await;
        assert_eq!(s, StatusCode::OK, "override should permit the request");
        assert_eq!(headers.get("x-ratelimit-limit").unwrap(), "50");
    }
}

// ---- helpers ----

fn unique_ip() -> std::net::Ipv4Addr {
    // Random TEST-NET-ish address so concurrent tests use distinct buckets.
    std::net::Ipv4Addr::new(198, 51, rand_octet(), rand_octet())
}

fn rand_octet() -> u8 {
    // Cheap entropy from a UUID; avoids a rand dependency.
    Uuid::new_v4().as_bytes()[0]
}
```

- [ ] **Step 2: Run the integration suite.**

Run: `PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo test -p mn-server --features integration --test rate_limit_enforcement -- --nocapture`
Expected: all four tests PASS. (If `/v1/sources` requires a corpus model and 503s, switch the success/override probes to a route that returns 200 without data — confirm `/v1/sources` returns `200 []` on an empty corpus first; the Phase-16 tests already rely on `GET /v1/sources` returning 200.)

- [ ] **Step 3: Add the read-uplift + admin tier tests.** Append two tests that mint tokens (copy `mint_token`, `admin_user_store`, `cfg_with_auth` helpers from `crates/mn-server/tests/admin_ratelimits_crud.rs` into this file, merging with `enabled_cfg` so the config both enables rate limiting AND sets the user store + jwt secret). Assert that an uplift/admin token's `x-ratelimit-limit` reflects the higher tier (`1000`) and that requests above the anon floor still succeed. Use the same `unique_ip()` so the IP path can't override the token tier.

```rust
// (Add `user_store_body` + `jwt_secret` to enabled_cfg via a second helper
// `enabled_auth_cfg(...)`, then assert headers["x-ratelimit-limit"] == "1000"
// for a minted admin token, sending the bearer via the Authorization header.)
```

- [ ] **Step 4: Run the full file + commit.**

Run: `PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo test -p mn-server --features integration --test rate_limit_enforcement -- --nocapture`
Expected: all tests PASS.

```bash
git add crates/mn-server/tests/rate_limit_enforcement.rs
git commit -m "test: phase-17 rate-limit enforcement integration suite"
```

---

## Task 9: Full gates + PR

- [ ] **Step 1: Run every gate under the MSRV toolchain.**

```bash
PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo fmt --all -- --check
PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo clippy --workspace --all-targets --all-features -- -D warnings
PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo test --workspace --no-fail-fast
PATH="$HOME/.rustup/toolchains/1.91.0-aarch64-apple-darwin/bin:$PATH" cargo test --workspace --features integration --no-fail-fast
```

Expected: fmt clean, clippy clean, doc clean, all tests pass. Fix any issue before proceeding. If `source_version_retention` flakes (teardown exit 101 with all tests `ok`), re-run that one binary in isolation to confirm it's the known shared-Postgres flake.

- [ ] **Step 2: Commit any gate fixes, push, open the PR.**

```bash
git push -u origin 034-phase17-ratelimit-enforcement
gh pr create --title "feat: phase-17 rate-limit enforcement middleware" --body "<verification checklist of the five gates above + summary of the engine/middleware/config/wiring/tests>"
```

- [ ] **Step 3: Monitor CI, then squash-merge.** Use the CI monitor recipe (leading `sleep 30`). Once all checks are green:

```bash
gh pr merge <pr> --squash --delete-branch && git checkout main && git pull --ff-only
```

---

## Self-review (completed by the plan author)

**Spec coverage:** FR-029 headers → Task 5 + Task 8/Step 1; FR-031 tier order → Task 3 `resolve` + Task 8; D11 tiers → Task 3; acceptance #8 (429 + Retry-After + body) → Task 5 + Task 8/Step 2; #9 (uplift higher tier) → Task 8/Step 3; #10 (CIDR override) → Task 8/Step 1 (`cidr_override` test); EC-62 (decision once at start) → Task 5 (resolve before `next.run`); EC-63 (longest prefix + newest tie + warn) → Task 2 + Task 3 `warn_on_overlap`; FR-034 (`rate_limit_decision`, no token logging) → Task 5 tracing. Multi-query D25 explicitly deferred with the `RateLimitContext` hook (Task 5).

**Placeholder scan:** none — all code blocks are concrete. Task 8/Step 3 is the one prose-described test (the others give full code); its helpers are named and sourced from an existing file.

**Type consistency:** `RateLimiter::{from_config, resolve, charge, refresh_overrides_now, reap}`, `Decision::{Allowed{remaining,reset_secs}, Rejected{retry_after_secs}}`, `Key::{Ip,User,Cidr}`, `Tier::{CidrOverride,Admin,ReadUplift,Anonymous}`, `RateLimitContext{key,tier,limit}`, and `app::build_with_limiter(pool, cfg, Option<Arc<RateLimiter>>)` are used consistently across tasks.

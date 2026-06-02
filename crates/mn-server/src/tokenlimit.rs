//! In-memory, rolling-window embedding token accounting.
//! Hourly = rolling 60 min via per-minute buckets; daily = rolling 24h via
//! per-hour buckets. Both checked in-memory (no DB on the hot path). A later
//! task adds DB-backed overrides + a restart snapshot.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, RwLock};

/// Effective per-window token ceilings for a subject.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Maximum tokens allowed within any rolling 60-minute window.
    pub hourly: u64,
    /// Maximum tokens allowed within any rolling 24-hour window.
    pub daily: u64,
}

/// The accounting key: an anonymous IP or an authenticated user id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TokenSubject {
    /// Unauthenticated request identified by client IP address string.
    Ip(String),
    /// Authenticated request identified by JWT `sub`.
    User(String),
}

/// Which rolling window a rejection or window-info refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// Rolling 60-minute (hourly) window.
    Hour,
    /// Rolling 24-hour (daily) window.
    Day,
}

/// Per-window limit/remaining/reset snapshot.
#[derive(Debug, Clone, Copy)]
pub struct WindowInfo {
    /// Configured token ceiling for this window.
    pub limit: u64,
    /// Tokens remaining before the window is exhausted.
    pub remaining: u64,
    /// Unix timestamp (seconds) when the oldest bucket in this window expires.
    pub reset_at_secs: i64,
}

/// Both windows' info for a subject.
#[derive(Debug, Clone, Copy)]
pub struct RateInfo {
    /// Hourly window snapshot.
    pub hour: WindowInfo,
    /// Daily window snapshot.
    pub day: WindowInfo,
}

/// A rejection: which window tripped, its ceiling, and when it resets.
#[derive(Debug, Clone, Copy)]
pub struct Reject {
    /// The window whose ceiling was exceeded.
    pub window: Window,
    /// The ceiling that was exceeded.
    pub limit: u64,
    /// Unix timestamp (seconds) when the oldest bucket in the window expires,
    /// allowing a retry.
    pub reset_at_secs: i64,
}

#[derive(Default)]
struct SubjectUsage {
    minutes: BTreeMap<i64, u64>, // unix-minute -> tokens
    hours: BTreeMap<i64, u64>,   // unix-hour   -> tokens
    last_seen_secs: i64,
}

impl SubjectUsage {
    fn prune(&mut self, now: i64) {
        let min_floor = now / 60 - 59; // keep last 60 minutes
        let hr_floor = now / 3600 - 23; // keep last 24 hours
        self.minutes.retain(|&m, _| m >= min_floor);
        self.hours.retain(|&hh, _| hh >= hr_floor);
    }

    fn hour_used(&self, now: i64) -> u64 {
        let floor = now / 60 - 59;
        self.minutes.range(floor..).map(|(_, v)| *v).sum()
    }

    fn day_used(&self, now: i64) -> u64 {
        let floor = now / 3600 - 23;
        self.hours.range(floor..).map(|(_, v)| *v).sum()
    }

    fn hour_reset(&self, now: i64) -> i64 {
        let floor = now / 60 - 59;
        self.minutes
            .range(floor..)
            .next()
            .map_or(now, |(&m, _)| (m + 60) * 60)
    }

    fn day_reset(&self, now: i64) -> i64 {
        let floor = now / 3600 - 23;
        self.hours
            .range(floor..)
            .next()
            .map_or(now, |(&hh, _)| (hh + 24) * 3600)
    }
}

/// In-memory rolling-window token limiter. Tier limits are stored for the
/// subject/tier resolver added in Task 4.5.
///
/// # Concurrency
///
/// `check` and `charge` are intentionally NOT atomic across calls. Two requests
/// that both pass `check` concurrently can collectively exceed a window ceiling
/// by at most `N * estimate` (N = concurrency). This is an accepted trade-off —
/// token accounting is soft-limited, not hard-enforced — so do not "fix" it by
/// holding the lock across both calls (that would serialize every embedding
/// request). `snapshot_for` over-charged subjects report `remaining == 0`
/// (saturating), never a wrapped value.
pub struct TokenUsageLimiter {
    // TODO(Task 4.8): a snapshot/reaper job evicts idle subjects. Until then a
    // subject key persists once seen (its buckets are pruned, the key is not).
    usage: Mutex<HashMap<TokenSubject, SubjectUsage>>,
    anon: Limits,
    uplift: Limits,
    admin: Limits,
    overrides: RwLock<Vec<crate::tokenlimit_override::Parsed>>,
}

impl TokenUsageLimiter {
    /// Construct an `Arc`-wrapped limiter from server config, mirroring
    /// [`RateLimiter::from_config`](crate::ratelimit::RateLimiter::from_config).
    /// Unlike the rate limiter, token accounting is always on (there is no
    /// disable switch), so this returns the limiter unconditionally.
    #[must_use]
    pub fn from_config(cfg: &crate::config::ServerConfig) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::new(
            Limits {
                hourly: cfg.token_limit_anon_hourly,
                daily: cfg.token_limit_anon_daily,
            },
            Limits {
                hourly: cfg.token_limit_uplift_hourly,
                daily: cfg.token_limit_uplift_daily,
            },
            Limits {
                hourly: cfg.token_limit_admin_hourly,
                daily: cfg.token_limit_admin_daily,
            },
        ))
    }

    /// Construct a limiter with the three tier default ceilings.
    /// The tier limits are used by the subject resolver added in Task 4.5.
    #[must_use]
    pub fn new(anon: Limits, uplift: Limits, admin: Limits) -> Self {
        Self {
            usage: Mutex::new(HashMap::new()),
            anon,
            uplift,
            admin,
            overrides: RwLock::new(Vec::new()),
        }
    }

    /// Returns `Ok` if `estimate` more tokens fit within both windows; else
    /// returns [`Reject`] identifying the window that would be exceeded.
    ///
    /// # Errors
    ///
    /// Returns [`Reject`] when the hourly or daily ceiling would be breached.
    pub fn check(
        &self,
        subject: &TokenSubject,
        limits: Limits,
        estimate: u64,
        now: i64,
    ) -> Result<(), Reject> {
        let mut map = self.usage.lock().expect("usage lock");
        let u = map.entry(subject.clone()).or_default();
        u.prune(now);
        let hour_used = u.hour_used(now);
        let day_used = u.day_used(now);
        let hour_reset = u.hour_reset(now);
        let day_reset = u.day_reset(now);
        drop(map);
        // saturating_add so an adversarial/huge `estimate` rejects (saturates to
        // u64::MAX > limit) rather than wrapping past the ceiling in release builds.
        if hour_used.saturating_add(estimate) > limits.hourly {
            return Err(Reject {
                window: Window::Hour,
                limit: limits.hourly,
                reset_at_secs: hour_reset,
            });
        }
        if day_used.saturating_add(estimate) > limits.daily {
            return Err(Reject {
                window: Window::Day,
                limit: limits.daily,
                reset_at_secs: day_reset,
            });
        }
        Ok(())
    }

    /// Debit `tokens` from `subject`'s per-minute and per-hour buckets at
    /// `now`. Call this after a successful embedding request.
    pub fn charge(&self, subject: &TokenSubject, tokens: u64, now: i64) {
        let mut map = self.usage.lock().expect("usage lock");
        let u = map.entry(subject.clone()).or_default();
        u.prune(now);
        // saturating_add so a corrupt/huge `tokens` can't wrap a bucket sum
        // (which would silently under-count usage and let requests slip past).
        let minute = u.minutes.entry(now / 60).or_default();
        *minute = minute.saturating_add(tokens);
        let hour = u.hours.entry(now / 3600).or_default();
        *hour = hour.saturating_add(tokens);
        u.last_seen_secs = now;
        drop(map);
    }

    /// Return a consistent snapshot of both windows for `subject` against
    /// the provided `limits`.
    #[must_use]
    pub fn snapshot_for(&self, subject: &TokenSubject, limits: Limits, now: i64) -> RateInfo {
        // Read-only: a `snapshot_for` (e.g. populating rate headers) must NOT
        // create a usage entry for an unseen subject. The window floors in
        // `hour_used`/`day_used` already exclude stale buckets, so we don't need
        // to prune on this read path.
        let map = self.usage.lock().expect("usage lock");
        let (hu, du, hour_reset, day_reset) = map.get(subject).map_or((0, 0, now, now), |u| {
            (u.hour_used(now), u.day_used(now), u.hour_reset(now), u.day_reset(now))
        });
        drop(map);
        RateInfo {
            hour: WindowInfo {
                limit: limits.hourly,
                remaining: limits.hourly.saturating_sub(hu),
                reset_at_secs: hour_reset,
            },
            day: WindowInfo {
                limit: limits.daily,
                remaining: limits.daily.saturating_sub(du),
                reset_at_secs: day_reset,
            },
        }
    }
}

/// Which tier resolved for a request (for telemetry / response labelling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenTier {
    /// Unauthenticated request, limited by client IP.
    Anonymous,
    /// GitHub-SSO read-uplift JWT.
    ReadUplift,
    /// Admin / writer JWT minted via Ed25519 challenge-response.
    Admin,
}

impl TokenUsageLimiter {
    /// Resolve the subject, tier, and effective limits for a request.
    ///
    /// An override beats the tier default, but the two override kinds apply on
    /// *different* paths and never cross (design §7.3): an authenticated request
    /// consults only the exact user-id override — a CIDR override NEVER applies
    /// to a JWT holder, so a user's tier default is their ceiling regardless of
    /// network location. An anonymous request consults only the longest-prefix
    /// CIDR override. (This is a deliberate asymmetry vs `ratelimit::resolve`,
    /// which consults CIDR before auth — do not "align" them.)
    pub fn resolve(
        &self,
        client_ip: &str,
        auth: Option<&crate::middleware::bearer::AuthContext>,
    ) -> (TokenSubject, TokenTier, Limits) {
        if let Some(ctx) = auth {
            let (subject, tier, base) = match ctx.tier {
                mn_auth::Tier::Admin => {
                    (TokenSubject::User(ctx.sub.clone()), TokenTier::Admin, self.admin)
                }
                mn_auth::Tier::ReadUplift => {
                    (TokenSubject::User(ctx.sub.clone()), TokenTier::ReadUplift, self.uplift)
                }
            };
            // Read the guard, extract the result, then drop it before returning.
            let user_override = {
                let ov = self.overrides.read().expect("ov lock");
                crate::tokenlimit_override::match_user(&ov, &ctx.sub)
            };
            let limits = user_override.map_or(base, |(h, d)| Limits { hourly: h, daily: d });
            return (subject, tier, limits);
        }
        if let Ok(ip) = client_ip.parse() {
            // Read the guard, extract the result, then drop it before the branch.
            let cidr_override = {
                let ov = self.overrides.read().expect("ov lock");
                crate::tokenlimit_override::match_cidr(&ov, ip)
            };
            if let Some((h, d)) = cidr_override {
                return (
                    TokenSubject::Ip(client_ip.to_owned()),
                    TokenTier::Anonymous,
                    Limits { hourly: h, daily: d },
                );
            }
        }
        (TokenSubject::Ip(client_ip.to_owned()), TokenTier::Anonymous, self.anon)
    }

    /// Reload the override cache from the DB (active rows only).
    ///
    /// # Errors
    /// Propagates any store error from `list_active`.
    pub async fn refresh_overrides_now(
        &self,
        pool: &sqlx::PgPool,
    ) -> Result<usize, mn_store::error::StoreError> {
        let rows = mn_store::entities::token_limit_override::list_active(pool).await?;
        let parsed: Vec<_> = rows
            .into_iter()
            .filter_map(|row| {
                let id = row.id;
                crate::tokenlimit_override::parse_row(row).or_else(|| {
                    // A silently-dropped override would quietly change spend
                    // limits — surface it (mirrors ratelimit's refresh).
                    tracing::warn!(override_id = %id, "skipping unparseable token_limit_override row");
                    None
                })
            })
            .collect();
        let n = parsed.len();
        *self.overrides.write().expect("ov lock") = parsed;
        Ok(n)
    }

    /// Replace the in-memory override cache. Only available in test builds.
    #[cfg(test)]
    pub fn set_overrides_for_test(&self, v: Vec<crate::tokenlimit_override::Parsed>) {
        *self.overrides.write().expect("ov lock") = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d() -> Limits {
        Limits { hourly: 100, daily: 1000 }
    }
    fn lim() -> Limits {
        Limits { hourly: 100, daily: 1000 }
    }
    fn subj() -> TokenSubject {
        TokenSubject::User("u1".into())
    }

    #[test]
    fn charges_and_reports_remaining() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        l.charge(&subj(), 30, now);
        let info = l.snapshot_for(&subj(), lim(), now);
        assert_eq!(info.hour.remaining, 70);
        assert_eq!(info.day.remaining, 970);
    }

    #[test]
    fn rejects_when_estimate_exceeds_hour() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        l.charge(&subj(), 90, now);
        let rej = l.check(&subj(), lim(), 20, now);
        assert!(matches!(rej, Err(Reject { window: Window::Hour, .. })));
    }

    #[test]
    fn hourly_usage_ages_out_after_60_minutes() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let t0 = 1_000_000_000;
        l.charge(&subj(), 90, t0);
        // 61 minutes later: minute buckets aged out of hour window
        let later = t0 + 61 * 60;
        assert!(l.check(&subj(), lim(), 90, later).is_ok());
        // but daily (hour buckets, 24h) still counts it
        let info = l.snapshot_for(&subj(), lim(), later);
        assert_eq!(info.day.remaining, 1000 - 90);
    }

    #[test]
    fn daily_usage_ages_out_after_24_hours() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let t0 = 1_000_000_000;
        l.charge(&subj(), 500, t0);
        let later = t0 + 25 * 3600;
        let info = l.snapshot_for(&subj(), lim(), later);
        assert_eq!(info.day.remaining, 1000);
    }

    #[test]
    fn rejects_when_estimate_exceeds_day() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        // Spread 50 tokens across 20 distinct hours = 1000 (the daily ceiling).
        // Each hour's bucket (50) leaves hourly headroom, so the daily window —
        // not the hourly one — is what trips. Check in the last charged hour.
        for h in 0..20 {
            l.charge(&subj(), 50, now + h * 3600);
        }
        let at = now + 19 * 3600; // hour window holds only the last 50; day holds 1000
        let rej = l.check(&subj(), lim(), 1, at);
        assert!(matches!(rej, Err(Reject { window: Window::Day, .. })));
    }

    #[test]
    fn exact_at_limit_is_allowed() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        l.charge(&subj(), 90, now);
        // 90 used + 10 estimate == 100 == hourly ceiling; strict `>` means OK.
        assert!(l.check(&subj(), lim(), 10, now).is_ok());
    }

    #[test]
    fn subjects_with_same_payload_but_different_kind_are_isolated() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        let ip = TokenSubject::Ip("x".into());
        let user = TokenSubject::User("x".into());
        l.charge(&ip, 40, now);
        // The user subject must be unaffected by the ip subject's usage.
        assert_eq!(l.snapshot_for(&user, lim(), now).hour.remaining, 100);
        assert_eq!(l.snapshot_for(&ip, lim(), now).hour.remaining, 60);
    }

    #[test]
    fn snapshot_for_does_not_create_an_entry() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        // Querying an unseen subject reports full headroom and must NOT create
        // a usage entry (read-only).
        let info = l.snapshot_for(&subj(), lim(), now);
        assert_eq!(info.hour.remaining, 100);
        assert_eq!(info.day.remaining, 1000);
        assert!(l.usage.lock().unwrap().is_empty(), "snapshot_for must not insert");
    }

    #[test]
    fn huge_estimate_rejects_without_wrapping() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        l.charge(&subj(), 1, now);
        // saturating_add: 1 + u64::MAX saturates to u64::MAX > hourly, so reject.
        assert!(l.check(&subj(), lim(), u64::MAX, now).is_err());
    }

    fn test_admin_ctx(sub: &str) -> crate::middleware::bearer::AuthContext {
        crate::middleware::bearer::AuthContext {
            sub: sub.to_owned(),
            role: mn_auth::Role::Admin,
            tier: mn_auth::Tier::Admin,
            jti: String::new(),
        }
    }

    #[test]
    fn resolve_picks_tier_limits_and_user_override() {
        let l = TokenUsageLimiter::new(
            Limits { hourly: 2000, daily: 20000 },
            Limits { hourly: 4000, daily: 40000 },
            Limits {
                hourly: 500_000,
                daily: 100_000_000,
            },
        );
        // anonymous IP -> anon limits
        let (s, _t, lim) = l.resolve("203.0.113.9", None);
        assert!(matches!(s, TokenSubject::Ip(_)));
        assert_eq!(lim.hourly, 2000);
        // a user override beats the tier default
        l.set_overrides_for_test(vec![crate::tokenlimit_override::Parsed::user(
            "alice", 9999, 99999,
        )]);
        let auth = test_admin_ctx("alice");
        let (_s, _t, lim) = l.resolve("203.0.113.9", Some(&auth));
        assert_eq!(lim.hourly, 9999);
    }

    #[test]
    fn resolve_anonymous_cidr_override_beats_anon_default() {
        use std::net::IpAddr;
        use time::OffsetDateTime;

        let l = TokenUsageLimiter::new(
            Limits { hourly: 2000, daily: 20000 },
            Limits { hourly: 4000, daily: 40000 },
            Limits {
                hourly: 500_000,
                daily: 100_000_000,
            },
        );
        // Build a Parsed::Cidr directly covering 203.0.113.0/24.
        let cidr = crate::tokenlimit_override::Parsed::Cidr {
            net: "203.0.113.0".parse::<IpAddr>().unwrap(),
            prefix: 24,
            raw: "203.0.113.0/24".to_owned(),
            hourly: 7777,
            daily: 77777,
            created_at: OffsetDateTime::from_unix_timestamp(1).unwrap(),
        };
        l.set_overrides_for_test(vec![cidr]);
        // An IP inside the block resolves to the override limits, not anon default.
        let (s, t, lim) = l.resolve("203.0.113.5", None);
        assert!(matches!(s, TokenSubject::Ip(_)));
        assert_eq!(t, TokenTier::Anonymous);
        assert_eq!(lim.hourly, 7777);
        assert_eq!(lim.daily, 77777);
        // An IP outside the block falls through to anon defaults.
        let (_s, _t, lim_out) = l.resolve("203.0.114.1", None);
        assert_eq!(lim_out.hourly, 2000);
    }

    #[test]
    fn resolve_authenticated_user_ignores_cidr_override() {
        use std::net::IpAddr;
        use time::OffsetDateTime;

        // A JWT holder whose IP falls in a CIDR override must get their tier
        // default, NOT the CIDR limit (design §7.3: CIDR overrides never apply
        // to authenticated subjects).
        let l = TokenUsageLimiter::new(
            Limits { hourly: 2000, daily: 20000 },
            Limits { hourly: 4000, daily: 40000 },
            Limits {
                hourly: 500_000,
                daily: 100_000_000,
            },
        );
        let cidr = crate::tokenlimit_override::Parsed::Cidr {
            net: "203.0.113.0".parse::<IpAddr>().unwrap(),
            prefix: 24,
            raw: "203.0.113.0/24".to_owned(),
            hourly: 500, // far below the uplift tier default
            daily: 5000,
            created_at: OffsetDateTime::from_unix_timestamp(1).unwrap(),
        };
        l.set_overrides_for_test(vec![cidr]);
        let auth = crate::middleware::bearer::AuthContext {
            sub: "bob".to_owned(),
            role: mn_auth::Role::Admin,
            tier: mn_auth::Tier::ReadUplift,
            jti: String::new(),
        };
        // Bob is authenticated (read-uplift) and his IP is in the CIDR block,
        // but he has no user override -> he gets the uplift default (4000), not 500.
        let (_s, t, lim) = l.resolve("203.0.113.7", Some(&auth));
        assert_eq!(t, TokenTier::ReadUplift);
        assert_eq!(lim.hourly, 4000);
    }
}

//! In-memory, rolling-window embedding token accounting.
//! Hourly = rolling 60 min via per-minute buckets; daily = rolling 24h via
//! per-hour buckets. Both checked in-memory (no DB on the hot path). A later
//! task adds DB-backed overrides + a restart snapshot.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, RwLock};

/// One persisted snapshot bucket: `(subject_kind, subject, hour_epoch, tokens)`.
/// Matches the positional column order of `token_usage_snapshot`.
type SnapshotRow = (String, String, i64, i64);

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

impl TokenSubject {
    /// The `subject_kind` discriminant persisted in `token_usage_snapshot`
    /// (matches the table's `CHECK (subject_kind IN ('ip','user'))`).
    ///
    /// Note: this is intentionally `'ip'`, NOT the `'cidr'` discriminant used by
    /// `token_limit_override` — the snapshot keys on resolved client IPs while
    /// overrides key on CIDR blocks. Do not "align" the two.
    const fn kind(&self) -> &'static str {
        match self {
            Self::Ip(_) => "ip",
            Self::User(_) => "user",
        }
    }

    /// The subject payload (IP string or user id) persisted as `subject`.
    fn value(&self) -> &str {
        match self {
            Self::Ip(v) | Self::User(v) => v,
        }
    }
}

/// Which rolling window a rejection or window-info refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// Rolling 60-minute (hourly) per-subject window.
    Hour,
    /// Rolling 24-hour (daily) per-subject window.
    Day,
    /// Site-wide rolling cap (anti-Sybil backstop; admin-exempt).
    Global,
}

/// Stale-reservation TTL. A reservation is released/settled within one Voyage
/// call (<=30s timeout); anything older than this leaked (e.g. a panic between
/// `reserve` and `settle`) and is pruned so it can't permanently inflate the
/// in-flight total.
const RESERVATION_TTL_SECS: i64 = 60;

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
    /// In-flight reservations: id -> (estimated tokens, created_at secs).
    /// Counted against both windows until `settle`/`release` resolves them, so
    /// concurrent requests can't all pass `reserve` and overshoot the ceiling.
    reservations: BTreeMap<u64, (u64, i64)>,
    last_seen_secs: i64,
}

impl SubjectUsage {
    fn prune(&mut self, now: i64) {
        let min_floor = now / 60 - 59; // keep last 60 minutes
        let hr_floor = now / 3600 - 23; // keep last 24 hours
        self.minutes.retain(|&m, _| m >= min_floor);
        self.hours.retain(|&hh, _| hh >= hr_floor);
        let res_floor = now - RESERVATION_TTL_SECS;
        self.reservations.retain(|_, &mut (_, t)| t >= res_floor);
    }

    /// Sum of live in-flight reservations (call after `prune`).
    fn reserved(&self) -> u64 {
        self.reservations.values().map(|(amt, _)| *amt).sum()
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

/// Site-wide usage for the anti-Sybil global cap: a rolling per-minute window
/// plus in-flight reservations, summed across ALL non-admin subjects.
#[derive(Default)]
struct GlobalUsage {
    minutes: BTreeMap<i64, u64>,
    reservations: BTreeMap<u64, (u64, i64)>,
}

impl GlobalUsage {
    fn prune(&mut self, now: i64, window_min: i64) {
        let floor = now / 60 - (window_min - 1);
        self.minutes.retain(|&m, _| m >= floor);
        let res_floor = now - RESERVATION_TTL_SECS;
        self.reservations.retain(|_, &mut (_, t)| t >= res_floor);
    }

    fn used(&self, now: i64, window_min: i64) -> u64 {
        let floor = now / 60 - (window_min - 1);
        self.minutes.range(floor..).map(|(_, v)| *v).sum()
    }

    fn reserved(&self) -> u64 {
        self.reservations.values().map(|(amt, _)| *amt).sum()
    }

    fn reset(&self, now: i64, window_min: i64) -> i64 {
        let floor = now / 60 - (window_min - 1);
        self.minutes
            .range(floor..)
            .next()
            .map_or(now, |(&m, _)| (m + window_min) * 60)
    }
}

/// In-memory rolling-window token limiter.
///
/// # Concurrency
///
/// Prefer [`reserve`](Self::reserve) + [`settle`](Self::settle) on the request
/// path: `reserve` atomically counts an estimate against the budget (including
/// other in-flight reservations) so concurrent requests from one subject can't
/// all pass and overshoot; `settle` reconciles the reservation to the actual
/// token count once the upstream responds (or [`release`](Self::release) frees
/// it on failure). The legacy [`check`](Self::check)/[`charge`](Self::charge)
/// pair is NOT atomic across calls and is retained only for unit coverage of
/// the bucket math.
///
/// A site-wide [`Window::Global`] cap (admin-exempt) backstops Sybil abuse
/// (proxy / GitHub-account rotation). It is in-memory per process: with
/// horizontal scaling the effective cap is `instances * global_limit`; a truly
/// cross-instance cap would need a shared counter (DB/Redis) on the hot path.
pub struct TokenUsageLimiter {
    // Subject keys persist between requests; the periodic snapshot job
    // (`snapshot_to_db`) evicts a key once it has been idle for >24h, so the
    // map cannot grow without bound.
    usage: Mutex<HashMap<TokenSubject, SubjectUsage>>,
    /// Site-wide usage for the global cap. Lock ordering: always lock `usage`
    /// BEFORE `global` (nothing locks `global` first).
    global: Mutex<GlobalUsage>,
    anon: Limits,
    uplift: Limits,
    admin: Limits,
    /// Site-wide token ceiling over `global_window_min`. `u64::MAX` disables it.
    global_limit: u64,
    /// Global-cap rolling window length, in minutes.
    global_window_min: i64,
    /// Monotonic id source for reservations.
    next_reservation_id: std::sync::atomic::AtomicU64,
    overrides: RwLock<Vec<crate::tokenlimit_override::Parsed>>,
}

impl TokenUsageLimiter {
    /// Construct an `Arc`-wrapped limiter from server config, mirroring
    /// [`RateLimiter::from_config`](crate::ratelimit::RateLimiter::from_config).
    /// Unlike the rate limiter, token accounting is always on (there is no
    /// disable switch), so this returns the limiter unconditionally.
    #[must_use]
    pub fn from_config(cfg: &crate::config::ServerConfig) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::new_with_global(
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
            cfg.token_limit_global,
            cfg.token_limit_global_window_secs,
        ))
    }

    /// Construct a limiter with the three tier default ceilings and the global
    /// cap DISABLED (`u64::MAX`). Used by unit tests and any caller that doesn't
    /// need the site-wide backstop.
    #[must_use]
    pub fn new(anon: Limits, uplift: Limits, admin: Limits) -> Self {
        Self::new_with_global(anon, uplift, admin, u64::MAX, 10_800)
    }

    /// Construct a limiter with tier ceilings plus a site-wide global cap of
    /// `global_limit` tokens over `global_window_secs`. `u64::MAX` disables the
    /// global cap.
    #[must_use]
    pub fn new_with_global(
        anon: Limits,
        uplift: Limits,
        admin: Limits,
        global_limit: u64,
        global_window_secs: u64,
    ) -> Self {
        let global_window_min = i64::try_from(global_window_secs / 60).unwrap_or(180).max(1);
        Self {
            usage: Mutex::new(HashMap::new()),
            global: Mutex::new(GlobalUsage::default()),
            anon,
            uplift,
            admin,
            global_limit,
            global_window_min,
            next_reservation_id: std::sync::atomic::AtomicU64::new(1),
            overrides: RwLock::new(Vec::new()),
        }
    }

    /// Whether the global cap applies to `tier` (admin is exempt — it requires
    /// an Ed25519 challenge, so it is not Sybil-able, and legitimate ingest /
    /// migration needs volume).
    const fn global_applies(&self, tier: TokenTier) -> bool {
        self.global_limit != u64::MAX && !matches!(tier, TokenTier::Admin)
    }

    /// Atomically reserve `estimate` tokens for an in-flight request against the
    /// subject's windows and (for non-admin tiers) the global cap. Concurrent
    /// requests see each other's reservations, so they can't all pass and
    /// overshoot. Returns a reservation id to pass to [`settle`](Self::settle)
    /// or [`release`](Self::release).
    ///
    /// # Errors
    ///
    /// Returns [`Reject`] (with [`Window::Hour`], [`Window::Day`], or
    /// [`Window::Global`]) when the reservation would breach a ceiling.
    // The `usage` guard is deliberately held across the nested `global` lock and
    // the inserts so the subject + global reservation are atomic; tightening the
    // drop (as the lint suggests) would reopen the concurrency race.
    #[allow(clippy::significant_drop_tightening)]
    pub fn reserve(
        &self,
        subject: &TokenSubject,
        tier: TokenTier,
        limits: Limits,
        estimate: u64,
        now: i64,
    ) -> Result<u64, Reject> {
        let id = self
            .next_reservation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut map = self.usage.lock().expect("usage lock");
        let u = map.entry(subject.clone()).or_default();
        u.prune(now);
        let reserved = u.reserved();
        if u.hour_used(now)
            .saturating_add(reserved)
            .saturating_add(estimate)
            > limits.hourly
        {
            return Err(Reject {
                window: Window::Hour,
                limit: limits.hourly,
                reset_at_secs: u.hour_reset(now),
            });
        }
        if u.day_used(now)
            .saturating_add(reserved)
            .saturating_add(estimate)
            > limits.daily
        {
            return Err(Reject {
                window: Window::Day,
                limit: limits.daily,
                reset_at_secs: u.day_reset(now),
            });
        }
        if self.global_applies(tier) {
            // Lock ordering: usage (held) -> global.
            let mut g = self.global.lock().expect("global lock");
            g.prune(now, self.global_window_min);
            let projected = g
                .used(now, self.global_window_min)
                .saturating_add(g.reserved())
                .saturating_add(estimate);
            if projected > self.global_limit {
                return Err(Reject {
                    window: Window::Global,
                    limit: self.global_limit,
                    reset_at_secs: g.reset(now, self.global_window_min),
                });
            }
            g.reservations.insert(id, (estimate, now));
        }
        u.reservations.insert(id, (estimate, now));
        u.last_seen_secs = now;
        Ok(id)
    }

    /// Resolve reservation `id` by charging the ACTUAL token count to the
    /// durable buckets (subject + global for non-admin). Call after a successful
    /// upstream embedding response.
    #[allow(clippy::significant_drop_tightening)] // usage + global held together intentionally
    pub fn settle(&self, subject: &TokenSubject, tier: TokenTier, id: u64, actual: u64, now: i64) {
        let mut map = self.usage.lock().expect("usage lock");
        let u = map.entry(subject.clone()).or_default();
        u.prune(now);
        u.reservations.remove(&id);
        let minute = u.minutes.entry(now / 60).or_default();
        *minute = minute.saturating_add(actual);
        let hour = u.hours.entry(now / 3600).or_default();
        *hour = hour.saturating_add(actual);
        u.last_seen_secs = now;
        if self.global_applies(tier) {
            let mut g = self.global.lock().expect("global lock");
            g.reservations.remove(&id);
            let gm = g.minutes.entry(now / 60).or_default();
            *gm = gm.saturating_add(actual);
        }
    }

    /// Drop reservation `id` without charging (upstream failed). Frees the
    /// in-flight estimate for both the subject and the global cap.
    #[allow(clippy::significant_drop_tightening)] // usage + global held together intentionally
    pub fn release(&self, subject: &TokenSubject, tier: TokenTier, id: u64) {
        let mut map = self.usage.lock().expect("usage lock");
        if let Some(u) = map.get_mut(subject) {
            u.reservations.remove(&id);
        }
        if self.global_applies(tier) {
            self.global
                .lock()
                .expect("global lock")
                .reservations
                .remove(&id);
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

    /// Persist each subject's in-window per-hour buckets to
    /// `token_usage_snapshot` for restart durability, evict long-idle subjects
    /// from memory, and prune snapshot rows older than the day window.
    /// Best-effort (called by the periodic snapshot job).
    ///
    /// # Concurrency
    ///
    /// The `usage` mutex guard is **never** held across an `.await`: the
    /// in-memory state is copied into owned `Vec`s under the lock, the guard is
    /// dropped, the async DB I/O runs lock-free, and eviction re-locks briefly
    /// at the end. This keeps `clippy::await_holding_lock` satisfied and avoids
    /// stalling the embedding hot path during a snapshot.
    ///
    /// # Errors
    ///
    /// Propagates sqlx errors from the upsert / prune statements.
    pub async fn snapshot_to_db(&self, pool: &sqlx::PgPool, now: i64) -> Result<(), sqlx::Error> {
        // 1) Copy the in-memory state into owned tuples; flag idle subjects.
        let hr_floor = now / 3600 - 23; // keep the last 24 hours of buckets
        let idle_before = now - 86_400; // a subject untouched for >24h is evictable
        let mut rows: Vec<SnapshotRow> = Vec::new();
        let mut evict: Vec<TokenSubject> = Vec::new();
        {
            let map = self.usage.lock().expect("usage lock");
            for (subject, u) in map.iter() {
                for (&hour, &tokens) in u.hours.range(hr_floor..) {
                    rows.push((
                        subject.kind().to_owned(),
                        subject.value().to_owned(),
                        hour,
                        i64::try_from(tokens).unwrap_or(i64::MAX),
                    ));
                }
                if u.last_seen_secs < idle_before {
                    evict.push(subject.clone());
                }
            }
            drop(map); // explicit: no lock held across the awaits below
        }

        // 2) Upsert each bucket (no guard held).
        for (kind, value, hour, tokens) in rows {
            sqlx::query(
                "INSERT INTO token_usage_snapshot (subject_kind, subject, hour_epoch, tokens) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (subject_kind, subject, hour_epoch) \
                 DO UPDATE SET tokens = EXCLUDED.tokens, updated_at = now()",
            )
            .bind(kind)
            .bind(value)
            .bind(hour)
            .bind(tokens)
            .execute(pool)
            .await?;
        }

        // 3) Prune snapshot rows older than the day window. The `-25` floor is
        //    `load_from_db`'s `-23` read floor plus ~2h of slack for clock skew
        //    between the writer and a restarting reader — keep the two in step.
        sqlx::query("DELETE FROM token_usage_snapshot WHERE hour_epoch < $1")
            .bind(now / 3600 - 25)
            .execute(pool)
            .await?;

        // 4) Evict idle subjects from memory (re-lock briefly). Re-check freshness
        //    under the re-acquired lock: a `charge` may have landed between the
        //    snapshot-read and here, in which case the subject is no longer idle
        //    and must NOT be dropped (else its just-charged tokens are lost).
        if !evict.is_empty() {
            let mut map = self.usage.lock().expect("usage lock");
            for s in evict {
                let still_idle = map.get(&s).is_some_and(|u| u.last_seen_secs < idle_before);
                if still_idle {
                    map.remove(&s);
                }
            }
        }
        Ok(())
    }

    /// Seed per-subject hour buckets from the snapshot at boot, so token
    /// accounting survives a restart within the rolling day window.
    ///
    /// Only the per-hour (daily-window) buckets are persisted; per-minute
    /// (hourly-window) buckets are deliberately not durable — a restart resets
    /// the tighter hourly window to empty, which is the safe direction (it can
    /// only let a subject through, never wrongly reject).
    ///
    /// # Concurrency
    ///
    /// The DB read runs first (no lock held); the mutex is only taken to merge
    /// the loaded buckets into memory, never across an `.await`.
    ///
    /// # Errors
    ///
    /// Propagates sqlx errors from the snapshot read.
    pub async fn load_from_db(&self, pool: &sqlx::PgPool, now: i64) -> Result<(), sqlx::Error> {
        let hr_floor = now / 3600 - 23;
        let rows: Vec<SnapshotRow> = sqlx::query_as(
            "SELECT subject_kind, subject, hour_epoch, tokens FROM token_usage_snapshot \
             WHERE hour_epoch >= $1",
        )
        .bind(hr_floor)
        .fetch_all(pool)
        .await?;

        // Take the lock only to merge the loaded buckets (never across the
        // await above), then drop it immediately.
        let mut map = self.usage.lock().expect("usage lock");
        for (kind, value, hour, tokens) in rows {
            let subject = match kind.as_str() {
                "user" => TokenSubject::User(value),
                "ip" => TokenSubject::Ip(value),
                other => {
                    // The DB CHECK constrains this to ip/user; a surprising value
                    // means schema drift — surface it rather than silently coerce.
                    tracing::warn!(
                        kind = other,
                        "unrecognised subject_kind in snapshot; treating as ip"
                    );
                    TokenSubject::Ip(value)
                }
            };
            let u = map.entry(subject).or_default();
            let bucket = u.hours.entry(hour).or_default();
            *bucket = bucket.saturating_add(u64::try_from(tokens).unwrap_or(0));
            u.last_seen_secs = u.last_seen_secs.max(now);
        }
        drop(map);
        Ok(())
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

    // ---- reservations (concurrency safety) ----

    #[test]
    fn reserve_blocks_concurrent_overshoot() {
        let l = TokenUsageLimiter::new(d(), d(), d()); // hourly 100
        let now = 1_000_000_000;
        // First in-flight reservation of 60 succeeds.
        let _id1 = l
            .reserve(&subj(), TokenTier::Anonymous, lim(), 60, now)
            .expect("first reserve fits");
        // A concurrent second reservation of 60 must be REJECTED: 60 reserved +
        // 60 = 120 > 100, even though nothing has been charged yet.
        let rej = l.reserve(&subj(), TokenTier::Anonymous, lim(), 60, now);
        assert!(matches!(rej, Err(Reject { window: Window::Hour, .. })));
    }

    #[test]
    fn settle_reconciles_reservation_to_actual() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        let id = l
            .reserve(&subj(), TokenTier::Anonymous, lim(), 60, now)
            .unwrap();
        // The request actually used only 10 tokens.
        l.settle(&subj(), TokenTier::Anonymous, id, 10, now);
        // Only the actual 10 are charged; the 60 reservation is gone.
        assert_eq!(l.snapshot_for(&subj(), lim(), now).hour.remaining, 90);
        // And a fresh 60-token request now fits (10 used + 60 <= 100).
        assert!(l
            .reserve(&subj(), TokenTier::Anonymous, lim(), 60, now)
            .is_ok());
    }

    #[test]
    fn release_frees_the_reservation() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        let id = l
            .reserve(&subj(), TokenTier::Anonymous, lim(), 80, now)
            .unwrap();
        l.release(&subj(), TokenTier::Anonymous, id);
        // Reservation released (upstream failed) -> full headroom again.
        assert!(l
            .reserve(&subj(), TokenTier::Anonymous, lim(), 100, now)
            .is_ok());
    }

    // ---- global cap (anti-Sybil) ----

    fn big() -> Limits {
        Limits {
            hourly: 1_000_000,
            daily: 1_000_000,
        }
    }

    #[test]
    fn global_cap_rejects_rotated_subjects_but_not_admin() {
        // Per-subject ceilings are huge; only the global cap (100 tokens / 1h)
        // should bind. Each "subject" stands in for a rotated proxy/account.
        let l = TokenUsageLimiter::new_with_global(big(), big(), big(), 100, 3600);
        let now = 1_000_000_000;
        let s1 = TokenSubject::Ip("198.51.100.1".into());
        let s2 = TokenSubject::Ip("198.51.100.2".into());

        // First anon subject reserves 60 globally.
        let _id1 = l
            .reserve(&s1, TokenTier::Anonymous, big(), 60, now)
            .unwrap();
        // A DIFFERENT anon subject reserving 60 trips the GLOBAL cap (60 + 60 >
        // 100), even though its own per-subject budget is fine.
        let rej = l.reserve(&s2, TokenTier::Anonymous, big(), 60, now);
        assert!(matches!(rej, Err(Reject { window: Window::Global, .. })));

        // Admin is exempt from the global cap (not Sybil-able; needs volume).
        let admin = TokenSubject::User("ingest-admin".into());
        assert!(l.reserve(&admin, TokenTier::Admin, big(), 60, now).is_ok());
    }

    #[test]
    fn global_settle_charges_the_global_window() {
        let l = TokenUsageLimiter::new_with_global(big(), big(), big(), 100, 3600);
        let now = 1_000_000_000;
        let s1 = TokenSubject::Ip("198.51.100.1".into());
        let id = l
            .reserve(&s1, TokenTier::Anonymous, big(), 60, now)
            .unwrap();
        // Actual usage 90 charged to the global window.
        l.settle(&s1, TokenTier::Anonymous, id, 90, now);
        // A second subject's 20-token request now trips the global cap (90 + 20).
        let s2 = TokenSubject::Ip("198.51.100.2".into());
        let rej = l.reserve(&s2, TokenTier::Anonymous, big(), 20, now);
        assert!(matches!(rej, Err(Reject { window: Window::Global, .. })));
    }
}

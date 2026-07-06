//! Single-use challenge nonces for the Ed25519 challenge-response flow
//! (FR-056).
//!
//! Flow:
//!
//! 1. Client calls `POST /v1/auth/challenge` with `{user_id}`.
//! 2. Server mints a 32-byte random nonce, stores
//!    `(challenge_id → {user_id, nonce, expires_at})`, returns
//!    `{challenge_id, nonce}`.
//! 3. Client signs `nonce` with their Ed25519 private key.
//! 4. Client calls `POST /v1/auth/verify` with
//!    `{challenge_id, signature}`.
//! 5. Server consumes the challenge (single-use: present → remove; absent
//!    → reject), looks up `user_id`'s public key in the user store, verifies
//!    the signature, mints an admin JWT.
//!
//! TTL is ≤ 60s per FR-056. Each `consume` call removes the entry whether or
//! not verification succeeds — a replay against an already-consumed
//! `challenge_id` fails with [`ChallengeError::NotFound`].
//!
//! Storage is in-memory and per-process. v1 has a single cloud-server
//! process; if we ever go multi-region we'll need a shared backend (Redis
//! or a Postgres table) here.
//!
//! # Bounded memory (issue #160)
//!
//! `POST /v1/auth/challenge` mints an entry per request; a client that never
//! reaches `verify` leaks it. Minting an admin challenge requires a known
//! `user_id`, but there is no per-request rate limit by default, so an admin
//! id plus a loop can still grow the map without bound. To defend itself, the
//! store caps outstanding entries at [`MAX_ENTRIES`]: on `mint`, once at the
//! cap it reclaims expired entries and, if still saturated with live ones,
//! evicts the soonest-to-expire. This bounds peak memory regardless of load
//! and without relying on an external reaper cadence. [`purge_expired`] stays
//! available for callers that also want a periodic idle sweep.
//!
//! [`purge_expired`]: ChallengeStore::purge_expired

use std::collections::HashMap;
use std::sync::Mutex;

use rand_core::{OsRng, RngCore as _};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Bytes of randomness in each challenge nonce.
pub const NONCE_LEN: usize = 32;

/// Maximum challenge TTL permitted by spec (FR-056: nonces with TTL ≤ 60s).
pub const MAX_TTL: Duration = Duration::seconds(60);

/// Hard cap on outstanding challenges (issue #160).
///
/// Minting is admin-gated, so legitimate concurrency is tiny (a handful of
/// in-flight logins); this cap sits far above that while bounding worst-case
/// memory under a mint-and-abandon burst. Configurable per-store via
/// [`ChallengeStore::with_max_entries`].
pub const MAX_ENTRIES: usize = 10_000;

/// One outstanding challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// Opaque server-side identifier; round-trips on `verify`.
    pub challenge_id: String,
    /// The user that requested this challenge.
    pub user_id: String,
    /// 32 bytes of randomness the client signs.
    pub nonce: [u8; NONCE_LEN],
    /// When this challenge stops being accepted.
    pub expires_at: OffsetDateTime,
}

/// In-memory store of pending challenges. Lock-free reads aren't needed for
/// v1's throughput; a `Mutex<HashMap>` keeps the implementation small.
#[derive(Debug)]
pub struct ChallengeStore {
    inner: Mutex<HashMap<String, Challenge>>,
    /// Hard cap on outstanding entries — see [`MAX_ENTRIES`].
    max_entries: usize,
}

impl Default for ChallengeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ChallengeStore {
    /// Build an empty store with the default [`MAX_ENTRIES`] cap.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_entries(MAX_ENTRIES)
    }

    /// Build an empty store with an explicit outstanding-entry cap. Mainly for
    /// tests (which want a tiny cap to exercise eviction cheaply) and any future
    /// config-driven tuning. A `max_entries` of 0 is clamped to 1 so `mint`
    /// always has room to insert the entry it just created.
    #[must_use]
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max_entries: max_entries.max(1),
        }
    }

    /// Mint a new challenge for `user_id`, valid until `now + ttl`. TTL is
    /// clamped to [`MAX_TTL`] per FR-056.
    ///
    /// Self-defends against unbounded growth (issue #160): when the store is at
    /// its [`MAX_ENTRIES`] cap it first reclaims expired entries, then — if
    /// still full of live entries — evicts the soonest-to-expire one so the map
    /// never exceeds the cap. The `O(n)` sweep only runs at the cap, so honest
    /// churn pays it at most once per fill cycle rather than on every mint.
    pub fn mint(
        &self,
        user_id: impl Into<String>,
        now: OffsetDateTime,
        ttl: Duration,
    ) -> Challenge {
        let ttl = if ttl > MAX_TTL { MAX_TTL } else { ttl };
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let challenge = Challenge {
            challenge_id: Uuid::new_v4().to_string(),
            user_id: user_id.into(),
            nonce,
            expires_at: now + ttl,
        };
        let mut guard = self.inner.lock().expect("challenge store poisoned");
        if guard.len() >= self.max_entries {
            // Reclaim expired entries first (cheap win, and the common case
            // under sustained mint-and-abandon load).
            guard.retain(|_, c| c.expires_at > now);
            // Still saturated with live entries → drop the soonest-to-expire so
            // a burst that outpaces expiry can't push memory past the cap. The
            // entry we're about to insert has the latest expiry, so it's never
            // the victim.
            if guard.len() >= self.max_entries {
                if let Some(key) = soonest_expiry_key(&guard) {
                    guard.remove(&key);
                }
            }
        }
        guard.insert(challenge.challenge_id.clone(), challenge.clone());
        challenge
    }

    /// Single-use lookup: removes the challenge if present and not expired.
    /// On expiry the entry is also removed (housekeeping) but
    /// [`ChallengeError::Expired`] is returned so the caller can surface the
    /// right remediation.
    ///
    /// # Errors
    ///
    /// Returns [`ChallengeError::NotFound`] for unknown / already-consumed
    /// ids, or [`ChallengeError::Expired`] when the entry exists but is past
    /// its `expires_at`.
    pub fn consume(
        &self,
        challenge_id: &str,
        now: OffsetDateTime,
    ) -> Result<Challenge, ChallengeError> {
        let entry = {
            let mut guard = self.inner.lock().expect("challenge store poisoned");
            guard.remove(challenge_id).ok_or(ChallengeError::NotFound)?
        };
        if entry.expires_at <= now {
            return Err(ChallengeError::Expired);
        }
        Ok(entry)
    }

    /// Garbage-collect expired entries. Safe to call periodically; not
    /// load-bearing for correctness (`consume` already rejects expired
    /// entries) but keeps memory bounded under sustained mint-and-abandon
    /// load.
    pub fn purge_expired(&self, now: OffsetDateTime) -> usize {
        let mut guard = self.inner.lock().expect("challenge store poisoned");
        let before = guard.len();
        guard.retain(|_, c| c.expires_at > now);
        before - guard.len()
    }

    /// Number of outstanding challenges. Primarily useful for tests and
    /// future observability.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("challenge store poisoned").len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Key of the entry with the earliest `expires_at` (the next one to lapse),
/// or `None` when the map is empty. Used by [`ChallengeStore::mint`] to pick
/// an eviction victim when the store is saturated with live entries.
fn soonest_expiry_key(map: &HashMap<String, Challenge>) -> Option<String> {
    map.iter()
        .min_by_key(|(_, c)| c.expires_at)
        .map(|(k, _)| k.clone())
}

/// All the ways a challenge lookup can fail.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChallengeError {
    /// `challenge_id` was unknown or already consumed.
    #[error("challenge_id not found (already consumed or never minted)")]
    NotFound,
    /// `challenge_id` matched but the entry was past its expiry.
    #[error("challenge expired")]
    Expired,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_750_000_000).unwrap()
    }

    #[test]
    fn mint_then_consume_succeeds_once() {
        let store = ChallengeStore::new();
        let now = t0();
        let c = store.mint("aaron", now, Duration::seconds(60));
        assert_eq!(c.user_id, "aaron");
        assert_eq!(c.nonce.len(), NONCE_LEN);
        // Lengths and uniqueness sanity checks on the id.
        assert_eq!(store.len(), 1);

        let consumed = store.consume(&c.challenge_id, now).unwrap();
        assert_eq!(consumed.user_id, "aaron");
        assert_eq!(consumed.nonce, c.nonce);
        // Single-use: a second consume must miss.
        assert_eq!(store.consume(&c.challenge_id, now), Err(ChallengeError::NotFound));
        assert!(store.is_empty());
    }

    #[test]
    fn consume_rejects_unknown_id() {
        let store = ChallengeStore::new();
        assert_eq!(store.consume("nonexistent", t0()), Err(ChallengeError::NotFound));
    }

    #[test]
    fn consume_rejects_expired_challenge() {
        let store = ChallengeStore::new();
        let now = t0();
        let c = store.mint("aaron", now, Duration::seconds(60));
        let after = now + Duration::seconds(61);
        let err = store.consume(&c.challenge_id, after).unwrap_err();
        assert_eq!(err, ChallengeError::Expired);
        // Expired entries are removed by consume.
        assert!(store.is_empty());
    }

    #[test]
    fn ttl_is_clamped_to_max() {
        let store = ChallengeStore::new();
        let now = t0();
        let c = store.mint("aaron", now, Duration::hours(1));
        assert_eq!(c.expires_at, now + MAX_TTL);
    }

    #[test]
    fn purge_removes_expired_entries() {
        let store = ChallengeStore::new();
        let now = t0();
        let c1 = store.mint("a", now, Duration::seconds(10));
        let _c2 = store.mint("b", now, Duration::seconds(60));
        let later = now + Duration::seconds(30);
        let purged = store.purge_expired(later);
        assert_eq!(purged, 1);
        // c1 was the one with the shorter TTL.
        assert_eq!(store.consume(&c1.challenge_id, later), Err(ChallengeError::NotFound));
    }

    #[test]
    fn nonces_are_distinct_across_mints() {
        let store = ChallengeStore::new();
        let now = t0();
        let a = store.mint("a", now, Duration::seconds(60));
        let b = store.mint("a", now, Duration::seconds(60));
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.challenge_id, b.challenge_id);
    }

    // Issue #160: a mint-and-abandon burst must never grow the map past the
    // cap.
    #[test]
    fn mint_never_exceeds_hard_cap() {
        let store = ChallengeStore::with_max_entries(8);
        let now = t0();
        for _ in 0..1000 {
            store.mint("admin", now, Duration::seconds(60));
        }
        assert_eq!(store.len(), 8, "store must stay bounded at its cap");
    }

    // At the cap, minting first reclaims expired entries instead of evicting a
    // live one.
    #[test]
    fn mint_reclaims_expired_before_evicting_at_cap() {
        let store = ChallengeStore::with_max_entries(4);
        let now = t0();
        for _ in 0..4 {
            store.mint("admin", now, Duration::seconds(10));
        }
        assert_eq!(store.len(), 4);
        let later = now + Duration::seconds(30);
        let fresh = store.mint("admin", later, Duration::seconds(60));
        assert_eq!(store.len(), 1);
        assert!(store.consume(&fresh.challenge_id, later).is_ok());
    }

    // Saturated with live entries → the soonest-to-expire is evicted and the
    // just-minted entry survives.
    #[test]
    fn mint_evicts_soonest_expiry_when_saturated_with_live_entries() {
        let store = ChallengeStore::with_max_entries(3);
        let now = t0();
        let soonest = store.mint("admin", now, Duration::seconds(10));
        let _mid = store.mint("admin", now, Duration::seconds(30));
        let _late = store.mint("admin", now, Duration::seconds(59));
        assert_eq!(store.len(), 3);

        let newest = store.mint("admin", now, Duration::seconds(60));
        assert_eq!(store.len(), 3);
        assert_eq!(
            store.consume(&soonest.challenge_id, now),
            Err(ChallengeError::NotFound),
            "soonest-to-expire entry should have been evicted"
        );
        assert!(store.consume(&newest.challenge_id, now).is_ok());
    }
}

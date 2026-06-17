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
}

impl Default for ChallengeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ChallengeStore {
    /// Build an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Mint a new challenge for `user_id`, valid until `now + ttl`. TTL is
    /// clamped to [`MAX_TTL`] per FR-056.
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
}

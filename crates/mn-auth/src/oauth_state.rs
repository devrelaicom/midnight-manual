//! CSRF state store for the GitHub OAuth flow (FR-062, FR-115, FR-117).
//!
//! Flow:
//!
//! 1. Client (CLI) calls `GET /v1/auth/github/start?cli_port=<port>`.
//!
//! 2. Server mints a state token, stores
//!    `(state_id → {cli_port, expires_at})`, redirects the user-agent to
//!    GitHub's authorize URL with `state=<state_id>`.
//!
//! 3. GitHub redirects back to `/v1/auth/github/callback?code=…&state=<state_id>`.
//!
//! 4. Server consumes the state (single-use) — confirming the callback
//!    matches a request we initiated — then exchanges the code for an
//!    access token and proceeds with the rest of the OAuth dance.
//!
//! TTL is short: the user has minutes to finish the GitHub login. Spec
//! doesn't pin a number; we default to 10 minutes which is comfortable
//! for slow networks but small enough to bound replay attack surface.
//!
//! Storage is in-memory and per-process, mirroring [`crate::challenge`].

use std::collections::HashMap;
use std::sync::Mutex;

use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Default state-token TTL — 10 minutes.
pub const DEFAULT_TTL: Duration = Duration::minutes(10);

/// Upper bound on state-token TTL. Anything wider is clamped to this on
/// `mint` so a misconfiguration can't open a long-lived replay window.
pub const MAX_TTL: Duration = Duration::minutes(15);

/// One outstanding OAuth state entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthState {
    /// The state token GitHub round-trips back to us on the callback.
    pub state_id: String,
    /// Optional CLI local-listener port. When set, the callback redirects
    /// the user-agent back to `http://127.0.0.1:<cli_port>/oauth?…` so the
    /// CLI's local listener can capture the minted token. When `None`, the
    /// callback returns a JSON body for manual / scripted use.
    pub cli_port: Option<u16>,
    /// When this entry stops being accepted.
    pub expires_at: OffsetDateTime,
}

/// In-memory store of pending OAuth states.
#[derive(Debug)]
pub struct OAuthStateStore {
    inner: Mutex<HashMap<String, OAuthState>>,
}

impl Default for OAuthStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthStateStore {
    /// Build an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Mint a new state entry valid until `now + ttl`. `ttl` is clamped to
    /// [`MAX_TTL`].
    pub fn mint(&self, cli_port: Option<u16>, now: OffsetDateTime, ttl: Duration) -> OAuthState {
        let ttl = if ttl > MAX_TTL { MAX_TTL } else { ttl };
        let state = OAuthState {
            state_id: Uuid::new_v4().to_string(),
            cli_port,
            expires_at: now + ttl,
        };
        let mut guard = self.inner.lock().expect("oauth state store poisoned");
        guard.insert(state.state_id.clone(), state.clone());
        state
    }

    /// Single-use lookup: removes the state if present and not expired.
    ///
    /// # Errors
    ///
    /// Returns [`OAuthStateError::NotFound`] for unknown / already-consumed
    /// ids, or [`OAuthStateError::Expired`] when the entry exists but is past
    /// its `expires_at`.
    pub fn consume(
        &self,
        state_id: &str,
        now: OffsetDateTime,
    ) -> Result<OAuthState, OAuthStateError> {
        let entry = {
            let mut guard = self.inner.lock().expect("oauth state store poisoned");
            guard.remove(state_id).ok_or(OAuthStateError::NotFound)?
        };
        if entry.expires_at <= now {
            return Err(OAuthStateError::Expired);
        }
        Ok(entry)
    }

    /// Garbage-collect expired entries.
    pub fn purge_expired(&self, now: OffsetDateTime) -> usize {
        let mut guard = self.inner.lock().expect("oauth state store poisoned");
        let before = guard.len();
        guard.retain(|_, s| s.expires_at > now);
        before - guard.len()
    }

    /// Number of outstanding state entries.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("oauth state store poisoned").len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// All the ways an OAuth state lookup can fail.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OAuthStateError {
    /// `state_id` was unknown or already consumed (CSRF check fired).
    #[error("state not found (already consumed or never minted)")]
    NotFound,
    /// `state_id` matched but the entry was past its expiry.
    #[error("oauth state expired")]
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
        let store = OAuthStateStore::new();
        let s = store.mint(Some(54321), t0(), DEFAULT_TTL);
        assert_eq!(s.cli_port, Some(54321));
        let consumed = store.consume(&s.state_id, t0()).unwrap();
        assert_eq!(consumed.cli_port, Some(54321));
        assert_eq!(store.consume(&s.state_id, t0()), Err(OAuthStateError::NotFound));
        assert!(store.is_empty());
    }

    #[test]
    fn mint_without_cli_port_works() {
        let store = OAuthStateStore::new();
        let s = store.mint(None, t0(), DEFAULT_TTL);
        assert_eq!(s.cli_port, None);
        let consumed = store.consume(&s.state_id, t0()).unwrap();
        assert_eq!(consumed.cli_port, None);
    }

    #[test]
    fn consume_rejects_unknown_id() {
        let store = OAuthStateStore::new();
        assert_eq!(store.consume("nope", t0()), Err(OAuthStateError::NotFound));
    }

    #[test]
    fn consume_rejects_expired() {
        let store = OAuthStateStore::new();
        let s = store.mint(Some(1), t0(), Duration::seconds(60));
        let later = t0() + Duration::seconds(61);
        let err = store.consume(&s.state_id, later).unwrap_err();
        assert_eq!(err, OAuthStateError::Expired);
        // Removed even on expiry.
        assert!(store.is_empty());
    }

    #[test]
    fn ttl_is_clamped_to_max() {
        let store = OAuthStateStore::new();
        let s = store.mint(None, t0(), Duration::hours(1));
        assert_eq!(s.expires_at, t0() + MAX_TTL);
    }

    #[test]
    fn purge_drops_expired_entries() {
        let store = OAuthStateStore::new();
        let s1 = store.mint(None, t0(), Duration::seconds(10));
        let _s2 = store.mint(None, t0(), DEFAULT_TTL);
        let later = t0() + Duration::seconds(30);
        let purged = store.purge_expired(later);
        assert_eq!(purged, 1);
        assert_eq!(store.consume(&s1.state_id, later), Err(OAuthStateError::NotFound));
    }

    #[test]
    fn state_ids_are_unique() {
        let store = OAuthStateStore::new();
        let a = store.mint(None, t0(), DEFAULT_TTL);
        let b = store.mint(None, t0(), DEFAULT_TTL);
        assert_ne!(a.state_id, b.state_id);
    }
}

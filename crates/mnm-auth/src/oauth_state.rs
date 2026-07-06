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
//!
//! # Bounded memory (issue #160)
//!
//! `GET /v1/auth/github/start` is unauthenticated, so a scanner (or ordinary
//! users who never complete the redirect) can `mint` states that are never
//! `consume`d. To keep the map from growing without bound, the store defends
//! itself on `mint`: whenever it reaches [`MAX_ENTRIES`] it first reclaims
//! expired entries, then — if still saturated with *live* entries — evicts the
//! soonest-to-expire one to make room. This bounds peak memory to
//! `MAX_ENTRIES` regardless of load and without depending on an external
//! reaper cadence (which cannot cap growth *between* ticks). [`purge_expired`]
//! remains available for callers that also want a periodic idle sweep.
//!
//! [`purge_expired`]: OAuthStateStore::purge_expired

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

/// Hard cap on outstanding OAuth-state entries (issue #160).
///
/// The endpoint is unauthenticated, so this bounds worst-case memory: a
/// mint-and-abandon burst can never grow the map past this many entries. Sized
/// far above any plausible count of genuinely-concurrent in-flight GitHub
/// logins (dozens to low hundreds) while still capping memory at a few MiB.
/// Configurable per-store via [`OAuthStateStore::with_max_entries`].
pub const MAX_ENTRIES: usize = 50_000;

/// Upper bound on the client-supplied `cli_state` nonce (issue #177).
///
/// The start endpoint is unauthenticated, so the nonce is attacker-influenceable;
/// [`OAuthStateStore::mint`] drops any `cli_state` longer than this so a
/// mint-and-abandon burst can't amplify per-entry memory. A legitimate
/// [`generate_cli_nonce`] value is 22 chars, comfortably under the cap.
pub const MAX_CLI_STATE_LEN: usize = 128;

/// Generate a fresh, URL-safe CSRF nonce for the CLI loopback OAuth flow
/// (issue #177).
///
/// 128 bits of OS randomness, base64url-encoded (22 chars, no padding). The
/// CLI sends this as `cli_state` on `GET /v1/auth/github/start`; the server
/// round-trips it into the `http://127.0.0.1:<port>/oauth?…&state=<nonce>`
/// redirect; and the CLI rejects any loopback callback whose `state` doesn't
/// match. A co-resident process that races the ephemeral port without knowing
/// the nonce therefore cannot fixate an attacker-chosen token.
#[must_use]
pub fn generate_cli_nonce() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use rand_core::{OsRng, RngCore as _};

    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

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
    /// Optional CLI-generated CSRF nonce (issue #177). When set, the callback
    /// echoes it back as `state=<nonce>` in the loopback redirect so the CLI
    /// can reject any callback it didn't initiate. Sanitized on `mint`:
    /// empties and over-[`MAX_CLI_STATE_LEN`] values are dropped to `None`.
    pub cli_state: Option<String>,
    /// When this entry stops being accepted.
    pub expires_at: OffsetDateTime,
}

/// In-memory store of pending OAuth states.
#[derive(Debug)]
pub struct OAuthStateStore {
    inner: Mutex<HashMap<String, OAuthState>>,
    /// Hard cap on outstanding entries — see [`MAX_ENTRIES`].
    max_entries: usize,
}

impl Default for OAuthStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthStateStore {
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

    /// Mint a new state entry valid until `now + ttl`. `ttl` is clamped to
    /// [`MAX_TTL`]. `cli_state` is the CLI's CSRF nonce (issue #177), sanitized
    /// here: empties and over-[`MAX_CLI_STATE_LEN`] values are stored as `None`.
    ///
    /// Self-defends against unbounded growth (issue #160): when the store is at
    /// its [`MAX_ENTRIES`] cap it first reclaims expired entries, then — if
    /// still full of live entries — evicts the soonest-to-expire one so the map
    /// never exceeds the cap. The `O(n)` work (the `retain` sweep, plus the
    /// `min_by_key` eviction scan) only runs while the store is at the cap:
    /// under honest churn that's at most once per fill cycle, but under a
    /// sustained flood that pins the store at the cap with nothing expiring,
    /// every mint pays both scans under the lock. That's an accepted cost —
    /// bounding memory is the priority, and the map size (hence the scan cost)
    /// is itself capped.
    pub fn mint(
        &self,
        cli_port: Option<u16>,
        cli_state: Option<String>,
        now: OffsetDateTime,
        ttl: Duration,
    ) -> OAuthState {
        let ttl = if ttl > MAX_TTL { MAX_TTL } else { ttl };
        // Sanitize the attacker-influenceable nonce: drop empties and anything
        // longer than the cap so a mint-and-abandon burst can't amplify memory.
        let cli_state = cli_state.filter(|s| !s.is_empty() && s.len() <= MAX_CLI_STATE_LEN);
        let state = OAuthState {
            state_id: Uuid::new_v4().to_string(),
            cli_port,
            cli_state,
            expires_at: now + ttl,
        };
        let mut guard = self.inner.lock().expect("oauth state store poisoned");
        if guard.len() >= self.max_entries {
            // Reclaim expired entries first (cheap win, and the common case
            // under sustained mint-and-abandon load).
            guard.retain(|_, s| s.expires_at > now);
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

/// Key of the entry with the earliest `expires_at` (the next one to lapse),
/// or `None` when the map is empty. Used by [`OAuthStateStore::mint`] to pick
/// an eviction victim when the store is saturated with live entries.
fn soonest_expiry_key(map: &HashMap<String, OAuthState>) -> Option<String> {
    map.iter()
        .min_by_key(|(_, s)| s.expires_at)
        .map(|(k, _)| k.clone())
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
        let s = store.mint(Some(54321), None, t0(), DEFAULT_TTL);
        assert_eq!(s.cli_port, Some(54321));
        let consumed = store.consume(&s.state_id, t0()).unwrap();
        assert_eq!(consumed.cli_port, Some(54321));
        assert_eq!(store.consume(&s.state_id, t0()), Err(OAuthStateError::NotFound));
        assert!(store.is_empty());
    }

    #[test]
    fn mint_without_cli_port_works() {
        let store = OAuthStateStore::new();
        let s = store.mint(None, None, t0(), DEFAULT_TTL);
        assert_eq!(s.cli_port, None);
        let consumed = store.consume(&s.state_id, t0()).unwrap();
        assert_eq!(consumed.cli_port, None);
    }

    // Issue #177: the CLI nonce round-trips through mint/consume so the
    // callback can echo it back to the CLI's loopback listener.
    #[test]
    fn mint_round_trips_cli_state_nonce() {
        let store = OAuthStateStore::new();
        let nonce = generate_cli_nonce();
        let s = store.mint(Some(54321), Some(nonce.clone()), t0(), DEFAULT_TTL);
        assert_eq!(s.cli_state.as_deref(), Some(nonce.as_str()));
        let consumed = store.consume(&s.state_id, t0()).unwrap();
        assert_eq!(consumed.cli_state, Some(nonce));
    }

    // A fresh nonce is non-empty, URL-safe, and well under the length cap.
    #[test]
    fn generate_cli_nonce_is_urlsafe_and_bounded() {
        let a = generate_cli_nonce();
        let b = generate_cli_nonce();
        assert_ne!(a, b, "nonces must be unpredictable");
        assert!(!a.is_empty() && a.len() <= MAX_CLI_STATE_LEN);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "nonce must be URL-safe (base64url, no padding): {a}",
        );
    }

    // Empty or over-long client nonces are dropped rather than stored, so the
    // unauthenticated endpoint can't be used to amplify per-entry memory.
    #[test]
    fn mint_sanitizes_oversized_and_empty_cli_state() {
        let store = OAuthStateStore::new();
        let empty = store.mint(Some(1), Some(String::new()), t0(), DEFAULT_TTL);
        assert_eq!(empty.cli_state, None);
        let huge = store.mint(Some(1), Some("x".repeat(MAX_CLI_STATE_LEN + 1)), t0(), DEFAULT_TTL);
        assert_eq!(huge.cli_state, None);
        let ok = store.mint(Some(1), Some("x".repeat(MAX_CLI_STATE_LEN)), t0(), DEFAULT_TTL);
        assert_eq!(ok.cli_state.as_deref(), Some("x".repeat(MAX_CLI_STATE_LEN).as_str()));
    }

    #[test]
    fn consume_rejects_unknown_id() {
        let store = OAuthStateStore::new();
        assert_eq!(store.consume("nope", t0()), Err(OAuthStateError::NotFound));
    }

    #[test]
    fn consume_rejects_expired() {
        let store = OAuthStateStore::new();
        let s = store.mint(Some(1), None, t0(), Duration::seconds(60));
        let later = t0() + Duration::seconds(61);
        let err = store.consume(&s.state_id, later).unwrap_err();
        assert_eq!(err, OAuthStateError::Expired);
        // Removed even on expiry.
        assert!(store.is_empty());
    }

    #[test]
    fn ttl_is_clamped_to_max() {
        let store = OAuthStateStore::new();
        let s = store.mint(None, None, t0(), Duration::hours(1));
        assert_eq!(s.expires_at, t0() + MAX_TTL);
    }

    #[test]
    fn purge_drops_expired_entries() {
        let store = OAuthStateStore::new();
        let s1 = store.mint(None, None, t0(), Duration::seconds(10));
        let _s2 = store.mint(None, None, t0(), DEFAULT_TTL);
        let later = t0() + Duration::seconds(30);
        let purged = store.purge_expired(later);
        assert_eq!(purged, 1);
        assert_eq!(store.consume(&s1.state_id, later), Err(OAuthStateError::NotFound));
    }

    #[test]
    fn state_ids_are_unique() {
        let store = OAuthStateStore::new();
        let a = store.mint(None, None, t0(), DEFAULT_TTL);
        let b = store.mint(None, None, t0(), DEFAULT_TTL);
        assert_ne!(a.state_id, b.state_id);
    }

    // Issue #160: the store must bound its own memory. A sustained
    // mint-and-abandon burst (the scanner scenario) must never grow the map
    // past the cap.
    #[test]
    fn mint_never_exceeds_hard_cap() {
        let store = OAuthStateStore::with_max_entries(8);
        for _ in 0..1000 {
            store.mint(None, None, t0(), DEFAULT_TTL);
        }
        assert_eq!(store.len(), 8, "store must stay bounded at its cap");
    }

    // At the cap, minting first reclaims expired entries instead of evicting a
    // live one — so an abandoned burst is cleaned up opportunistically.
    #[test]
    fn mint_reclaims_expired_before_evicting_at_cap() {
        let store = OAuthStateStore::with_max_entries(4);
        // Fill with short-lived entries.
        for _ in 0..4 {
            store.mint(None, None, t0(), Duration::seconds(10));
        }
        assert_eq!(store.len(), 4);
        // Time advances past their expiry; the next mint should sweep all four
        // expired entries and leave just the fresh one.
        let later = t0() + Duration::seconds(30);
        let fresh = store.mint(None, None, later, DEFAULT_TTL);
        assert_eq!(store.len(), 1);
        assert!(store.consume(&fresh.state_id, later).is_ok());
    }

    // When the store is saturated with *live* entries, the mint evicts the
    // soonest-to-expire one and keeps the just-minted entry.
    #[test]
    fn mint_evicts_soonest_expiry_when_saturated_with_live_entries() {
        let store = OAuthStateStore::with_max_entries(3);
        // Three live entries with staggered expiries; the first has the
        // soonest expiry and should be the eviction victim.
        let soonest = store.mint(None, None, t0(), Duration::minutes(1));
        let _mid = store.mint(None, None, t0(), Duration::minutes(5));
        let _late = store.mint(None, None, t0(), Duration::minutes(9));
        assert_eq!(store.len(), 3);

        // A fourth live mint at the cap: no entry is expired, so the
        // soonest-to-expire (`soonest`) is evicted to make room.
        let newest = store.mint(None, None, t0(), DEFAULT_TTL);
        assert_eq!(store.len(), 3);
        assert_eq!(
            store.consume(&soonest.state_id, t0()),
            Err(OAuthStateError::NotFound),
            "soonest-to-expire entry should have been evicted"
        );
        // The just-minted entry survives.
        assert!(store.consume(&newest.state_id, t0()).is_ok());
    }
}

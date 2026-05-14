//! HS256 JWT mint + verify (FR-058, FR-117).
//!
//! Two JWT shapes live behind this module:
//!
//! - **Admin** — minted by the Ed25519 challenge-response flow; carries
//!   `tier = "admin"`, the user's role, and a 1-hour TTL (D21).
//! - **Read-uplift** — minted by the GitHub OAuth flow; carries
//!   `tier = "read_uplift"` and a 30-day TTL (FR-117). It can never satisfy
//!   a write endpoint's role check because the tier guard runs first.
//!
//! Both shapes share the same `Claims` struct + the same HS256 signing
//! secret. The wire format is otherwise stock JWT — `Authorization: Bearer
//! <token>` on every authenticated request.
//!
//! The signing secret comes from `MIDNIGHT_MANUAL_JWT_SECRET` (32+ random
//! bytes per spec line 584). Rotating the secret invalidates every
//! previously-issued token (SC-033) — admins re-authenticate via
//! `mnm login`.

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::role::{Role, Tier};

/// Default admin-token TTL — 1 hour (D21).
pub const DEFAULT_ADMIN_TTL: time::Duration = time::Duration::hours(1);

/// Default read-uplift TTL — 30 days (FR-117).
pub const DEFAULT_READ_UPLIFT_TTL: time::Duration = time::Duration::days(30);

/// JWT claims carried in both token types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — the `user_id` (or GitHub login for read-uplift tokens).
    pub sub: String,
    /// Issued-at, Unix seconds.
    pub iat: i64,
    /// Expiry, Unix seconds.
    pub exp: i64,
    /// Caller role. For read-uplift tokens this is always `writer`-equivalent
    /// privilege but the tier guard refuses writes regardless.
    pub role: Role,
    /// Privilege tier — distinguishes admin from read-uplift (FR-117).
    pub tier: Tier,
    /// JWT id — every mint gets a fresh UUID so future revocation lists work.
    pub jti: String,
}

impl Claims {
    /// Build claims for an admin JWT minted from a successful Ed25519
    /// challenge-response. `iat` is the supplied `now`; `exp` is `now + ttl`.
    #[must_use]
    pub fn admin(
        user_id: impl Into<String>,
        role: Role,
        now: OffsetDateTime,
        ttl: time::Duration,
    ) -> Self {
        Self {
            sub: user_id.into(),
            iat: now.unix_timestamp(),
            exp: (now + ttl).unix_timestamp(),
            role,
            tier: Tier::Admin,
            jti: Uuid::new_v4().to_string(),
        }
    }

    /// Build claims for a read-uplift JWT minted from a successful GitHub
    /// OAuth flow. `sub` is the GitHub login per spec line 397.
    #[must_use]
    pub fn read_uplift(
        github_login: impl Into<String>,
        now: OffsetDateTime,
        ttl: time::Duration,
    ) -> Self {
        Self {
            sub: github_login.into(),
            iat: now.unix_timestamp(),
            // Role on a read-uplift token is meaningless for authorization
            // (the tier guard refuses writes); we serialize `writer` because
            // it's the lowest-privilege role we have and the field is
            // required.
            exp: (now + ttl).unix_timestamp(),
            role: Role::Writer,
            tier: Tier::ReadUplift,
            jti: Uuid::new_v4().to_string(),
        }
    }
}

/// HS256 signing secret. Construct via [`SigningSecret::from_bytes`] — the
/// raw byte buffer is kept inside an opaque newtype so it can't accidentally
/// be `Debug`-printed.
#[derive(Clone)]
pub struct SigningSecret(Vec<u8>);

impl SigningSecret {
    /// Build a signing secret from raw bytes. Spec line 584 requires 32+ bytes
    /// of randomness from `MIDNIGHT_MANUAL_JWT_SECRET`.
    ///
    /// # Errors
    ///
    /// Returns [`JwtError::SecretTooShort`] if `bytes.len() < 32`.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, JwtError> {
        if bytes.len() < 32 {
            return Err(JwtError::SecretTooShort(bytes.len()));
        }
        Ok(Self(bytes))
    }
}

impl std::fmt::Debug for SigningSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigningSecret(redacted)")
    }
}

/// Mint a JWT for the given claims.
///
/// # Errors
///
/// Returns [`JwtError::Encode`] if `jsonwebtoken` fails to serialize the
/// claims (effectively never given our shape, but bubble up rather than
/// `expect`).
pub fn mint(secret: &SigningSecret, claims: &Claims) -> Result<String, JwtError> {
    let header = Header::new(Algorithm::HS256);
    let key = EncodingKey::from_secret(&secret.0);
    encode(&header, claims, &key).map_err(|e| JwtError::Encode(e.to_string()))
}

/// Verify a JWT, returning the embedded claims on success.
///
/// Verification checks: HS256 signature, presence of `sub`/`iat`/`exp`/
/// `role`/`tier`/`jti`, expiry against `now`, and a clock-skew leeway of 0
/// (callers can apply their own leeway if needed).
///
/// # Errors
///
/// Returns [`JwtError::Expired`] when the embedded `exp` is in the past
/// relative to `now`, [`JwtError::BadSignature`] when the signature doesn't
/// match the secret, [`JwtError::Malformed`] on missing claims / wrong shape.
pub fn verify(
    secret: &SigningSecret,
    token: &str,
    now: OffsetDateTime,
) -> Result<Claims, JwtError> {
    let key = DecodingKey::from_secret(&secret.0);
    let mut validation = Validation::new(Algorithm::HS256);
    validation.required_spec_claims = ["sub", "iat", "exp"]
        .into_iter()
        .map(String::from)
        .collect();
    // We validate `exp` ourselves so we can return a typed `Expired` error.
    validation.validate_exp = false;
    // No issuer / audience pinning in v1.
    validation.validate_aud = false;

    let data = decode::<Claims>(token, &key, &validation).map_err(|e| match e.kind() {
        jsonwebtoken::errors::ErrorKind::InvalidSignature => JwtError::BadSignature,
        _ => JwtError::Malformed(e.to_string()),
    })?;
    let claims = data.claims;
    if claims.exp <= now.unix_timestamp() {
        return Err(JwtError::Expired {
            exp: claims.exp,
            now: now.unix_timestamp(),
        });
    }
    Ok(claims)
}

/// All the ways a JWT operation can fail.
#[derive(Debug, Error)]
pub enum JwtError {
    /// `MIDNIGHT_MANUAL_JWT_SECRET` was shorter than the 32-byte floor.
    #[error("signing secret has {0} bytes; expected at least 32 (spec line 584)")]
    SecretTooShort(usize),
    /// Encoding failure on mint.
    #[error("jwt encode: {0}")]
    Encode(String),
    /// Token did not validate against the supplied secret.
    #[error("jwt signature did not verify")]
    BadSignature,
    /// Token is structurally wrong / missing required claims.
    #[error("jwt malformed: {0}")]
    Malformed(String),
    /// Token's `exp` is in the past.
    #[error("jwt expired (exp={exp}, now={now})")]
    Expired {
        /// `exp` from the token.
        exp: i64,
        /// Verifier's `now`.
        now: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> SigningSecret {
        SigningSecret::from_bytes(vec![7u8; 32]).unwrap()
    }

    fn t_now() -> OffsetDateTime {
        // Fixed instant so test math is reproducible.
        OffsetDateTime::from_unix_timestamp(1_750_000_000).unwrap()
    }

    #[test]
    fn round_trip_admin_token() {
        let s = secret();
        let now = t_now();
        let claims = Claims::admin("aaron", Role::Admin, now, DEFAULT_ADMIN_TTL);
        let token = mint(&s, &claims).unwrap();
        let got = verify(&s, &token, now).unwrap();
        assert_eq!(got.sub, "aaron");
        assert_eq!(got.role, Role::Admin);
        assert_eq!(got.tier, Tier::Admin);
        assert_eq!(got.exp - got.iat, DEFAULT_ADMIN_TTL.whole_seconds());
    }

    #[test]
    fn round_trip_read_uplift_token() {
        let s = secret();
        let now = t_now();
        let claims = Claims::read_uplift("aaronbassett", now, DEFAULT_READ_UPLIFT_TTL);
        let token = mint(&s, &claims).unwrap();
        let got = verify(&s, &token, now).unwrap();
        assert_eq!(got.sub, "aaronbassett");
        assert_eq!(got.tier, Tier::ReadUplift);
        assert_eq!(got.exp - got.iat, DEFAULT_READ_UPLIFT_TTL.whole_seconds());
    }

    #[test]
    fn verify_rejects_expired() {
        let s = secret();
        let now = t_now();
        let claims = Claims::admin("aaron", Role::Admin, now, time::Duration::seconds(1));
        let token = mint(&s, &claims).unwrap();
        let later = now + time::Duration::seconds(2);
        let err = verify(&s, &token, later).unwrap_err();
        assert!(matches!(err, JwtError::Expired { .. }));
    }

    #[test]
    fn verify_rejects_different_secret() {
        let signer = secret();
        let now = t_now();
        let claims = Claims::admin("aaron", Role::Admin, now, DEFAULT_ADMIN_TTL);
        let token = mint(&signer, &claims).unwrap();
        let other = SigningSecret::from_bytes(vec![9u8; 32]).unwrap();
        let err = verify(&other, &token, now).unwrap_err();
        assert!(matches!(err, JwtError::BadSignature));
    }

    #[test]
    fn verify_rejects_tampered_token() {
        let s = secret();
        let now = t_now();
        let claims = Claims::admin("aaron", Role::Admin, now, DEFAULT_ADMIN_TTL);
        let mut token = mint(&s, &claims).unwrap();
        // Flip a byte in the signature segment.
        let signature_segment = token.rfind('.').unwrap();
        let mut bytes = token.into_bytes();
        let idx = signature_segment + 5;
        bytes[idx] = if bytes[idx] == b'a' { b'b' } else { b'a' };
        token = String::from_utf8(bytes).unwrap();
        let err = verify(&s, &token, now).unwrap_err();
        assert!(matches!(err, JwtError::BadSignature | JwtError::Malformed(_)));
    }

    #[test]
    fn short_secret_is_rejected() {
        let err = SigningSecret::from_bytes(vec![0u8; 16]).unwrap_err();
        assert!(matches!(err, JwtError::SecretTooShort(16)));
    }

    #[test]
    fn verify_rejects_random_garbage() {
        let s = secret();
        let err = verify(&s, "not.a.jwt", t_now()).unwrap_err();
        // Either Malformed or BadSignature depending on which check trips
        // first — both are correct rejections.
        assert!(matches!(err, JwtError::Malformed(_) | JwtError::BadSignature));
    }

    #[test]
    fn jti_is_unique_per_mint() {
        let now = t_now();
        let a = Claims::admin("aaron", Role::Admin, now, DEFAULT_ADMIN_TTL);
        let b = Claims::admin("aaron", Role::Admin, now, DEFAULT_ADMIN_TTL);
        assert_ne!(a.jti, b.jti, "every mint must mint a fresh jti");
    }

    #[test]
    fn signing_secret_debug_is_redacted() {
        let s = secret();
        let printed = format!("{s:?}");
        assert!(!printed.contains('7'), "signing secret bytes must not appear in Debug");
        assert!(printed.contains("redacted"));
    }
}

//! Ed25519 keypair primitives (D10).
//!
//! Admin auth is challenge-response: the client signs a server-issued nonce
//! with their Ed25519 private key, the server verifies against the public
//! half stored in the user store, and on success mints an HS256 JWT.
//!
//! Wire format for stored public keys: `ed25519:<base64(32 bytes)>`. The
//! prefix is a future-proofing knob — if a v2 ever introduces a second
//! signature scheme, we can add an alternate prefix without churning the
//! user-store schema.
//!
//! Storage discipline (FR-067):
//!
//! - The private half is written to disk with mode `0o600`.
//! - The private half is NEVER echoed to stdout or any log.
//! - The public half is echoed only in the canonical wire form so the user
//!   can paste it into the user-store TOML.

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use thiserror::Error;

/// Wire prefix for an Ed25519 public key in the user-store TOML.
pub const ED25519_WIRE_PREFIX: &str = "ed25519:";

/// Length of an Ed25519 public key, in bytes (RFC 8032).
pub const PUBLIC_KEY_LEN: usize = 32;

/// Length of an Ed25519 signing (private) key, in bytes (the seed).
pub const SIGNING_KEY_LEN: usize = 32;

/// Length of an Ed25519 signature.
pub const SIGNATURE_LEN: usize = 64;

/// Owned Ed25519 keypair. The signing half is held in memory — callers are
/// responsible for never logging it (FR-067).
#[derive(Debug)]
pub struct Keypair {
    signing: SigningKey,
}

impl Keypair {
    /// Generate a fresh random keypair using the OS RNG.
    #[must_use]
    pub fn generate() -> Self {
        let mut rng = rand_core::OsRng;
        Self {
            signing: SigningKey::generate(&mut rng),
        }
    }

    /// Build a keypair from a 32-byte signing-key seed.
    #[must_use]
    pub fn from_signing_bytes(seed: [u8; SIGNING_KEY_LEN]) -> Self {
        Self {
            signing: SigningKey::from_bytes(&seed),
        }
    }

    /// Return the raw 32-byte seed of the signing half. Callers MUST persist
    /// this with `0o600` perms and never echo it.
    #[must_use]
    pub fn signing_bytes(&self) -> [u8; SIGNING_KEY_LEN] {
        self.signing.to_bytes()
    }

    /// Return the verifying (public) half.
    #[must_use]
    pub fn verifying(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    /// Encode the public half in the user-store wire form
    /// (`ed25519:<base64(32 bytes)>`).
    #[must_use]
    pub fn public_wire(&self) -> String {
        encode_public_wire(self.verifying().as_bytes())
    }

    /// Sign an arbitrary byte buffer (typically a challenge nonce).
    #[must_use]
    pub fn sign(&self, msg: &[u8]) -> [u8; SIGNATURE_LEN] {
        self.signing.sign(msg).to_bytes()
    }
}

/// Parse a stored `ed25519:<base64>` string into the underlying public key.
///
/// # Errors
///
/// Returns [`KeyError::Format`] if the prefix is missing,
/// [`KeyError::Base64`] if the base64 body is malformed, or
/// [`KeyError::Length`] if the decoded bytes are not 32 long.
pub fn parse_public_key_wire(s: &str) -> Result<VerifyingKey, KeyError> {
    let body = s
        .strip_prefix(ED25519_WIRE_PREFIX)
        .ok_or_else(|| KeyError::Format(format!("missing `{ED25519_WIRE_PREFIX}` prefix")))?;
    let bytes = STANDARD_NO_PAD
        .decode(body.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(body))
        .map_err(|e| KeyError::Base64(e.to_string()))?;
    if bytes.len() != PUBLIC_KEY_LEN {
        return Err(KeyError::Length {
            expected: PUBLIC_KEY_LEN,
            got: bytes.len(),
        });
    }
    let mut arr = [0u8; PUBLIC_KEY_LEN];
    arr.copy_from_slice(&bytes);
    VerifyingKey::from_bytes(&arr).map_err(|e| KeyError::Format(e.to_string()))
}

/// Encode a 32-byte verifying key as `ed25519:<base64>`. The padding-less
/// alphabet keeps the wire form compact and unambiguous.
#[must_use]
pub fn encode_public_wire(bytes: &[u8; PUBLIC_KEY_LEN]) -> String {
    format!("{ED25519_WIRE_PREFIX}{}", STANDARD_NO_PAD.encode(bytes))
}

/// Verify a signature against the public key + message.
///
/// # Errors
///
/// Returns [`KeyError::BadSignature`] when the signature does not match the
/// message under the supplied key. This is the canonical "wrong key / wrong
/// signature / wrong message" failure mode.
pub fn verify_signature(
    public: &VerifyingKey,
    msg: &[u8],
    signature: &[u8; SIGNATURE_LEN],
) -> Result<(), KeyError> {
    let sig = Signature::from_bytes(signature);
    public.verify(msg, &sig).map_err(|_| KeyError::BadSignature)
}

/// All the ways an Ed25519 operation can fail.
#[derive(Debug, Error)]
pub enum KeyError {
    /// Wire-format problem (missing prefix, bad encoding key).
    #[error("invalid public key format: {0}")]
    Format(String),
    /// Base64 decoding failed.
    #[error("invalid base64 in public key: {0}")]
    Base64(String),
    /// Decoded bytes had the wrong length.
    #[error("public key has {got} bytes (expected {expected})")]
    Length {
        /// Expected byte count.
        expected: usize,
        /// What we got.
        got: usize,
    },
    /// Signature did not verify under the supplied public key + message.
    #[error("signature verification failed")]
    BadSignature,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_sign_and_verify() {
        let kp = Keypair::generate();
        let msg = b"the quick brown fox jumps over the lazy dog";
        let sig = kp.sign(msg);
        verify_signature(&kp.verifying(), msg, &sig).expect("signature should verify");
    }

    #[test]
    fn verify_rejects_wrong_message() {
        let kp = Keypair::generate();
        let sig = kp.sign(b"original message");
        let err = verify_signature(&kp.verifying(), b"tampered message", &sig).unwrap_err();
        assert!(matches!(err, KeyError::BadSignature));
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let kp = Keypair::generate();
        let other = Keypair::generate();
        let sig = kp.sign(b"msg");
        let err = verify_signature(&other.verifying(), b"msg", &sig).unwrap_err();
        assert!(matches!(err, KeyError::BadSignature));
    }

    #[test]
    fn public_wire_round_trips() {
        let kp = Keypair::generate();
        let wire = kp.public_wire();
        assert!(wire.starts_with(ED25519_WIRE_PREFIX));
        let parsed = parse_public_key_wire(&wire).expect("round-trip parse");
        assert_eq!(parsed.to_bytes(), kp.verifying().to_bytes());
    }

    #[test]
    fn parse_rejects_missing_prefix() {
        let err = parse_public_key_wire("AAAA").unwrap_err();
        assert!(matches!(err, KeyError::Format(_)));
    }

    #[test]
    fn parse_rejects_wrong_length() {
        // Three bytes of base64 decoded is 2 bytes — not 32.
        let err = parse_public_key_wire("ed25519:AAA").unwrap_err();
        assert!(matches!(err, KeyError::Length { .. }));
    }

    #[test]
    fn parse_rejects_invalid_base64() {
        let err = parse_public_key_wire("ed25519:!!!").unwrap_err();
        assert!(matches!(err, KeyError::Base64(_)));
    }

    #[test]
    fn from_signing_bytes_round_trips() {
        let original = Keypair::generate();
        let seed = original.signing_bytes();
        let restored = Keypair::from_signing_bytes(seed);
        assert_eq!(
            original.verifying().to_bytes(),
            restored.verifying().to_bytes(),
            "same seed -> same public key",
        );
        let msg = b"hello";
        let sig = restored.sign(msg);
        verify_signature(&original.verifying(), msg, &sig).expect("cross-verify");
    }

    #[test]
    fn parse_accepts_padded_base64() {
        // Generate a real public key (must decompress to a valid Edwards
        // point — random bytes won't), encode with padding, and re-parse.
        let kp = Keypair::generate();
        let bytes = kp.verifying().to_bytes();
        let padded =
            format!("ed25519:{}", base64::engine::general_purpose::STANDARD.encode(bytes),);
        let parsed = parse_public_key_wire(&padded).unwrap();
        assert_eq!(parsed.to_bytes(), bytes);
    }
}

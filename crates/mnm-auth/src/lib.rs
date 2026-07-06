//! `mnm-auth` — Ed25519 challenge-response, HS256 JWT mint/verify, user-store
//! loading, and (Phase 7b) GitHub OAuth.
//!
//! Phase 7a (this revision) lands the primitives:
//!
//! - [`role`] — `Role` (admin / writer) and `Tier` (admin / read_uplift).
//! - [`user`] — `UserStore` loader (FR-057): strict TOML parse with
//!   `deny_unknown_fields`, schema-version gate, duplicate-id rejection.
//! - [`keypair`] — Ed25519 keypair generation, sign / verify, and the
//!   `ed25519:<base64>` wire form used by `users.toml`.
//! - [`jwt`] — HS256 mint + verify with typed errors for expiry / signature
//!   / malformed (FR-058, FR-117).
//! - [`challenge`] — single-use nonces backing the challenge-response flow
//!   (FR-056); in-memory `ChallengeStore` with a 60s TTL cap.
//!
//! Phase 7b will land the HTTP endpoints (`/v1/auth/{challenge,verify}`,
//! `/v1/auth/github/{start,callback}`), the bearer-extraction axum
//! middleware, and the CLI commands (`mnm keys generate`, `mnm login`,
//! `mnm auth github`, `mnm users *`).

#![doc(html_root_url = "https://docs.rs/mnm-auth/0.1.0")]
#![allow(clippy::doc_markdown)]

pub mod challenge;
pub mod jwt;
pub mod keypair;
pub mod oauth_state;
pub mod role;
pub mod user;

pub use challenge::{Challenge, ChallengeError, ChallengeStore, MAX_TTL as CHALLENGE_MAX_TTL};
pub use jwt::{
    mint as mint_jwt, verify as verify_jwt, Claims, JwtError, SigningSecret, DEFAULT_ADMIN_TTL,
    DEFAULT_READ_UPLIFT_TTL,
};
pub use keypair::{
    encode_public_wire, parse_public_key_wire, verify_signature, KeyError, Keypair,
    ED25519_WIRE_PREFIX, PUBLIC_KEY_LEN, SIGNATURE_LEN, SIGNING_KEY_LEN,
};
pub use oauth_state::{
    generate_cli_nonce, OAuthState, OAuthStateError, OAuthStateStore,
    DEFAULT_TTL as OAUTH_STATE_DEFAULT_TTL, MAX_CLI_STATE_LEN as OAUTH_STATE_MAX_CLI_STATE_LEN,
    MAX_TTL as OAUTH_STATE_MAX_TTL,
};
pub use role::{Role, Tier};
pub use user::{User, UserStore, UserStoreError, SCHEMA_VERSION as USER_STORE_SCHEMA_VERSION};

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

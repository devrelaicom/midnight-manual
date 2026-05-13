//! `mn-auth` — Ed25519 challenge-response, JWTs, GitHub OAuth, and user-store loading.
//!
//! Phase-7 (US9) lands the full implementation: Ed25519 keys, HS256 JWT mint/verify
//! (1h TTL), GitHub OAuth web + device flow with org-membership verification, and the
//! TOML user-store loader. See [`specs/001-rag-platform/tasks.md`](../../../specs/001-rag-platform/tasks.md).

#![doc(html_root_url = "https://docs.rs/mn-auth/0.1.0")]

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

//! User store — the `users.toml` file that maps `user_id → public_key + role`.
//!
//! The store is loaded once at server startup (FR-057) from
//! `MIDNIGHT_MANUAL_USER_STORE` and held in memory; the server NEVER mutates
//! it at runtime (D20). Mutations are admin-CLI operations against the local
//! file followed by a deploy.
//!
//! Wire shape (mirrored from spec.md §Story 9):
//!
//! ```toml
//! schema_version = 1
//!
//! [[users]]
//! user_id    = "aaron"
//! role       = "admin"
//! public_key = "ed25519:base64..."
//! created_at = "2026-05-13"
//! note       = "founding admin"
//! ```
//!
//! Unknown fields are rejected at parse time (FR-057's "fail-fast at
//! startup"); duplicate `user_id` values are rejected at load time.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::role::Role;

/// Canonical schema version. A user-store file MUST carry
/// `schema_version = 1`; a higher value is rejected by the loader, while a
/// lower value would be migrated by a future migrator (none in v1).
pub const SCHEMA_VERSION: u32 = 1;

/// The full user-store contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserStore {
    /// Schema version sentinel.
    pub schema_version: u32,
    /// Indexed by `user_id` for O(1) lookup on auth.
    pub users: HashMap<String, User>,
}

/// One row in `users.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// Stable id — the JWT `sub` claim.
    pub user_id: String,
    /// Role gate.
    pub role: Role,
    /// `ed25519:base64...` wire form. Validated at load.
    pub public_key: String,
    /// ISO-8601 date when this user was added (used for audit displays only).
    pub created_at: String,
    /// Optional human note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Wire shape — the on-disk TOML format. Deliberately separate from
/// [`UserStore`] so we can validate / index after parsing without complicating
/// the public type.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserStoreWire {
    schema_version: u32,
    #[serde(default)]
    users: Vec<UserWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserWire {
    user_id: String,
    role: Role,
    public_key: String,
    created_at: String,
    #[serde(default)]
    note: Option<String>,
}

impl UserStore {
    /// Parse a user-store from an in-memory TOML body.
    ///
    /// # Errors
    ///
    /// Returns [`UserStoreError::Parse`] on malformed TOML or unknown fields
    /// (FR-057); [`UserStoreError::SchemaVersionMismatch`] if the file's
    /// `schema_version` is newer than [`SCHEMA_VERSION`];
    /// [`UserStoreError::DuplicateUser`] when two rows share a `user_id`;
    /// [`UserStoreError::InvalidPublicKey`] when a `public_key` doesn't match
    /// the `ed25519:base64...` wire form.
    pub fn parse(body: &str) -> Result<Self, UserStoreError> {
        let wire: UserStoreWire =
            toml::from_str(body).map_err(|e| UserStoreError::Parse(e.to_string()))?;

        if wire.schema_version > SCHEMA_VERSION {
            return Err(UserStoreError::SchemaVersionMismatch {
                found: wire.schema_version,
                supported: SCHEMA_VERSION,
            });
        }

        let mut users: HashMap<String, User> = HashMap::with_capacity(wire.users.len());
        for u in wire.users {
            // The full key validation lives in `keypair::parse_public_key_wire`;
            // here we only sanity-check the prefix so a typo in users.toml
            // surfaces at startup rather than at first-login time.
            if !u.public_key.starts_with("ed25519:") {
                return Err(UserStoreError::InvalidPublicKey {
                    user_id: u.user_id,
                    reason: "public_key must start with `ed25519:`".to_owned(),
                });
            }
            let key = u.user_id.clone();
            let user = User {
                user_id: u.user_id,
                role: u.role,
                public_key: u.public_key,
                created_at: u.created_at,
                note: u.note,
            };
            if users.insert(key.clone(), user).is_some() {
                return Err(UserStoreError::DuplicateUser(key));
            }
        }

        Ok(Self {
            schema_version: wire.schema_version,
            users,
        })
    }

    /// Read and parse a user-store file from disk.
    ///
    /// # Errors
    ///
    /// Returns [`UserStoreError::Io`] on filesystem failure, plus any of the
    /// variants returned by [`UserStore::parse`].
    pub fn load(path: &Path) -> Result<Self, UserStoreError> {
        let body = std::fs::read_to_string(path).map_err(|e| UserStoreError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        Self::parse(&body)
    }

    /// Look up a user by id.
    #[must_use]
    pub fn get(&self, user_id: &str) -> Option<&User> {
        self.users.get(user_id)
    }

    /// Iterate users in arbitrary order.
    pub fn iter(&self) -> impl Iterator<Item = &User> {
        self.users.values()
    }

    /// Total user count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.users.len()
    }

    /// True when the store has no users.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }
}

/// All the ways user-store loading can fail.
#[derive(Debug, Error)]
pub enum UserStoreError {
    /// I/O failure reading the on-disk file.
    #[error("read user store `{path}`: {message}")]
    Io {
        /// Path that failed to read.
        path: String,
        /// Underlying io error message.
        message: String,
    },
    /// Malformed TOML or unknown field (FR-057's strict mode).
    #[error("parse user store: {0}")]
    Parse(String),
    /// File's `schema_version` is newer than this binary supports.
    #[error(
        "user store schema_version={found} is newer than supported (max {supported}). Upgrade the binary."
    )]
    SchemaVersionMismatch {
        /// Version we read.
        found: u32,
        /// Highest version we support.
        supported: u32,
    },
    /// Two rows shared a `user_id`.
    #[error("duplicate user_id in user store: `{0}`")]
    DuplicateUser(String),
    /// A `public_key` value is malformed.
    #[error("invalid public_key for user `{user_id}`: {reason}")]
    InvalidPublicKey {
        /// The offending user_id.
        user_id: String,
        /// Why it failed validation.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_store() {
        let body = r#"
schema_version = 1

[[users]]
user_id = "aaron"
role = "admin"
public_key = "ed25519:AAAA"
created_at = "2026-05-13"
note = "founding admin"
"#;
        let s = UserStore::parse(body).unwrap();
        assert_eq!(s.schema_version, 1);
        assert_eq!(s.len(), 1);
        let u = s.get("aaron").unwrap();
        assert_eq!(u.role, Role::Admin);
        assert_eq!(u.public_key, "ed25519:AAAA");
        assert_eq!(u.note.as_deref(), Some("founding admin"));
    }

    #[test]
    fn parses_multiple_users_with_distinct_roles() {
        let body = r#"
schema_version = 1

[[users]]
user_id = "aaron"
role = "admin"
public_key = "ed25519:A"
created_at = "2026-05-13"

[[users]]
user_id = "ci-bot"
role = "writer"
public_key = "ed25519:B"
created_at = "2026-05-14"
"#;
        let s = UserStore::parse(body).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.get("aaron").unwrap().role, Role::Admin);
        assert_eq!(s.get("ci-bot").unwrap().role, Role::Writer);
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let body = r#"
schema_version = 1
mystery = "field"

[[users]]
user_id = "aaron"
role = "admin"
public_key = "ed25519:A"
created_at = "2026-05-13"
"#;
        let err = UserStore::parse(body).unwrap_err();
        assert!(matches!(err, UserStoreError::Parse(_)));
    }

    #[test]
    fn rejects_unknown_user_field() {
        let body = r#"
schema_version = 1

[[users]]
user_id = "aaron"
role = "admin"
public_key = "ed25519:A"
created_at = "2026-05-13"
mystery = "field"
"#;
        let err = UserStore::parse(body).unwrap_err();
        assert!(matches!(err, UserStoreError::Parse(_)));
    }

    #[test]
    fn rejects_schema_version_in_the_future() {
        let body = "schema_version = 99\n";
        let err = UserStore::parse(body).unwrap_err();
        assert!(matches!(err, UserStoreError::SchemaVersionMismatch { found: 99, supported: 1 }));
    }

    #[test]
    fn rejects_duplicate_user_id() {
        let body = r#"
schema_version = 1

[[users]]
user_id = "aaron"
role = "admin"
public_key = "ed25519:A"
created_at = "2026-05-13"

[[users]]
user_id = "aaron"
role = "writer"
public_key = "ed25519:B"
created_at = "2026-05-14"
"#;
        let err = UserStore::parse(body).unwrap_err();
        assert!(matches!(err, UserStoreError::DuplicateUser(u) if u == "aaron"));
    }

    #[test]
    fn rejects_public_key_without_ed25519_prefix() {
        let body = r#"
schema_version = 1

[[users]]
user_id = "aaron"
role = "admin"
public_key = "rsa:nope"
created_at = "2026-05-13"
"#;
        let err = UserStore::parse(body).unwrap_err();
        assert!(matches!(
            err,
            UserStoreError::InvalidPublicKey { user_id, .. } if user_id == "aaron"
        ));
    }

    #[test]
    fn empty_store_is_valid() {
        let s = UserStore::parse("schema_version = 1\n").unwrap();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn unknown_role_is_rejected() {
        let body = r#"
schema_version = 1

[[users]]
user_id = "aaron"
role = "supreme"
public_key = "ed25519:A"
created_at = "2026-05-13"
"#;
        let err = UserStore::parse(body).unwrap_err();
        assert!(matches!(err, UserStoreError::Parse(_)));
    }
}

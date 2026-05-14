//! Caller roles.
//!
//! Two roles ship in v1:
//! - `admin` — full surface, including `/v1/admin/*`.
//! - `writer` — ingest endpoints only; cannot mutate the rate-limit registry.
//!
//! The `tier` field on a minted JWT distinguishes admin-tier tokens (from the
//! Ed25519 challenge-response flow) from `read_uplift` tokens (from the GitHub
//! OAuth flow per FR-117). A read-uplift token MUST never satisfy a write
//! endpoint's role check (FR-062 / FR-117) — the tier guard runs before any
//! role guard so a stray field can't escalate.

use serde::{Deserialize, Serialize};

/// User role. Stored in the user-store TOML and copied into the JWT's `role`
/// claim on mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// `/v1/admin/*` + writes + reads.
    Admin,
    /// Writes (ingest) + reads. No `/v1/admin/*`.
    Writer,
}

impl Role {
    /// Whether this role can call `/v1/admin/*`.
    #[must_use]
    pub const fn can_admin(self) -> bool {
        matches!(self, Self::Admin)
    }

    /// Whether this role can perform ingest writes.
    #[must_use]
    pub const fn can_write(self) -> bool {
        matches!(self, Self::Admin | Self::Writer)
    }

    /// Stable wire string.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Writer => "writer",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// Privilege tier. A JWT carries this in addition to `role` so write endpoints
/// can reject GitHub-minted read-uplift tokens *before* consulting the role
/// claim — per FR-117 the tier check runs first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Admin / writer JWT minted via Ed25519 challenge-response.
    Admin,
    /// 30-day bearer minted via GitHub OAuth — grants rate-limit uplift only.
    ReadUplift,
}

impl Tier {
    /// Whether this tier may reach write endpoints.
    #[must_use]
    pub const fn can_write(self) -> bool {
        matches!(self, Self::Admin)
    }

    /// Stable wire string.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::ReadUplift => "read_uplift",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_wire_strings_match_spec() {
        assert_eq!(Role::Admin.as_wire(), "admin");
        assert_eq!(Role::Writer.as_wire(), "writer");
    }

    #[test]
    fn role_permission_matrix() {
        assert!(Role::Admin.can_admin());
        assert!(Role::Admin.can_write());
        assert!(!Role::Writer.can_admin());
        assert!(Role::Writer.can_write());
    }

    #[test]
    fn tier_admin_can_write_read_uplift_cannot() {
        assert!(Tier::Admin.can_write());
        assert!(!Tier::ReadUplift.can_write());
    }

    #[test]
    fn role_serialises_lowercase() {
        let v = serde_json::to_value(Role::Admin).unwrap();
        assert_eq!(v, serde_json::Value::String("admin".into()));
        let v = serde_json::to_value(Role::Writer).unwrap();
        assert_eq!(v, serde_json::Value::String("writer".into()));
    }

    #[test]
    fn tier_serialises_with_underscore() {
        let v = serde_json::to_value(Tier::ReadUplift).unwrap();
        assert_eq!(v, serde_json::Value::String("read_uplift".into()));
    }
}

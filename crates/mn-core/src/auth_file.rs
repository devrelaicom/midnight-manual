//! `auth.toml` reader (Phase-2 stub, expanded in Phase 7 with the writer).
//!
//! The token file lives at `$XDG_CONFIG_HOME/midnight-manual/auth.toml` with
//! `chmod 0600` on the private half (see D28). Phase-2 only needs the read path
//! so the MCP server can resolve a read-uplift bearer at startup. Phase 7's
//! `mnm login` and `mnm auth github` commands land the writer + rotation logic.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

/// Canonical schema version. The file MUST carry `schema_version = 1`; a
/// mismatch fails the load with [`AuthFileError::SchemaVersionMismatch`].
pub const SCHEMA_VERSION: u32 = 1;

/// Full auth.toml shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthFile {
    /// Schema version sentinel. Always `1` in v1.
    pub schema_version: u32,
    /// Admin section — set by `mnm login`. Hidden from MCP server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin: Option<AdminSection>,
    /// Read-uplift section — set by `mnm auth github`. Used by MCP + CLI reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_uplift: Option<ReadUpliftSection>,
}

/// `[admin]` — admin-mode JWT (1h TTL, D21).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSection {
    /// Logged-in user id (sub claim).
    pub user_id: String,
    /// HS256 JWT.
    pub token: String,
    /// When the token expires.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// `[read_uplift]` — GitHub-OAuth-minted read-tier bearer (30d default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadUpliftSection {
    /// GitHub login that authenticated.
    pub github_login: String,
    /// Opaque bearer token.
    pub token: String,
    /// When the token expires.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

impl AuthFile {
    /// Read and validate an auth.toml from `path`.
    ///
    /// Returns:
    /// - `Ok(Some(AuthFile))` on a present, well-formed file with matching schema.
    /// - `Ok(None)` when the file is absent (anonymous mode — EC-39, FR-070).
    /// - `Err(...)` on a present but malformed or schema-mismatched file.
    ///
    /// # Errors
    ///
    /// Returns [`AuthFileError::Io`] for I/O failures other than "not found",
    /// [`AuthFileError::Parse`] for malformed TOML, and
    /// [`AuthFileError::SchemaVersionMismatch`] if `schema_version` is not
    /// [`SCHEMA_VERSION`].
    pub fn read_optional(path: &Path) -> Result<Option<Self>, AuthFileError> {
        match std::fs::metadata(path) {
            Ok(md) => {
                check_permissions(path, &md)?;
                let body = std::fs::read_to_string(path).map_err(|e| AuthFileError::Io {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })?;
                let file: Self = toml::from_str(&body).map_err(|e| AuthFileError::Parse {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })?;
                if file.schema_version != SCHEMA_VERSION {
                    return Err(AuthFileError::SchemaVersionMismatch {
                        path: path.to_path_buf(),
                        found: file.schema_version,
                        expected: SCHEMA_VERSION,
                    });
                }
                Ok(Some(file))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AuthFileError::Io {
                path: path.to_path_buf(),
                message: e.to_string(),
            }),
        }
    }

    /// Returns the admin JWT only if present and not expired (`now < expires_at`).
    #[must_use]
    pub fn active_admin_token(&self, now: OffsetDateTime) -> Option<&str> {
        self.admin
            .as_ref()
            .filter(|a| now < a.expires_at)
            .map(|a| a.token.as_str())
    }

    /// Returns the read-uplift bearer only if present and not expired.
    #[must_use]
    pub fn active_read_uplift_token(&self, now: OffsetDateTime) -> Option<&str> {
        self.read_uplift
            .as_ref()
            .filter(|r| now < r.expires_at)
            .map(|r| r.token.as_str())
    }
}

/// All the ways auth.toml loading can fail.
#[derive(Debug, Error)]
pub enum AuthFileError {
    /// I/O failure (excluding `NotFound`, which is rendered as `Ok(None)`).
    #[error("failed to read auth file `{}`: {message}", path.display())]
    Io {
        /// File path that failed to read.
        path: PathBuf,
        /// Underlying I/O error message.
        message: String,
    },
    /// TOML parse failure on the resolved file.
    #[error("failed to parse auth file `{}`: {message}", path.display())]
    Parse {
        /// File path that failed to parse.
        path: PathBuf,
        /// Underlying parser error message.
        message: String,
    },
    /// `schema_version` does not match [`SCHEMA_VERSION`].
    #[error(
        "auth file `{}` has schema_version={found}; expected {expected}. Re-run `mnm login` to refresh.",
        .path.display()
    )]
    SchemaVersionMismatch {
        /// The file path that failed validation.
        path: PathBuf,
        /// The schema version we found.
        found: u32,
        /// The version we expected.
        expected: u32,
    },
    /// File permissions are too permissive (group- or world-readable). Bearer
    /// tokens MUST be `chmod 0600` per D28; we refuse to load any wider mode
    /// so a leaked file (e.g. copied into a shared dir, baked into an image)
    /// does not silently authenticate as the user.
    #[error(
        "auth file `{}` has insecure permissions ({mode:#o}); expected 0o600. Run `chmod 600 \"{}\"` and retry.",
        path.display(), path.display()
    )]
    InsecurePermissions {
        /// The file path that failed the permission check.
        path: PathBuf,
        /// The mode bits we observed.
        mode: u32,
    },
}

#[cfg(unix)]
fn check_permissions(path: &Path, md: &std::fs::Metadata) -> Result<(), AuthFileError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = md.permissions().mode() & 0o777;
    // Any group / world bits set is a refusal.
    if mode & 0o077 != 0 {
        return Err(AuthFileError::InsecurePermissions { path: path.to_path_buf(), mode });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_path: &Path, _md: &std::fs::Metadata) -> Result<(), AuthFileError> {
    // Windows / WASI: rely on the user's NTFS ACLs or platform-equivalent.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tempfile(body: &str) -> tempfile::NamedTempFile {
        use std::io::Write as _;
        let mut f = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("create tempfile");
        f.write_all(body.as_bytes()).expect("write tempfile");
        f
    }

    #[test]
    fn absent_file_returns_none() {
        let path = std::path::PathBuf::from("/definitely/does/not/exist/auth.toml");
        let r = AuthFile::read_optional(&path).expect("ok");
        assert!(r.is_none());
    }

    #[test]
    fn empty_sections_load_ok() {
        let body = "schema_version = 1\n";
        let f = write_tempfile(body);
        let r = AuthFile::read_optional(f.path()).unwrap().unwrap();
        assert_eq!(r.schema_version, 1);
        assert!(r.admin.is_none());
        assert!(r.read_uplift.is_none());
    }

    #[test]
    fn schema_mismatch_fails() {
        let body = "schema_version = 2\n";
        let f = write_tempfile(body);
        let err = AuthFile::read_optional(f.path()).unwrap_err();
        assert!(matches!(
            err,
            AuthFileError::SchemaVersionMismatch { found: 2, expected: 1, .. },
        ));
    }

    #[test]
    fn admin_active_window_respected() {
        let body = r#"
schema_version = 1

[admin]
user_id = "aaron"
token = "jwt-abc"
expires_at = "2026-05-13T15:30:00Z"
"#;
        let f = write_tempfile(body);
        let r = AuthFile::read_optional(f.path()).unwrap().unwrap();
        let now_before = OffsetDateTime::parse(
            "2026-05-13T15:29:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let now_after = OffsetDateTime::parse(
            "2026-05-13T15:31:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert_eq!(r.active_admin_token(now_before), Some("jwt-abc"));
        assert_eq!(r.active_admin_token(now_after), None);
    }

    #[test]
    fn malformed_toml_returns_parse() {
        let f = write_tempfile("definitely := not = toml\n");
        let err = AuthFile::read_optional(f.path()).unwrap_err();
        assert!(matches!(err, AuthFileError::Parse { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_group_readable_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let f = write_tempfile("schema_version = 1\n");
        // tempfile defaults to 0600 — widen to 0640 (group-readable) to
        // simulate an exported / leaked file.
        std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o640))
            .expect("chmod tempfile");
        let err = AuthFile::read_optional(f.path()).unwrap_err();
        assert!(
            matches!(err, AuthFileError::InsecurePermissions { mode: 0o640, .. }),
            "expected InsecurePermissions, got {err:?}"
        );
    }
}

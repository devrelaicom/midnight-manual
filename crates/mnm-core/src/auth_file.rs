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

    /// Serialize this auth file to a TOML body suitable for writing to disk.
    /// Section order is `schema_version`, `[admin]`, `[read_uplift]` —
    /// matching what `mnm login` / `mnm auth github` would have produced.
    ///
    /// # Errors
    ///
    /// Returns [`AuthFileError::Serialize`] on the (effectively never)
    /// serialiser failure.
    pub fn to_toml(&self) -> Result<String, AuthFileError> {
        toml::to_string(self).map_err(|e| AuthFileError::Serialize(e.to_string()))
    }

    /// Atomically write the auth file to `path`, creating the file with
    /// mode `0o600` (Unix) and refusing to write if an existing file has
    /// looser permissions.
    ///
    /// Atomicity comes from a `path.tmp` sibling that is `rename(2)`'d into
    /// place; concurrent readers will see either the previous file or the
    /// new file but never a partial mid-write.
    ///
    /// # Errors
    ///
    /// Returns [`AuthFileError::Io`] on filesystem failure,
    /// [`AuthFileError::Serialize`] on the (rare) TOML encode failure, or
    /// [`AuthFileError::InsecurePermissions`] if an existing file at `path`
    /// already has group- or world-readable bits set (we refuse to silently
    /// re-narrow them).
    pub fn write(&self, path: &Path) -> Result<(), AuthFileError> {
        // Refuse to clobber a world-readable file — the operator likely
        // didn't intend us to inherit those permissions, and our subsequent
        // chmod could race with another reader.
        if let Ok(md) = std::fs::metadata(path) {
            check_permissions(path, &md)?;
        }
        let body = self.to_toml()?;
        atomic_write(path, &body)?;
        Ok(())
    }

    /// Read the existing file (or build a fresh empty one), update the
    /// `[admin]` section with the supplied JWT + expiry, and persist back to
    /// disk under the same permission discipline as [`AuthFile::write`].
    ///
    /// # Errors
    ///
    /// Returns any variant of [`AuthFileError`] that
    /// [`AuthFile::read_optional`] or [`AuthFile::write`] can produce.
    pub fn write_admin_token(
        path: &Path,
        user_id: impl Into<String>,
        token: impl Into<String>,
        expires_at: OffsetDateTime,
    ) -> Result<(), AuthFileError> {
        let mut file = Self::read_optional(path)?.unwrap_or_else(Self::empty);
        file.admin = Some(AdminSection {
            user_id: user_id.into(),
            token: token.into(),
            expires_at,
        });
        file.write(path)
    }

    /// Like [`AuthFile::write_admin_token`] but for the `[read_uplift]`
    /// section, used by `mnm auth github`.
    ///
    /// # Errors
    ///
    /// See [`AuthFile::write_admin_token`].
    pub fn write_read_uplift_token(
        path: &Path,
        github_login: impl Into<String>,
        token: impl Into<String>,
        expires_at: OffsetDateTime,
    ) -> Result<(), AuthFileError> {
        let mut file = Self::read_optional(path)?.unwrap_or_else(Self::empty);
        file.read_uplift = Some(ReadUpliftSection {
            github_login: github_login.into(),
            token: token.into(),
            expires_at,
        });
        file.write(path)
    }

    /// Fresh schema-versioned auth file with no sections populated.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            admin: None,
            read_uplift: None,
        }
    }
}

/// Write `body` to `path` via a tmp-then-rename dance so the file is
/// atomically replaced and never observed half-written. On Unix the file
/// is created with mode `0o600`.
fn atomic_write(path: &Path, body: &str) -> Result<(), AuthFileError> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| AuthFileError::Io {
                path: parent.to_path_buf(),
                message: e.to_string(),
            })?;
        }
    }
    let tmp = path.with_extension("toml.tmp");
    let file_handle = open_restricted(&tmp)?;
    {
        let mut bw = std::io::BufWriter::new(file_handle);
        bw.write_all(body.as_bytes())
            .map_err(|e| AuthFileError::Io {
                path: tmp.clone(),
                message: e.to_string(),
            })?;
        bw.flush().map_err(|e| AuthFileError::Io {
            path: tmp.clone(),
            message: e.to_string(),
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|e| AuthFileError::Io {
        path: path.to_path_buf(),
        message: format!("rename {} -> {}: {e}", tmp.display(), path.display()),
    })?;
    Ok(())
}

#[cfg(unix)]
fn open_restricted(path: &Path) -> Result<std::fs::File, AuthFileError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| AuthFileError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
}

#[cfg(not(unix))]
fn open_restricted(path: &Path) -> Result<std::fs::File, AuthFileError> {
    // Platform permission model is the user's NTFS ACL / equivalent.
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|e| AuthFileError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
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
    /// TOML serialization failure on a write.
    #[error("failed to serialize auth file: {0}")]
    Serialize(String),
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

    fn rfc(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).unwrap()
    }

    #[test]
    fn writes_then_reads_admin_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");
        let exp = rfc("2026-05-14T01:30:00Z");
        AuthFile::write_admin_token(&path, "aaron", "jwt-xyz", exp).expect("write");

        let loaded = AuthFile::read_optional(&path).unwrap().unwrap();
        let admin = loaded.admin.unwrap();
        assert_eq!(admin.user_id, "aaron");
        assert_eq!(admin.token, "jwt-xyz");
        assert_eq!(admin.expires_at, exp);
        assert!(loaded.read_uplift.is_none(), "writer must not invent unrelated sections");
    }

    #[test]
    fn writes_then_reads_read_uplift_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");
        let exp = rfc("2026-06-14T00:00:00Z");
        AuthFile::write_read_uplift_token(&path, "aaronbassett", "ru_abc", exp).expect("write");

        let loaded = AuthFile::read_optional(&path).unwrap().unwrap();
        let r = loaded.read_uplift.unwrap();
        assert_eq!(r.github_login, "aaronbassett");
        assert_eq!(r.token, "ru_abc");
        assert_eq!(r.expires_at, exp);
    }

    #[test]
    fn writing_admin_does_not_clobber_read_uplift() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");
        let ru_exp = rfc("2026-06-14T00:00:00Z");
        AuthFile::write_read_uplift_token(&path, "aaronbassett", "ru_abc", ru_exp).unwrap();
        let admin_exp = rfc("2026-05-14T01:30:00Z");
        AuthFile::write_admin_token(&path, "aaron", "jwt-xyz", admin_exp).unwrap();

        let loaded = AuthFile::read_optional(&path).unwrap().unwrap();
        assert_eq!(loaded.read_uplift.unwrap().token, "ru_abc");
        assert_eq!(loaded.admin.unwrap().token, "jwt-xyz");
    }

    #[cfg(unix)]
    #[test]
    fn writer_creates_file_with_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");
        AuthFile::write_admin_token(&path, "aaron", "jwt", rfc("2026-05-14T01:30:00Z")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "writer must create the file with 0o600");
    }

    #[cfg(unix)]
    #[test]
    fn writer_refuses_insecure_existing_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");
        // Seed an existing world-readable file and confirm we refuse to
        // overwrite it (we'd inherit / fight its perms otherwise).
        std::fs::write(&path, "schema_version = 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = AuthFile::write_admin_token(&path, "aaron", "jwt", rfc("2026-05-14T01:30:00Z"))
            .unwrap_err();
        assert!(
            matches!(err, AuthFileError::InsecurePermissions { mode: 0o644, .. }),
            "expected InsecurePermissions, got {err:?}"
        );
    }
}

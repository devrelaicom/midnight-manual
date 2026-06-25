//! Shared helpers used across multiple subcommands.
//!
//! Hoists the previously-duplicated server-URL resolver out of
//! `commands/sources.rs` so `login`, `users`, and any future write-side
//! command can share the same precedence walk.

use mnm_core::error::Error as CoreError;
use serde::Deserialize;
use time::OffsetDateTime;

/// Deserialize-only mirror of the server's HTTP error envelope
/// (`midnight_manual_server::error::ErrorBody`):
/// `{ error: { code, message, remediation, context }, request_id }`.
///
/// The `error` object is the wire form of [`mnm_core::error::Error`], which
/// already derives `Deserialize`; this wrapper exists only so CLI commands can
/// pull the typed envelope out of a non-2xx body without depending on the
/// server crate. `request_id` is accepted but currently unused by the CLI's
/// surfaced messages.
#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    /// The typed error.
    error: CoreError,
}

/// Attempt to decode a non-2xx HTTP response body into the server's typed
/// [`mnm_core::error::Error`] envelope.
///
/// Returns `Some(error)` when `body` is the `{ error: { … }, request_id }`
/// envelope the server emits for every error response, or `None` when the body
/// is not a recognizable envelope (e.g. a proxy's plain-text 502). Callers fall
/// back to echoing the raw (redacted) body in the `None` case.
///
/// Reusable across CLI commands: any caller holding a raw error body can pull
/// the typed `code` / `message` / `remediation` out of it to surface a
/// remediation rather than dumping raw JSON — parity with the MCP cloud client,
/// which decodes the same envelope (`mnm_mcp::cloud_client::parse_mismatch`).
#[must_use]
pub fn decode_error_envelope(body: &str) -> Option<CoreError> {
    serde_json::from_str::<ErrorEnvelope>(body)
        .ok()
        .map(|e| e.error)
}

/// Resolve the cloud server URL.
///
/// Precedence (D17):
///
/// 1. The explicit `--server` flag (or `MIDNIGHT_MANUAL_SERVER` env, surfaced
///    by clap on the global flag).
///
/// 2. The resolved config's `[server].url`.
///
/// 3. The compiled-in default
///    (`<https://midnight-manual.midnightntwrk.expert>`).
///
/// Trailing `/` is stripped so callers can safely format `format!("{url}/v1/...")`.
///
/// This is the discovery-performing convenience wrapper: it runs
/// [`Config::discover`](mnm_core::config::Config::discover) itself when no flag
/// is supplied. Callers that have *already* discovered a config should prefer
/// [`resolve_server_url_from`] so a single config read backs every derived
/// value (avoids a TOCTOU desync + a redundant file read).
#[must_use]
pub fn resolve_server_url(flag: Option<&str>) -> String {
    if let Some(s) = flag {
        return s.trim_end_matches('/').to_owned();
    }
    let env = mnm_core::config::StdEnv;
    // Best-effort: this convenience wrapper is only reached with no pre-discovered
    // cfg; the authoritative loud discover already ran in `cli::run`. Callers that
    // have a cfg should use `resolve_server_url_from` instead.
    let (cfg, _) = mnm_core::config::Config::discover(None, &env).unwrap_or_default();
    resolve_server_url_from(flag, &cfg)
}

/// Resolve the cloud server URL against an *already-discovered* config.
///
/// Same precedence as [`resolve_server_url`] (flag > `cfg.server.url`), but with
/// no I/O: the caller threads in the `cfg` it already read. This keeps a single
/// source of truth so e.g. `telemetry_enabled` and `cloud_url` cannot drift
/// apart across two separate `Config::discover` reads.
///
/// Trailing `/` is stripped so callers can safely format `format!("{url}/v1/...")`.
#[must_use]
pub fn resolve_server_url_from(flag: Option<&str>, cfg: &mnm_core::config::Config) -> String {
    flag.unwrap_or(&cfg.server.url)
        .trim_end_matches('/')
        .to_owned()
}

/// Look up the active read-uplift bearer in `$XDG_CONFIG_HOME/midnight-manual/auth.toml`.
/// Absent or expired tokens degrade silently to anonymous mode.
#[must_use]
pub fn resolve_read_uplift_token() -> Option<String> {
    let path = mnm_core::paths::auth_file_path(&mnm_core::config::StdEnv)?;
    let file = mnm_core::auth_file::AuthFile::read_optional(&path)
        .ok()
        .flatten()?;
    file.active_read_uplift_token(OffsetDateTime::now_utc())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_wins_and_strips_trailing_slash() {
        let url = resolve_server_url(Some("http://localhost:8080/"));
        assert_eq!(url, "http://localhost:8080");
    }

    #[test]
    fn flag_passthrough_when_no_trailing_slash() {
        let url = resolve_server_url(Some("http://localhost:8080"));
        assert_eq!(url, "http://localhost:8080");
    }

    /// The server's `{ error: { … }, request_id }` envelope decodes into the
    /// typed [`CoreError`], exposing `code` / `message` / `remediation`.
    #[test]
    fn decode_error_envelope_extracts_typed_error() {
        let body = serde_json::json!({
            "error": {
                "code": "embedding_model_mismatch",
                "message": "client model voyage-code-3@1 does not match corpus model voyage-code-3@2",
                "remediation": "re-run `mnm models pull` to fetch voyage-code-3@2",
                "context": { "corpus_model": "voyage-code-3@2", "client_model": "voyage-code-3@1" }
            },
            "request_id": "req-123"
        })
        .to_string();
        let err = decode_error_envelope(&body).expect("envelope must decode");
        assert_eq!(err.code, mnm_core::error::ErrorCode::EmbeddingModelMismatch);
        assert!(err.message.contains("does not match corpus model"));
        assert!(err.remediation.contains("models pull"));
    }

    /// A body that is not the typed envelope (e.g. a proxy's plain-text error)
    /// yields `None`, so callers fall back to the redacted raw form.
    #[test]
    fn decode_error_envelope_rejects_non_envelope_body() {
        assert!(decode_error_envelope("502 Bad Gateway").is_none());
        // Valid JSON but the wrong shape (no `error` object) → None.
        assert!(decode_error_envelope(r#"{"detail":"nope"}"#).is_none());
    }
}

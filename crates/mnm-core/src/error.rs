//! Typed error envelope used by every cloud HTTP response, MCP tool result,
//! and CLI exit-with-error path.
//!
//! Spec: FR-030, Constitution V. The wire shape is intentionally additive — new
//! `context` keys do NOT break old callers — and every variant ships a human-
//! readable `remediation` field telling the caller exactly what to do next.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Canonical machine-readable error codes.
///
/// New variants are additive (MINOR bump per Constitution X). Removing or
/// renaming a variant is a MAJOR contract break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // -- 4xx client-side --
    /// Generic 400 — payload failed validation. `context` should name the field.
    InvalidRequest,
    /// 400 — `queries.length > MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST` (D25, FR-088).
    MultiQueryLimitExceeded,
    /// 401 — JWT missing, signature invalid, or token expired. Remediation: `mnm login`.
    Unauthorized,
    /// 403 — caller authenticated but lacks the required role for this endpoint.
    Forbidden,
    /// 404 — resource not found (source slug, chunk id, etc.).
    NotFound,
    /// 409 — `client_embedding_model` disagrees with the corpus's active model (D12, FR-038).
    EmbeddingModelMismatch,
    /// 409 — write attempted against an `ingest_run` already in `aborted` state (FR-022).
    RunAborted,
    /// 429 — per-IP / SSO / CIDR rate-limit budget exhausted (D11, FR-031).
    RateLimited,
    // -- 5xx server-side --
    /// 500 — internal invariant violated. Should be rare; logged with `request_id`.
    Internal,
    /// 503 — transient backend unavailability (DB down, model not yet loaded). Caller
    /// should retry per the `Retry-After` header (Constitution VI, FR-035).
    ServiceUnavailable,
    /// 503 — MCP-tool-side: the local cloud client could not reach the server.
    CloudUnreachable,
    /// 503 — MCP-tool-side: local ML model file missing or corrupt; remediation: retry — the
    /// reranker loads lazily on first use (or pre-fetch via `mnm models pull`).
    ModelsMissing,
    /// 500 — telemetry event failed schema validation; dropped server-side (FR-109).
    TelemetrySchemaInvalid,
}

impl ErrorCode {
    /// Returns the canonical `lower_snake_case` wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::MultiQueryLimitExceeded => "multi_query_limit_exceeded",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::EmbeddingModelMismatch => "embedding_model_mismatch",
            Self::RunAborted => "run_aborted",
            Self::RateLimited => "rate_limited",
            Self::Internal => "internal",
            Self::ServiceUnavailable => "service_unavailable",
            Self::CloudUnreachable => "cloud_unreachable",
            Self::ModelsMissing => "models_missing",
            Self::TelemetrySchemaInvalid => "telemetry_schema_invalid",
        }
    }

    /// HTTP status code mapping (server side only; CLI maps codes to exit codes).
    #[must_use]
    pub const fn http_status(self) -> u16 {
        match self {
            Self::InvalidRequest | Self::MultiQueryLimitExceeded => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::EmbeddingModelMismatch | Self::RunAborted => 409,
            Self::RateLimited => 429,
            Self::ServiceUnavailable | Self::CloudUnreachable | Self::ModelsMissing => 503,
            Self::Internal | Self::TelemetrySchemaInvalid => 500,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-error free-form context map.
///
/// Permissive on the wire (`serde_json::Value`) so error envelopes can carry
/// arbitrary diagnostic context without a contract break. Builders should put
/// the offending field name, the conflicting model identifier, the rate-limit
/// reset time, etc. — never user input verbatim (Constitution VII).
pub type ErrorContext = BTreeMap<String, serde_json::Value>;

/// The wire-format error envelope: `{ error: { code, message, remediation, context } }`.
///
/// Always paired with an out-of-band `request_id` (returned in the `X-Request-Id`
/// header or alongside the envelope at the response root). Constructed via
/// [`Error::builder`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Error {
    /// Stable machine-readable code (see [`ErrorCode`]).
    pub code: ErrorCode,
    /// Operator-facing summary. One sentence, no trailing period.
    pub message: String,
    /// Concrete next step for the caller. Always populated (Constitution V).
    pub remediation: String,
    /// Optional structured diagnostic map; arbitrary additive keys allowed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: ErrorContext,
}

impl Error {
    /// Start building an [`Error`] of the given [`ErrorCode`].
    #[must_use]
    pub const fn builder(code: ErrorCode) -> ErrorBuilder {
        ErrorBuilder {
            code,
            message: None,
            remediation: None,
            context: ErrorContext::new(),
        }
    }

    /// The HTTP status implied by this error's code.
    #[must_use]
    pub const fn http_status(&self) -> u16 {
        self.code.http_status()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

/// Builder for [`Error`]. Forces `message` + `remediation` to be set by panicking
/// in `build()` if either is missing — every constructable error MUST be actionable.
#[derive(Debug)]
pub struct ErrorBuilder {
    code: ErrorCode,
    message: Option<String>,
    remediation: Option<String>,
    context: ErrorContext,
}

impl ErrorBuilder {
    /// Set the operator-facing message.
    #[must_use]
    pub fn message(mut self, msg: impl Into<String>) -> Self {
        self.message = Some(msg.into());
        self
    }

    /// Set the caller-actionable remediation string.
    #[must_use]
    pub fn remediation(mut self, rem: impl Into<String>) -> Self {
        self.remediation = Some(rem.into());
        self
    }

    /// Attach an arbitrary diagnostic key/value to the error context.
    #[must_use]
    pub fn context(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        self.context.insert(key.into(), value.into());
        self
    }

    /// Finalize the builder.
    ///
    /// # Panics
    ///
    /// Panics if `message` or `remediation` was not set. This is a programmer
    /// error (Constitution VI) — every error must be actionable.
    #[must_use]
    pub fn build(self) -> Error {
        Error {
            code: self.code,
            message: self
                .message
                .expect("Error::builder requires .message() — every error must have a summary"),
            remediation: self.remediation.expect(
                "Error::builder requires .remediation() — every error must point at a next step",
            ),
            context: self.context,
        }
    }
}

/// Crate-local `Result` alias parameterized on the typed [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trips_through_json() {
        for code in [
            ErrorCode::InvalidRequest,
            ErrorCode::EmbeddingModelMismatch,
            ErrorCode::RateLimited,
            ErrorCode::ModelsMissing,
        ] {
            let s = serde_json::to_string(&code).unwrap();
            let back: ErrorCode = serde_json::from_str(&s).unwrap();
            assert_eq!(code, back);
        }
    }

    #[test]
    fn wire_shape_is_stable() {
        let err = Error::builder(ErrorCode::EmbeddingModelMismatch)
            .message("client model bge-small@1 does not match corpus model bge-base@1")
            .remediation("run `mnm models pull` to fetch bge-base@1")
            .context("corpus_model", "bge-base-en-v1.5@1")
            .context("client_model", "bge-small-en-v1.5@1")
            .build();

        let v: serde_json::Value = serde_json::to_value(&err).unwrap();
        assert_eq!(v["code"], "embedding_model_mismatch");
        assert!(v["message"].is_string());
        assert!(v["remediation"].is_string());
        assert_eq!(v["context"]["corpus_model"], "bge-base-en-v1.5@1");
    }

    #[test]
    fn http_status_mapping() {
        assert_eq!(ErrorCode::InvalidRequest.http_status(), 400);
        assert_eq!(ErrorCode::Unauthorized.http_status(), 401);
        assert_eq!(ErrorCode::EmbeddingModelMismatch.http_status(), 409);
        assert_eq!(ErrorCode::RateLimited.http_status(), 429);
        assert_eq!(ErrorCode::ServiceUnavailable.http_status(), 503);
        assert_eq!(ErrorCode::Internal.http_status(), 500);
    }

    #[test]
    #[should_panic(expected = "requires .message()")]
    fn builder_panics_without_message() {
        let _ = Error::builder(ErrorCode::Internal)
            .remediation("file an issue")
            .build();
    }

    #[test]
    #[should_panic(expected = "requires .remediation()")]
    fn builder_panics_without_remediation() {
        let _ = Error::builder(ErrorCode::Internal)
            .message("something broke")
            .build();
    }

    #[test]
    fn empty_context_is_elided_from_wire() {
        let err = Error::builder(ErrorCode::NotFound)
            .message("source not found")
            .remediation("check `mnm sources list`")
            .build();
        let v: serde_json::Value = serde_json::to_value(&err).unwrap();
        assert!(v.get("context").is_none(), "empty context must be elided");
    }
}

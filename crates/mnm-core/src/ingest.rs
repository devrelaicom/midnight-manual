//! Shared document-upload wire types for the ingest pipeline.
//!
//! The document-upload handler reports per-document failures in a structured
//! `conflicts` array rather than failing the whole batch: a conflicted document
//! is silently NOT inserted into the finalized corpus version, so the conflict
//! list is the only signal an operator gets that documents were dropped.
//!
//! [`UploadConflict`] is the single source of truth for that wire shape, shared
//! by the server (which produces it) and the CLI (which must surface it). The
//! JSON representation is exactly `{ "path": ..., "reason": ... }`.

use serde::{Deserialize, Serialize};

/// Marker suffix appended to carry-conflict reasons when the failure requires
/// the CLI to re-embed and re-upload the document rather than just retrying.
///
/// The server embeds this string in its conflict messages; the CLI matches
/// against it with `.contains(REEMBED_REQUIRED_MARKER)`. Anchoring both sides
/// to this constant makes the cross-crate contract compiler-checked.
pub const REEMBED_REQUIRED_MARKER: &str = "re-embed required";

/// One per-document conflict surfaced by the document-upload handler.
///
/// A document carrying a conflict was NOT inserted into the corpus version
/// (e.g. a duplicate path within the upload batch, or a store-level insert
/// failure). Callers that ignore conflicts silently lose documents.
///
/// When the document was rejected by ingest-time prompt-injection scanning
/// (issue #103), `reason` is the stable marker [`PROMPT_INJECTION_REASON`] and
/// the optional `final_score` / `reject_threshold` / `pattern` / `model` fields
/// carry the full scan breakdown. For every other (non-injection) conflict those
/// fields are `None` and are omitted from the wire JSON, leaving the historical
/// `{ "path", "reason" }` shape untouched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UploadConflict {
    /// Repo-relative path of the offending document.
    pub path: String,
    /// Free-form reason the document was not inserted (or [`PROMPT_INJECTION_REASON`]).
    pub reason: String,
    /// Blended injection score that triggered rejection (injection rejections only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_score: Option<f64>,
    /// Reject threshold the blended score was compared against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_threshold: Option<f64>,
    /// Pattern-detector leg of the scan that rejected this document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<crate::injection::PatternResult>,
    /// Model-detector leg of the scan that rejected this document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<crate::injection::ModelReport>,
}

/// Stable `reason` value for documents rejected by prompt-injection scanning.
pub const PROMPT_INJECTION_REASON: &str = "prompt_injection";

/// Stable `reason` PREFIX for documents dropped because injection scanning could
/// not complete.
///
/// Used when the model leg was unreachable under [fail-closed] mode. The server
/// may append a parenthetical detail (e.g. `" (fail-closed)"`), so match with
/// `starts_with`, not equality.
///
/// [fail-closed]: crate::injection::FailMode::Closed
pub const PROMPT_INJECTION_UNAVAILABLE_REASON: &str = "prompt_injection_scan_unavailable";

impl UploadConflict {
    /// Construct a plain (non-injection) conflict: just a path and reason, with
    /// all injection-detail fields unset. This is the shape every pre-#103 call
    /// site produces.
    #[must_use]
    pub fn plain(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            reason: reason.into(),
            final_score: None,
            reject_threshold: None,
            pattern: None,
            model: None,
        }
    }

    /// Whether this conflict is an intentional prompt-injection drop — either a
    /// scan rejection ([`PROMPT_INJECTION_REASON`]) or a fail-closed
    /// scan-unavailable drop ([`PROMPT_INJECTION_UNAVAILABLE_REASON`]).
    ///
    /// The ingest finalize completeness backstop excludes these from its
    /// expected-document count: unlike an accidental drop (a failed insert), an
    /// injection drop is the feature working as intended and must NOT abort the
    /// whole run.
    #[must_use]
    pub fn is_injection_rejection(&self) -> bool {
        self.reason == PROMPT_INJECTION_REASON
            || self.reason.starts_with(PROMPT_INJECTION_UNAVAILABLE_REASON)
    }
}

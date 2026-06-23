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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploadConflict {
    /// Repo-relative path of the offending document.
    pub path: String,
    /// Free-form reason the document was not inserted.
    pub reason: String,
}

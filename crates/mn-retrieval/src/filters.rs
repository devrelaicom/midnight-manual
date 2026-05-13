//! Search-filter parser + SQL construction (US4 acceptance #11).
//!
//! Filters are an object on `POST /v1/search`:
//! ```json
//! {
//!   "attribution": ["foundation", "partner"],
//!   "verified": true,
//!   "content_type": ["tutorial"],
//!   "source_slug": ["midnight-docs"],
//!   "language_target": { "name": "compact", "version_constraint_satisfies": "0.31" },
//!   "sdk_dependency": [{ "kind": "npm", "name": "@midnight-ntwrk/midnight-js" }],
//!   "package": [{ "kind": "rust", "name": "midnight-foo" }]
//! }
//! ```
//!
//! Semantics: AND across keys, OR within each key's array. The
//! `language_target.version_constraint_satisfies` field is handled at the
//! application layer (semver crate) since Postgres has no native semver type.

use serde::{Deserialize, Serialize};

/// Top-level filter object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchFilters {
    /// Whitelist of `document.provenance.attribution` values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attribution: Vec<String>,
    /// If set, restrict to documents with `verified` matching this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// Whitelist of content-type tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_type: Vec<String>,
    /// Whitelist of source slugs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_slug: Vec<String>,
    /// Optional language-target constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_target: Option<LanguageTargetFilter>,
    /// Whitelist of SDK dependencies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sdk_dependency: Vec<SdkDependencyFilter>,
    /// Whitelist of packages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package: Vec<PackageFilter>,
}

/// One language-target filter, optionally with a version constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageTargetFilter {
    /// Language name (e.g. `"compact"`).
    pub name: String,
    /// Optional version constraint: only documents whose
    /// `provenance.language_targets` include a target whose `version_constraint`
    /// matches this value satisfy the filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_constraint_satisfies: Option<String>,
}

/// One SDK-dependency filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkDependencyFilter {
    /// Package-manager kind (e.g. `"npm"`, `"cargo"`).
    pub kind: String,
    /// Canonical package name.
    pub name: String,
}

/// One package filter (matches `chunk -> document -> package` membership).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageFilter {
    /// Package kind.
    pub kind: String,
    /// Package name.
    pub name: String,
}

impl SearchFilters {
    /// `true` when no filters at all are set. Lets the SQL builder short-
    /// circuit and skip the WHERE-clause additions entirely.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.attribution.is_empty()
            && self.verified.is_none()
            && self.content_type.is_empty()
            && self.source_slug.is_empty()
            && self.language_target.is_none()
            && self.sdk_dependency.is_empty()
            && self.package.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_default() {
        assert!(SearchFilters::default().is_empty());
    }

    #[test]
    fn round_trips_through_json() {
        let f = SearchFilters {
            attribution: vec!["foundation".into(), "partner".into()],
            verified: Some(true),
            content_type: vec!["tutorial".into()],
            source_slug: vec!["midnight-docs".into()],
            language_target: Some(LanguageTargetFilter {
                name: "compact".into(),
                version_constraint_satisfies: Some("0.31".into()),
            }),
            sdk_dependency: vec![SdkDependencyFilter {
                kind: "npm".into(),
                name: "@midnight-ntwrk/midnight-js".into(),
            }],
            package: vec![PackageFilter {
                kind: "rust".into(),
                name: "midnight-foo".into(),
            }],
        };
        let v = serde_json::to_value(&f).unwrap();
        let back: SearchFilters = serde_json::from_value(v).unwrap();
        assert_eq!(f, back);
        assert!(!back.is_empty());
    }

    #[test]
    fn empty_arrays_are_elided_on_serialize() {
        let f = SearchFilters::default();
        let v = serde_json::to_value(f).unwrap();
        assert!(v.get("attribution").is_none());
        assert!(v.get("source_slug").is_none());
    }
}

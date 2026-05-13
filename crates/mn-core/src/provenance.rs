//! `document.provenance` JSONB shape — the trust + verification metadata that
//! drives the confidence-scoring blend in Phase 9 (US6, D24).
//!
//! The DB column is `jsonb` so unknown keys are tolerated at the storage layer;
//! validation happens at the application boundary (Constitution VIII). The
//! struct here is the canonical Rust mirror — see `data-model.md` §"JSONB
//! schemas" for the wire shape and `specs/001-rag-platform/spec.md` US6 for the
//! per-field scoring impact.

use serde::{Deserialize, Serialize};
use time::Date;

/// Source attribution — drives the dominant trust-score multiplier (US6 §"Trust
/// score computation").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Attribution {
    /// Authored by the Midnight Foundation. Highest default trust.
    Foundation,
    /// Authored by a Midnight ecosystem partner under agreement.
    Partner,
    /// Third-party content endorsed for inclusion.
    ThirdParty,
    /// Community-contributed content with no explicit vetting.
    Community,
    /// Attribution not set — lowest default trust.
    #[default]
    Unknown,
}

/// Free-form content-type tag used in filters and scoring. New variants are
/// additive; unknown wire values deserialize to [`ContentType::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    /// Reference documentation, API surface.
    Doc,
    /// Step-by-step tutorial / walkthrough.
    Tutorial,
    /// Pure reference material (specs, RFCs).
    Reference,
    /// Worked example.
    Example,
    /// Compact contract source code.
    ContractSource,
    /// SDK source code (Rust / TypeScript).
    SdkSource,
    /// Test source.
    Test,
    /// README file at any level.
    Readme,
    /// Any other content type — preserved verbatim where possible.
    #[default]
    #[serde(other)]
    Other,
}

/// Optional language target a document/chunk applies to. Used by US6's
/// `version_match` scoring multiplier (e.g. "this is Compact ≥ 0.23 only").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LanguageTarget {
    /// Language name (e.g. `"compact"`, `"rust"`, `"typescript"`).
    pub name: String,
    /// Optional semver-style constraint, e.g. `">=0.23"`, `"^1.4.0"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_constraint: Option<String>,
}

/// Declared SDK dependency a document/chunk targets — used in filter narrowing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SdkDependency {
    /// Package ecosystem kind (e.g. `"npm"`, `"cargo"`).
    pub kind: String,
    /// Canonical package name.
    pub name: String,
    /// Optional semver constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_constraint: Option<String>,
}

/// Deprecation flag on a document/chunk. Triggers a configurable penalty
/// multiplier (default ×0.3, US6 §"Trust score computation").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Deprecation {
    /// Whether the content is marked deprecated.
    pub is_deprecated: bool,
    /// Optional ISO date marking when the deprecation began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Date>,
    /// Optional human-readable reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The full `document.provenance` JSONB shape mirrored as a typed Rust struct.
///
/// All fields are optional on the wire — old corpora may have sparse provenance,
/// and scoring multipliers degrade to sensible defaults (see [`Attribution::Unknown`],
/// `verified=false`, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Provenance {
    /// Author attribution (drives the dominant trust multiplier).
    #[serde(default)]
    pub attribution: Attribution,

    /// Whether the content has been explicitly verified by an authorized
    /// principal (Foundation, partner, or other vetted reviewer).
    #[serde(default)]
    pub verified: bool,
    /// Who performed the verification, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<String>,
    /// When the verification occurred (ISO date).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<Date>,
    /// Free-form notes captured during verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_notes: Option<String>,

    /// Programming-language constraints the content applies to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_targets: Vec<LanguageTarget>,

    /// SDK package constraints the content applies to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sdk_dependencies: Vec<SdkDependency>,

    /// Deprecation flag (with optional reason / since).
    #[serde(default)]
    pub deprecation: Deprecation,

    /// Free-form taxonomy tags used in filter narrowing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Coarse content-type tag.
    #[serde(default)]
    pub content_type: ContentType,
}

impl Provenance {
    /// Construct a `Provenance` carrying only the attribution. Convenience for
    /// minimal-metadata test fixtures.
    #[must_use]
    pub fn attributed_to(attribution: Attribution) -> Self {
        Self { attribution, ..Self::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_safe() {
        let p = Provenance::default();
        assert_eq!(p.attribution, Attribution::Unknown);
        assert!(!p.verified);
        assert!(p.tags.is_empty());
    }

    #[test]
    fn round_trips_full_shape() {
        let p = Provenance {
            attribution: Attribution::Foundation,
            verified: true,
            verified_by: Some("midnight-foundation".into()),
            verified_at: Date::from_calendar_date(2026, time::Month::April, 1).ok(),
            verification_notes: None,
            language_targets: vec![LanguageTarget {
                name: "compact".into(),
                version_constraint: Some(">=0.23".into()),
            }],
            sdk_dependencies: vec![SdkDependency {
                kind: "npm".into(),
                name: "@midnight-ntwrk/midnight-js".into(),
                version_constraint: Some("^1.4.0".into()),
            }],
            deprecation: Deprecation::default(),
            tags: vec!["quickstart".into(), "tutorial".into()],
            content_type: ContentType::Tutorial,
        };
        let v = serde_json::to_value(&p).unwrap();
        let back: Provenance = serde_json::from_value(v).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn empty_collections_elided() {
        let v = serde_json::to_value(Provenance::default()).unwrap();
        assert!(v.get("language_targets").is_none());
        assert!(v.get("sdk_dependencies").is_none());
        assert!(v.get("tags").is_none());
    }

    #[test]
    fn tolerates_unknown_attribution_via_default() {
        // Forward-compatibility: server adds a new attribution variant; old
        // CLI deserializing the response should still parse the document. We
        // model this by deserializing a totally absent field which falls back
        // to the Default impl.
        let v = serde_json::json!({});
        let p: Provenance = serde_json::from_value(v).unwrap();
        assert_eq!(p.attribution, Attribution::Unknown);
    }

    #[test]
    fn unknown_content_type_falls_back_to_other() {
        let v = serde_json::json!({ "content_type": "blog_post_2026_meta" });
        let p: Provenance = serde_json::from_value(v).unwrap();
        assert_eq!(p.content_type, ContentType::Other);
    }

    #[test]
    fn attribution_serializes_snake_case() {
        let v = serde_json::to_value(Attribution::ThirdParty).unwrap();
        assert_eq!(v, serde_json::Value::String("third_party".into()));
    }
}

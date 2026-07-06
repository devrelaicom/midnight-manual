//! `document.provenance` JSONB shape — the trust + verification metadata that
//! drives the confidence-scoring blend in Phase 9 (US6, D24).
//!
//! The DB column is `jsonb` so unknown keys are tolerated at the storage layer;
//! validation happens at the application boundary. The struct here is the
//! canonical Rust mirror — see the data-model schema reference §"JSONB schemas"
//! for the wire shape and US6 for the per-field scoring impact.

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
    /// Attribution not set, or an unrecognized/naturally-cased wire value —
    /// lowest default trust. `#[serde(other)]` mirrors [`ContentType::Other`]
    /// so an unknown value degrades gracefully to this variant instead of
    /// failing deserialization of the entire [`Provenance`] struct (which would
    /// wipe every sibling field via the consumer's `unwrap_or_default()`).
    #[default]
    #[serde(other)]
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
    fn absent_attribution_field_uses_default() {
        // `#[serde(default)]` on the field: a totally absent `attribution` key
        // falls back to the Default impl. (This is distinct from an unknown
        // *value*, covered below — the old name conflated the two.)
        let v = serde_json::json!({});
        let p: Provenance = serde_json::from_value(v).unwrap();
        assert_eq!(p.attribution, Attribution::Unknown);
    }

    #[test]
    fn unknown_attribution_value_falls_back_to_unknown() {
        // `#[serde(other)]` on `Unknown`: an unrecognized wire value degrades to
        // the field default rather than erroring.
        let v = serde_json::json!({ "attribution": "supreme_overlord" });
        let p: Provenance = serde_json::from_value(v).unwrap();
        assert_eq!(p.attribution, Attribution::Unknown);
    }

    #[test]
    fn unknown_attribution_value_preserves_sibling_fields() {
        // Regression for #168: before the `#[serde(other)]` fallback, an unknown
        // or naturally-cased attribution value (here PascalCase `Foundation`,
        // which the snake_case rename does not accept) failed deserialization of
        // the ENTIRE struct. The consumer's `unwrap_or_default()` then discarded
        // every sibling field. The failure must now be isolated to `attribution`.
        let v = serde_json::json!({
            "attribution": "Foundation", // PascalCase — not the snake_case wire form
            "verified": true,
            "verified_by": "midnight-foundation",
            "tags": ["quickstart"],
        });
        let p: Provenance = serde_json::from_value(v).unwrap();
        assert_eq!(p.attribution, Attribution::Unknown, "unknown value degrades to Unknown");
        assert!(p.verified, "verified must survive an unknown attribution");
        assert_eq!(p.verified_by.as_deref(), Some("midnight-foundation"));
        assert_eq!(p.tags, vec!["quickstart".to_string()]);
    }

    #[test]
    fn known_attribution_values_still_deserialize() {
        // The `#[serde(other)]` fallback must not swallow the real variants.
        for (wire, expected) in [
            ("foundation", Attribution::Foundation),
            ("partner", Attribution::Partner),
            ("third_party", Attribution::ThirdParty),
            ("community", Attribution::Community),
            ("unknown", Attribution::Unknown),
        ] {
            let v = serde_json::json!({ "attribution": wire });
            let p: Provenance = serde_json::from_value(v).unwrap();
            assert_eq!(p.attribution, expected, "wire={wire}");
        }
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
        // `#[serde(other)]` on `Unknown` must not affect serialization: a genuine
        // Unknown still emits `"unknown"` (it round-trips, unlike a catch-all).
        let u = serde_json::to_value(Attribution::Unknown).unwrap();
        assert_eq!(u, serde_json::Value::String("unknown".into()));
    }
}

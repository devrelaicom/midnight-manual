//! Per-facet match model for `POST /v1/search` filters (see
//! docs/superpowers/specs/2026-06-04-search-facets-query-modes-design.md).
//!
//! Every facet is one of: a set-match (`{any_of, none_of}`) over strings or
//! structured elements, a bare bool, or a range. Combination is AND across
//! facets, OR within `any_of`, exclude `none_of`. The semver-bearing facets
//! (`language_target`, `sdk_dependency`) carry a `version_satisfies` field
//! evaluated in the Rust post-match (a later task), not SQL.

use serde::{Deserialize, Serialize};
use time::Date;

/// Set membership for one facet: OR within `any_of`, exclude `none_of`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetMatch<T> {
    /// Values that satisfy the facet; empty means "no positive constraint".
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub any_of: Vec<T>,
    /// Values that disqualify a row regardless of `any_of`.
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub none_of: Vec<T>,
}

// Hand-written so the empty `SetMatch` requires no `T: Default` bound — both
// fields are `Vec<T>`, which is `Default` for any `T`. `#[derive(Default)]`
// would (incorrectly) add `T: Default`, which the element matchers below
// (`PackageMatch`, `SdkDependencyMatch`, ...) cannot satisfy.
impl<T> Default for SetMatch<T> {
    fn default() -> Self {
        Self {
            any_of: Vec::new(),
            none_of: Vec::new(),
        }
    }
}

impl<T> SetMatch<T> {
    /// True when neither `any_of` nor `none_of` constrains anything.
    // Kept non-`const` so the public API stays uniform with the
    // necessarily-non-const `SearchFilters::is_empty`; the spec fixes these
    // signatures.
    #[allow(clippy::missing_const_for_fn)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.any_of.is_empty() && self.none_of.is_empty()
    }
}

/// One `symbol` element matcher (`chunk.symbol_path` JSONB containment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolMatch {
    /// Symbol kind to match (e.g. `"circuit"`); `None` matches any kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Symbol name to match; `None` matches any name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One `package` element matcher (`chunk -> document -> package`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMatch {
    /// Package-manager kind (e.g. `"cargo"`, `"npm"`).
    pub kind: String,
    /// Canonical package name.
    pub name: String,
}

/// One `language_target` element (semver evaluated in the Rust post-match).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageTargetMatch {
    /// Language-target name (e.g. `"compact"`).
    pub name: String,
    /// Optional semver requirement evaluated against the target's constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_satisfies: Option<String>,
}

/// One `sdk_dependency` element (semver evaluated in the Rust post-match).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdkDependencyMatch {
    /// Package-manager kind (e.g. `"npm"`, `"cargo"`).
    pub kind: String,
    /// Canonical dependency name.
    pub name: String,
    /// Optional semver requirement evaluated against the dependency's constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_satisfies: Option<String>,
}

/// `{after?, before?}` inclusive temporal range (ISO dates).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalRange {
    /// Inclusive lower bound; `None` leaves the range open below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Date>,
    /// Inclusive upper bound; `None` leaves the range open above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Date>,
}

/// `{min?, max?}` inclusive numeric range.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumericRange {
    /// Inclusive lower bound; `None` leaves the range open below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    /// Inclusive upper bound; `None` leaves the range open above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
}

/// Top-level filter object. Every field defaults to empty/absent so a missing
/// `filters` key means "no constraints". `deny_unknown_fields` makes a
/// misspelled facet a hard error instead of a silent drop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchFilters {
    /// `document.provenance.attribution` set-match.
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub attribution: SetMatch<String>,
    /// Content-type set-match.
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub content_type: SetMatch<String>,
    /// Chunk-kind set-match (e.g. `"code"`, `"prose"`).
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub kind: SetMatch<String>,
    /// Source-kind set-match.
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub source_kind: SetMatch<String>,
    /// Source-slug set-match.
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub source_slug: SetMatch<String>,
    /// Programming-language set-match (e.g. `"compact"`).
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub language: SetMatch<String>,
    /// Tag set-match.
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub tags: SetMatch<String>,
    /// Heading-path set-match.
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub heading_path: SetMatch<String>,
    /// Symbol set-match over `{kind?, name?}` elements.
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub symbol: SetMatch<SymbolMatch>,
    /// Package set-match over `{kind, name}` elements.
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub package: SetMatch<PackageMatch>,
    /// Restrict to documents whose `verified` flag equals this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// Restrict to chunks whose `deprecated` flag equals this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    /// Language-target set-match (semver in the Rust post-match).
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub language_target: SetMatch<LanguageTargetMatch>,
    /// SDK-dependency set-match (semver in the Rust post-match).
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub sdk_dependency: SetMatch<SdkDependencyMatch>,
    /// Ingestion-timestamp range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingested_at: Option<TemporalRange>,
    /// Upstream source-modified-timestamp range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_modified_at: Option<TemporalRange>,
    /// Chunk token-count range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<NumericRange>,
}

impl SearchFilters {
    /// True when no facet constrains anything (lets the SQL builder skip work).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.attribution.is_empty()
            && self.content_type.is_empty()
            && self.kind.is_empty()
            && self.source_kind.is_empty()
            && self.source_slug.is_empty()
            && self.language.is_empty()
            && self.tags.is_empty()
            && self.heading_path.is_empty()
            && self.symbol.is_empty()
            && self.package.is_empty()
            && self.verified.is_none()
            && self.deprecated.is_none()
            && self.language_target.is_empty()
            && self.sdk_dependency.is_empty()
            && self.ingested_at.is_none()
            && self.source_modified_at.is_none()
            && self.token_count.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_shape_round_trips() {
        let json = serde_json::json!({
            "kind":        { "any_of": ["code"] },
            "language":    { "any_of": ["compact"], "none_of": ["typescript"] },
            "symbol":      { "any_of": [{ "kind": "circuit" }, { "name": "deployContract" }] },
            "deprecated":  false,
            "ingested_at": { "after": "2026-05-01" },
            "token_count": { "min": 50 }
        });
        let f: SearchFilters = serde_json::from_value(json).unwrap();
        assert_eq!(f.kind.any_of, vec!["code".to_owned()]);
        assert_eq!(f.language.none_of, vec!["typescript".to_owned()]);
        assert_eq!(f.symbol.any_of.len(), 2);
        assert_eq!(f.deprecated, Some(false));
        assert_eq!(f.token_count.as_ref().unwrap().min, Some(50));
        // serialize → deserialize identity
        let back: SearchFilters =
            serde_json::from_value(serde_json::to_value(&f).unwrap()).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn default_is_empty() {
        assert!(SearchFilters::default().is_empty());
    }

    #[test]
    fn empty_arrays_elided_on_serialize() {
        let v = serde_json::to_value(SearchFilters::default()).unwrap();
        assert!(v.as_object().unwrap().is_empty(), "default must serialize to `{{}}`");
    }

    #[test]
    fn rejects_unknown_nested_field() {
        // A typo in a nested matcher must be a hard error, not a silent vacuous
        // match — `deny_unknown_fields` applies one level down, not just at the
        // top level. `knid` (a misspelled `kind`) would otherwise deserialize
        // into a match-anything `SymbolMatch`.
        let typo_element = serde_json::json!({ "symbol": { "any_of": [{ "knid": "circuit" }] } });
        assert!(serde_json::from_value::<SearchFilters>(typo_element).is_err());
        let typo_set = serde_json::json!({ "language": { "any_of": ["x"], "nope": 1 } });
        assert!(serde_json::from_value::<SearchFilters>(typo_set).is_err());
        let typo_range = serde_json::json!({ "token_count": { "min": 1, "maxx": 9 } });
        assert!(serde_json::from_value::<SearchFilters>(typo_range).is_err());
    }
}

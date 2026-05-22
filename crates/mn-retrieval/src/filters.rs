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

use mn_core::provenance::Provenance;
use mn_core::scoring::parse_version;
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
    /// Optional concrete version that the chunk's declared
    /// `provenance.sdk_dependencies[*].version_constraint` must be satisfied by
    /// (semver, evaluated server-side per FR-033). `None` means name-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_constraint_satisfies: Option<String>,
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

    /// Evaluate the filter dimensions that Postgres can't express — the semver
    /// `version_constraint_satisfies` refinements on `language_target` and
    /// `sdk_dependency` (FR-033) — against a chunk's provenance.
    ///
    /// The scalar/array dimensions (`attribution`, `verified`, `content_type`,
    /// `source_slug`, `package`) are applied in SQL during candidate retrieval;
    /// this method handles only what remains so a fully-matching chunk passes
    /// both layers. Semantics: AND across keys, OR within each key's array.
    #[must_use]
    pub fn semver_post_match(&self, provenance: &Provenance) -> bool {
        if let Some(lt) = &self.language_target {
            let matched = provenance.language_targets.iter().any(|t| {
                t.name.eq_ignore_ascii_case(&lt.name)
                    && version_satisfies(
                        lt.version_constraint_satisfies.as_deref(),
                        t.version_constraint.as_deref(),
                    )
            });
            if !matched {
                return false;
            }
        }
        if !self.sdk_dependency.is_empty() {
            let matched = self.sdk_dependency.iter().any(|dep| {
                provenance.sdk_dependencies.iter().any(|d| {
                    d.kind.eq_ignore_ascii_case(&dep.kind)
                        && d.name == dep.name
                        && version_satisfies(
                            dep.version_constraint_satisfies.as_deref(),
                            d.version_constraint.as_deref(),
                        )
                })
            });
            if !matched {
                return false;
            }
        }
        true
    }
}

/// Does a concrete requested version satisfy a chunk's declared version
/// constraint? `requested = None` (name-only filter) always passes. A chunk
/// with no constraint applies to every version, so it passes any request. A
/// malformed requested version is treated leniently (name-only). A chunk
/// constraint that fails to parse never satisfies.
fn version_satisfies(requested: Option<&str>, chunk_constraint: Option<&str>) -> bool {
    let Some(req) = requested else {
        return true;
    };
    let Some(candidate) = parse_version(req) else {
        return true;
    };
    chunk_constraint
        .is_none_or(|c| semver::VersionReq::parse(c).is_ok_and(|r| r.matches(&candidate)))
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
                version_constraint_satisfies: Some("1.4.0".into()),
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

    use mn_core::provenance::{LanguageTarget, Provenance, SdkDependency};

    fn prov_with_compact(constraint: Option<&str>) -> Provenance {
        Provenance {
            language_targets: vec![LanguageTarget {
                name: "compact".into(),
                version_constraint: constraint.map(str::to_owned),
            }],
            ..Provenance::default()
        }
    }

    #[test]
    fn empty_filter_post_matches_anything() {
        assert!(SearchFilters::default().semver_post_match(&Provenance::default()));
    }

    #[test]
    fn language_target_name_only_requires_matching_target() {
        let f = SearchFilters {
            language_target: Some(LanguageTargetFilter {
                name: "compact".into(),
                version_constraint_satisfies: None,
            }),
            ..Default::default()
        };
        assert!(f.semver_post_match(&prov_with_compact(Some(">=0.23"))));
        // A chunk with no compact target fails the name filter.
        assert!(!f.semver_post_match(&Provenance::default()));
    }

    #[test]
    fn language_target_version_satisfies_and_misses() {
        let f = |v: &str| SearchFilters {
            language_target: Some(LanguageTargetFilter {
                name: "compact".into(),
                version_constraint_satisfies: Some(v.to_owned()),
            }),
            ..Default::default()
        };
        // chunk targets compact >=0.23
        let prov = prov_with_compact(Some(">=0.23"));
        assert!(f("0.31").semver_post_match(&prov), "0.31 satisfies >=0.23");
        assert!(!f("0.10").semver_post_match(&prov), "0.10 misses >=0.23");
        // chunk with no constraint applies to all versions.
        assert!(f("0.10").semver_post_match(&prov_with_compact(None)));
    }

    #[test]
    fn sdk_dependency_is_or_within_array() {
        let prov = Provenance {
            sdk_dependencies: vec![SdkDependency {
                kind: "npm".into(),
                name: "@midnight-ntwrk/midnight-js".into(),
                version_constraint: Some(">=1.0.0".into()),
            }],
            ..Provenance::default()
        };
        let f = SearchFilters {
            sdk_dependency: vec![
                SdkDependencyFilter {
                    kind: "cargo".into(),
                    name: "nope".into(),
                    version_constraint_satisfies: None,
                },
                SdkDependencyFilter {
                    kind: "npm".into(),
                    name: "@midnight-ntwrk/midnight-js".into(),
                    version_constraint_satisfies: Some("1.4.0".into()),
                },
            ],
            ..Default::default()
        };
        // Second filter entry matches → OR within the array passes.
        assert!(f.semver_post_match(&prov));

        // A version that the chunk constraint can't satisfy fails.
        let f_miss = SearchFilters {
            sdk_dependency: vec![SdkDependencyFilter {
                kind: "npm".into(),
                name: "@midnight-ntwrk/midnight-js".into(),
                version_constraint_satisfies: Some("0.9.0".into()),
            }],
            ..Default::default()
        };
        assert!(!f_miss.semver_post_match(&prov));
    }
}

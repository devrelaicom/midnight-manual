//! Per-facet match model for `POST /v1/search` filters (see
//! docs/superpowers/specs/2026-06-04-search-facets-query-modes-design.md).
//!
//! Every facet is one of: a set-match (`{any_of, none_of}`) over strings or
//! structured elements, a bare bool, or a range. Combination is AND across
//! facets, OR within `any_of`, exclude `none_of`. The semver-bearing facets
//! (`language_target`, `sdk_dependency`) carry a `version_satisfies` field
//! classified per-candidate in [`SearchFilters::version_outcomes`], not SQL.

use mn_core::provenance::Provenance;
use serde::{Deserialize, Serialize};
use time::Date;

use crate::facets;

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
    /// Symbol kind to match — an open, chunker-derived syntactic label (e.g.
    /// `"fn"`, `"struct"`, `"impl"`, `"method"`, `"class"`); `None` matches any
    /// kind.
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

/// Version-matching mode for the semver-bearing facets (spec §3). Default
/// permissive: filters bias rather than restrict; only breaking mismatches drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionMatchMode {
    /// Hard semantics: SQL name gate + drop everything not Satisfies.
    Strict,
    /// Soft preference: no name gate; Breaking drops, near-misses are penalized.
    #[default]
    Permissive,
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

/// A filter validation failure, naming the offending facet. The server maps
/// this to a 400 `invalid_request` with a remediation pointing at `/v1/facets`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterError {
    /// The wire key of the offending facet (matches a `/v1/facets` key).
    pub facet: String,
    /// Human-readable description of the violation.
    pub message: String,
}

impl FilterError {
    fn new(facet: &str, message: impl Into<String>) -> Self {
        Self {
            facet: facet.to_owned(),
            message: message.into(),
        }
    }
}

impl SearchFilters {
    /// Validate every set facet against the registry (closed-set values +
    /// negatability), ranges for ordering, and semver constraints for
    /// parseability. Returns the first violation found.
    ///
    /// # Errors
    /// Returns [`FilterError`] naming the offending facet on any violation.
    pub fn validate(&self) -> Result<(), FilterError> {
        check_string_set("attribution", &self.attribution)?;
        check_string_set("content_type", &self.content_type)?;
        check_string_set("kind", &self.kind)?;
        check_string_set("source_kind", &self.source_kind)?;
        check_string_set("source_slug", &self.source_slug)?;
        check_string_set("language", &self.language)?;
        check_string_set("tags", &self.tags)?;
        check_string_set("heading_path", &self.heading_path)?;
        check_negatable("symbol", &self.symbol)?;
        check_negatable("package", &self.package)?;
        // `symbol.kind` is intentionally NOT validated against a fixed enum: it
        // is an open, chunker-derived syntactic vocabulary (e.g. `impl`,
        // `method`, `class`, `trait`, ...), like `language`/`tags`. Only
        // `package.kind` is closed (DB `CHECK (kind IN (...))`).
        check_kind_enum(
            "package",
            facets::PACKAGE_KIND_VALUES,
            self.package
                .any_of
                .iter()
                .chain(self.package.none_of.iter())
                .map(|p| p.kind.as_str()),
        )?;
        // language_target / sdk_dependency: not negatable + semver parseable.
        check_negatable("language_target", &self.language_target)?;
        check_negatable("sdk_dependency", &self.sdk_dependency)?;
        for lt in &self.language_target.any_of {
            check_semver("language_target", lt.version_satisfies.as_deref())?;
        }
        for dep in &self.sdk_dependency.any_of {
            check_semver("sdk_dependency", dep.version_satisfies.as_deref())?;
        }
        check_numeric_range("token_count", self.token_count.as_ref())?;
        check_temporal_range("ingested_at", self.ingested_at.as_ref())?;
        check_temporal_range("source_modified_at", self.source_modified_at.as_ref())?;
        Ok(())
    }

    /// Classify a candidate's provenance against the version-bearing facets.
    /// Callers must have run [`SearchFilters::validate`] (unparseable
    /// `version_satisfies` values are treated as absent here).
    #[must_use]
    pub fn version_outcomes(&self, provenance: &Provenance) -> VersionOutcomes {
        let language_target =
            if self.language_target.any_of.is_empty() {
                None
            } else {
                best_facet_outcome(self.language_target.any_of.iter().enumerate().map(
                    |(i, want)| {
                        let constraints: Vec<Option<&str>> = provenance
                            .language_targets
                            .iter()
                            .filter(|have| have.name.eq_ignore_ascii_case(&want.name))
                            .map(|have| have.version_constraint.as_deref())
                            .collect();
                        (i, want.version_satisfies.as_deref(), constraints)
                    },
                ))
            };
        let sdk_dependency =
            if self.sdk_dependency.any_of.is_empty() {
                None
            } else {
                best_facet_outcome(self.sdk_dependency.any_of.iter().enumerate().map(
                    |(i, want)| {
                        let constraints: Vec<Option<&str>> = provenance
                            .sdk_dependencies
                            .iter()
                            .filter(|have| {
                                have.kind.eq_ignore_ascii_case(&want.kind) && have.name == want.name
                            })
                            .map(|have| have.version_constraint.as_deref())
                            .collect();
                        (i, want.version_satisfies.as_deref(), constraints)
                    },
                ))
            };
        VersionOutcomes {
            language_target,
            sdk_dependency,
        }
    }
}

/// Per-facet classification outcome for one candidate (spec §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacetVersionOutcome {
    /// The chunk declares no matching-name target for any requested element.
    Silent,
    /// Best classification across (element, matching target) pairs.
    Classified {
        /// The winning class.
        class: mn_core::version_match::MatchClass,
        /// Index into the facet's `any_of` of the winning element.
        element: usize,
    },
}

/// Outcomes for both semver-bearing facets; `None` = facet unconstrained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionOutcomes {
    /// `language_target` facet outcome.
    pub language_target: Option<FacetVersionOutcome>,
    /// `sdk_dependency` facet outcome.
    pub sdk_dependency: Option<FacetVersionOutcome>,
}

/// Best classification across a facet's elements (spec §3.3). Each item is
/// `(element idx, requested version, matching targets' constraints)`. Elements
/// with no matching-name target contribute nothing; the facet is [`Silent`]
/// only when no element matched any target's name.
///
/// [`Silent`]: FacetVersionOutcome::Silent
fn best_facet_outcome<'t>(
    elements: impl Iterator<Item = (usize, Option<&'t str>, Vec<Option<&'t str>>)>,
) -> Option<FacetVersionOutcome> {
    use mn_core::version_match::{class_rank, classify, parse_request, MatchClass};

    let mut found_any_name = false;
    let mut best: Option<(usize, MatchClass)> = None;
    for (idx, requested, constraints) in elements {
        if constraints.is_empty() {
            continue;
        }
        found_any_name = true;
        let parsed = requested.and_then(parse_request);
        for c in constraints {
            let class = match (&parsed, requested) {
                (Some(p), _) => classify(p, c),
                // name-only element (no version requested) → Satisfies
                (None, None) => MatchClass::Satisfies,
                // unparseable requested (validate() should prevent)
                (None, Some(_)) => MatchClass::Unknown,
            };
            if best
                .as_ref()
                .is_none_or(|(_, b)| class_rank(&class) < class_rank(b))
            {
                best = Some((idx, class));
            }
        }
    }
    match best {
        Some((element, class)) => Some(FacetVersionOutcome::Classified { class, element }),
        None if found_any_name => None, // unreachable; kept for totality
        None => Some(FacetVersionOutcome::Silent),
    }
}

fn check_string_set(key: &str, set: &SetMatch<String>) -> Result<(), FilterError> {
    // Negatability is the same rule for every set facet, so defer to the shared
    // check rather than duplicate it here.
    check_negatable(key, set)?;
    let desc = facets::lookup(key).expect("registry key");
    if let Some(allowed) = desc.closed_values {
        for v in set.any_of.iter().chain(set.none_of.iter()) {
            if !allowed.contains(&v.as_str()) {
                return Err(FilterError::new(
                    key,
                    format!("`{v}` is not a valid `{key}` value (allowed: {})", allowed.join(", ")),
                ));
            }
        }
    }
    Ok(())
}

fn check_negatable<T>(key: &str, set: &SetMatch<T>) -> Result<(), FilterError> {
    let desc = facets::lookup(key).expect("registry key");
    if !desc.negatable && !set.none_of.is_empty() {
        return Err(FilterError::new(key, format!("`{key}` does not support `none_of`")));
    }
    Ok(())
}

/// Reject any element `kind` not in a facet's closed sub-enum (e.g. `symbol.kind`).
fn check_kind_enum<'a>(
    facet: &str,
    allowed: &[&str],
    kinds: impl Iterator<Item = &'a str>,
) -> Result<(), FilterError> {
    for k in kinds {
        if !allowed.contains(&k) {
            return Err(FilterError::new(
                facet,
                format!("`{k}` is not a valid `{facet}.kind` (allowed: {})", allowed.join(", ")),
            ));
        }
    }
    Ok(())
}

fn check_semver(key: &str, constraint: Option<&str>) -> Result<(), FilterError> {
    if let Some(c) = constraint {
        if mn_core::version_match::parse_request(c).is_none() {
            return Err(FilterError::new(
                key,
                format!(
                    "`{c}` is not a valid version or range — pass the user's concrete \
                     version (e.g. `0.31`) or a semver range (e.g. `>=0.23`)"
                ),
            ));
        }
    }
    Ok(())
}

fn check_numeric_range(key: &str, r: Option<&NumericRange>) -> Result<(), FilterError> {
    if let Some(r) = r {
        if let (Some(min), Some(max)) = (r.min, r.max) {
            if min > max {
                return Err(FilterError::new(key, "min must be <= max"));
            }
        }
    }
    Ok(())
}

fn check_temporal_range(key: &str, r: Option<&TemporalRange>) -> Result<(), FilterError> {
    if let Some(r) = r {
        if let (Some(after), Some(before)) = (r.after, r.before) {
            if after > before {
                return Err(FilterError::new(key, "after must be <= before"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mn_core::provenance::LanguageTarget;

    #[test]
    fn semver_post_match_filters_by_version() {
        use mn_core::version_match::MatchClass;
        let prov = Provenance {
            language_targets: vec![LanguageTarget {
                name: "compact".into(),
                version_constraint: Some(">=0.23".into()),
            }],
            ..Provenance::default()
        };
        let satisfies = SearchFilters {
            language_target: SetMatch {
                any_of: vec![LanguageTargetMatch {
                    name: "compact".into(),
                    version_satisfies: Some("0.31".into()),
                }],
                none_of: vec![],
            },
            ..Default::default()
        };
        assert!(matches!(
            satisfies.version_outcomes(&prov).language_target,
            Some(FacetVersionOutcome::Classified {
                class: MatchClass::Satisfies,
                ..
            })
        ));
        let misses = SearchFilters {
            language_target: SetMatch {
                any_of: vec![LanguageTargetMatch {
                    name: "compact".into(),
                    version_satisfies: Some("0.10".into()),
                }],
                none_of: vec![],
            },
            ..Default::default()
        };
        assert!(matches!(
            misses.version_outcomes(&prov).language_target,
            Some(FacetVersionOutcome::Classified {
                class: MatchClass::Breaking,
                ..
            })
        ));
    }

    #[test]
    fn empty_filter_post_matches_anything() {
        let out = SearchFilters::default().version_outcomes(&Provenance::default());
        assert!(out.language_target.is_none());
        assert!(out.sdk_dependency.is_none());
    }

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

    #[test]
    fn rejects_invalid_closed_enum_value() {
        let f = SearchFilters {
            kind: SetMatch {
                any_of: vec!["binary".to_owned()],
                none_of: vec![],
            },
            ..Default::default()
        };
        let err = f.validate().unwrap_err();
        assert_eq!(err.facet, "kind");
        assert!(err.message.contains("binary"));
    }

    #[test]
    fn rejects_negation_on_non_negatable_facet() {
        let f = SearchFilters {
            language_target: SetMatch {
                any_of: vec![],
                none_of: vec![LanguageTargetMatch {
                    name: "compact".into(),
                    version_satisfies: None,
                }],
            },
            ..Default::default()
        };
        let err = f.validate().unwrap_err();
        assert_eq!(err.facet, "language_target");
    }

    #[test]
    fn rejects_contradictory_numeric_range() {
        let f = SearchFilters {
            token_count: Some(NumericRange { min: Some(100), max: Some(10) }),
            ..Default::default()
        };
        assert!(f.validate().is_err());
    }

    #[test]
    fn rejects_contradictory_temporal_range() {
        let after = Date::from_calendar_date(2026, time::Month::June, 1).unwrap();
        let before = Date::from_calendar_date(2026, time::Month::January, 1).unwrap();
        let f = SearchFilters {
            ingested_at: Some(TemporalRange {
                after: Some(after),
                before: Some(before),
            }),
            ..Default::default()
        };
        assert!(f.validate().is_err());
    }

    #[test]
    fn rejects_malformed_version_satisfies() {
        let f = SearchFilters {
            language_target: SetMatch {
                any_of: vec![LanguageTargetMatch {
                    name: "compact".into(),
                    version_satisfies: Some("not-a-version".into()),
                }],
                none_of: vec![],
            },
            ..Default::default()
        };
        assert!(f.validate().is_err());
        // Ranges are accepted now (not just concrete versions): a well-formed
        // range alongside the garbage case above must still validate.
        let ranged = SearchFilters {
            language_target: SetMatch {
                any_of: vec![LanguageTargetMatch {
                    name: "compact".into(),
                    version_satisfies: Some(">=0.23".into()),
                }],
                none_of: vec![],
            },
            ..Default::default()
        };
        assert!(ranged.validate().is_ok());
    }

    #[test]
    fn valid_filter_passes() {
        let f = SearchFilters {
            kind: SetMatch {
                any_of: vec!["code".into()],
                none_of: vec![],
            },
            language: SetMatch {
                any_of: vec!["compact".into()],
                none_of: vec!["typescript".into()],
            },
            ..Default::default()
        };
        assert!(f.validate().is_ok());
    }

    #[test]
    fn accepts_arbitrary_symbol_kind() {
        // symbol.kind is open (chunker-derived syntactic vocabulary), so any
        // kind — including ones not in any fixed list — validates.
        let f = SearchFilters {
            symbol: SetMatch {
                any_of: vec![
                    SymbolMatch {
                        kind: Some("impl".into()),
                        name: Some("Foo".into()),
                    },
                    SymbolMatch {
                        kind: Some("method".into()),
                        name: None,
                    },
                    SymbolMatch {
                        kind: None,
                        name: Some("bar".into()),
                    },
                ],
                none_of: vec![],
            },
            ..Default::default()
        };
        assert!(f.validate().is_ok());
    }

    #[test]
    fn rejects_invalid_package_kind() {
        let f = SearchFilters {
            package: SetMatch {
                any_of: vec![PackageMatch {
                    kind: "pypi".into(),
                    name: "x".into(),
                }],
                none_of: vec![],
            },
            ..Default::default()
        };
        let err = f.validate().unwrap_err();
        assert_eq!(err.facet, "package");
    }

    #[test]
    fn accepts_range_version_satisfies() {
        for val in [">=0.23", "^1.2", "~1.4.2", "0.31"] {
            let f = SearchFilters {
                language_target: SetMatch {
                    any_of: vec![LanguageTargetMatch {
                        name: "compact".into(),
                        version_satisfies: Some(val.into()),
                    }],
                    none_of: vec![],
                },
                ..Default::default()
            };
            assert!(f.validate().is_ok(), "{val} should validate");
        }
        // empty interval still rejected
        let f = SearchFilters {
            language_target: SetMatch {
                any_of: vec![LanguageTargetMatch {
                    name: "compact".into(),
                    version_satisfies: Some(">=2.0, <1.0".into()),
                }],
                none_of: vec![],
            },
            ..Default::default()
        };
        assert!(f.validate().is_err());
    }

    #[test]
    fn version_outcomes_classify_per_facet() {
        use mn_core::version_match::MatchClass;
        let prov = Provenance {
            language_targets: vec![mn_core::provenance::LanguageTarget {
                name: "compact".into(),
                version_constraint: Some(">=0.23".into()),
            }],
            ..Provenance::default()
        };
        let mk = |ver: &str| SearchFilters {
            language_target: SetMatch {
                any_of: vec![LanguageTargetMatch {
                    name: "compact".into(),
                    version_satisfies: Some(ver.into()),
                }],
                none_of: vec![],
            },
            ..Default::default()
        };
        // satisfies
        let out = mk("0.31").version_outcomes(&prov);
        assert!(matches!(
            out.language_target,
            Some(FacetVersionOutcome::Classified {
                class: MatchClass::Satisfies,
                element: 0
            })
        ));
        assert!(out.sdk_dependency.is_none()); // unconstrained facet
                                               // breaking (0.x minor mismatch)
        let out = mk("0.10").version_outcomes(&prov);
        assert!(matches!(
            out.language_target,
            Some(FacetVersionOutcome::Classified {
                class: MatchClass::Breaking,
                ..
            })
        ));
        // silent: no matching-name target
        let out = mk("0.31").version_outcomes(&Provenance::default());
        assert!(matches!(out.language_target, Some(FacetVersionOutcome::Silent)));
        // name-only element (no version) against a declaring chunk → Satisfies
        let f = SearchFilters {
            language_target: SetMatch {
                any_of: vec![LanguageTargetMatch {
                    name: "compact".into(),
                    version_satisfies: None,
                }],
                none_of: vec![],
            },
            ..Default::default()
        };
        assert!(matches!(
            f.version_outcomes(&prov).language_target,
            Some(FacetVersionOutcome::Classified {
                class: MatchClass::Satisfies,
                ..
            })
        ));
    }
}

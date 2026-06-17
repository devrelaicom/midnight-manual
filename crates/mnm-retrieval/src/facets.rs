//! The facet registry — the single source of truth for the v1 filter universe.
//!
//! It declares which filters exist, their type, whether they support negation,
//! and (for closed enums) their allowed values. The server SQL predicate
//! builder, the `/v1/facets` discovery endpoint, and filter validation all read
//! from here so the advertised facet set can never drift from what is actually
//! enforced.

/// The shape family of a facet, which determines its wire form and SQL mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacetType {
    /// Closed-set string membership (`{any_of, none_of}`), values enumerated here.
    Enum,
    /// Open-set string membership (`{any_of, none_of}`), values corpus-derived.
    OpenSet,
    /// Structured element membership (`{any_of: [{...}], none_of: [{...}]}`).
    ObjectSet,
    /// A bare boolean.
    Bool,
    /// `{after?, before?}` over a timestamp column.
    RangeTemporal,
    /// `{min?, max?}` over an integer column.
    RangeNumeric,
}

/// One facet's metadata.
///
/// The server's predicate builder derives the SQL mapping from `key` +
/// `facet_type`; this registry is the only place the facet universe is
/// declared, so advertised facets cannot drift from what is enforced.
#[derive(Debug, Clone, Copy)]
pub struct FacetDescriptor {
    /// Stable wire identifier for the facet (e.g. `"attribution"`).
    pub key: &'static str,
    /// The facet's shape family, which determines its wire form and SQL mapping.
    pub facet_type: FacetType,
    /// Whether the facet accepts a `none_of` negation clause.
    pub negatable: bool,
    /// `Some` iff `facet_type == Enum`: the exhaustive allowed values.
    pub closed_values: Option<&'static [&'static str]>,
}

/// Allowed values for the `kind` facet (chunk content kind). Kept as consts so
/// tests and the discovery endpoint reference the same arrays.
pub const KIND_VALUES: &[&str] = &["markdown", "code", "plaintext"];
/// Allowed values for the `source_kind` facet.
pub const SOURCE_KIND_VALUES: &[&str] = &["docs_site", "code_repo", "standalone", "mixed"];
/// Allowed values for the `attribution` facet (provenance attribution).
pub const ATTRIBUTION_VALUES: &[&str] = &[
    "foundation",
    "partner",
    "third_party",
    "community",
    "unknown",
];
/// Allowed values for the `content_type` facet.
pub const CONTENT_TYPE_VALUES: &[&str] = &[
    "doc",
    "tutorial",
    "reference",
    "example",
    "contract_source",
    "sdk_source",
    "test",
    "readme",
    "other",
];
/// `package.kind` is closed even though `package.name` is open.
pub const PACKAGE_KIND_VALUES: &[&str] = &["rust", "npm", "compact", "other"];

/// The complete v1 facet registry.
///
/// `#[rustfmt::skip]` keeps the descriptors as an aligned one-row-per-facet
/// table (the plan's intended layout); it also keeps the function under the
/// `clippy::too_many_lines` threshold that the expanded form trips.
#[rustfmt::skip]
#[must_use]
pub const fn facets() -> &'static [FacetDescriptor] {
    use FacetType::{Bool, Enum, ObjectSet, OpenSet, RangeNumeric, RangeTemporal};
    &[
        FacetDescriptor { key: "attribution",        facet_type: Enum,          negatable: true,  closed_values: Some(ATTRIBUTION_VALUES) },
        FacetDescriptor { key: "content_type",       facet_type: Enum,          negatable: true,  closed_values: Some(CONTENT_TYPE_VALUES) },
        FacetDescriptor { key: "kind",               facet_type: Enum,          negatable: true,  closed_values: Some(KIND_VALUES) },
        FacetDescriptor { key: "source_kind",        facet_type: Enum,          negatable: true,  closed_values: Some(SOURCE_KIND_VALUES) },
        FacetDescriptor { key: "source_slug",        facet_type: OpenSet,       negatable: true,  closed_values: None },
        FacetDescriptor { key: "language",           facet_type: OpenSet,       negatable: true,  closed_values: None },
        FacetDescriptor { key: "tags",               facet_type: OpenSet,       negatable: true,  closed_values: None },
        FacetDescriptor { key: "heading_path",       facet_type: OpenSet,       negatable: true,  closed_values: None },
        FacetDescriptor { key: "symbol",             facet_type: ObjectSet,     negatable: true,  closed_values: None },
        FacetDescriptor { key: "package",            facet_type: ObjectSet,     negatable: true,  closed_values: None },
        FacetDescriptor { key: "verified",           facet_type: Bool,          negatable: false, closed_values: None },
        FacetDescriptor { key: "deprecated",         facet_type: Bool,          negatable: false, closed_values: None },
        FacetDescriptor { key: "language_target",    facet_type: ObjectSet,     negatable: false, closed_values: None },
        FacetDescriptor { key: "sdk_dependency",     facet_type: ObjectSet,     negatable: false, closed_values: None },
        FacetDescriptor { key: "ingested_at",        facet_type: RangeTemporal, negatable: false, closed_values: None },
        FacetDescriptor { key: "source_modified_at", facet_type: RangeTemporal, negatable: false, closed_values: None },
        FacetDescriptor { key: "token_count",        facet_type: RangeNumeric,  negatable: false, closed_values: None },
    ]
}

/// Look up one descriptor by key.
#[must_use]
pub fn lookup(key: &str) -> Option<&'static FacetDescriptor> {
    facets().iter().find(|f| f.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_expected_keys_and_no_dupes() {
        let keys: Vec<&str> = facets().iter().map(|f| f.key).collect();
        // Spot-check representative members of each type.
        for expected in [
            "attribution",
            "kind",
            "language",
            "symbol",
            "deprecated",
            "ingested_at",
            "token_count",
        ] {
            assert!(keys.contains(&expected), "missing facet `{expected}`");
        }
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "duplicate facet keys in registry");
    }

    #[test]
    fn closed_enums_carry_values_open_sets_do_not() {
        let kind = lookup("kind").unwrap();
        assert!(matches!(kind.facet_type, FacetType::Enum));
        assert!(kind.closed_values.is_some(), "closed enum must list values");

        let language = lookup("language").unwrap();
        assert!(matches!(language.facet_type, FacetType::OpenSet));
        assert!(language.closed_values.is_none(), "open set must not hard-code values");

        // Guard the "Some iff Enum" invariant across the whole registry so a
        // future mis-entry (e.g. an OpenSet given closed_values) fails loudly.
        for desc in facets() {
            assert_eq!(
                desc.closed_values.is_some(),
                desc.facet_type == FacetType::Enum,
                "`{}`: closed_values must be Some iff facet_type == Enum",
                desc.key
            );
        }
    }

    #[test]
    fn ranges_and_semver_are_not_negatable() {
        for k in [
            "ingested_at",
            "token_count",
            "language_target",
            "sdk_dependency",
        ] {
            assert!(!lookup(k).unwrap().negatable, "`{k}` must not be negatable");
        }
        assert!(lookup("language").unwrap().negatable, "open sets are negatable");
    }

    #[test]
    fn registry_is_exactly_v1_keys() {
        // Pins the registry to the v1 facet set. The skill docs mirror this list
        // in `mnm-skills` (`catalog_documents_every_facet_key`). Changing the
        // registry must be a deliberate edit here AND in
        // `crates/mnm-skills/assets/midnight-advanced-search/references/filters-and-modes.md`.
        let mut got: Vec<&str> = facets().iter().map(|f| f.key).collect();
        got.sort_unstable();
        let mut want = [
            "attribution",
            "content_type",
            "kind",
            "source_kind",
            "source_slug",
            "language",
            "tags",
            "heading_path",
            "symbol",
            "package",
            "verified",
            "deprecated",
            "language_target",
            "sdk_dependency",
            "ingested_at",
            "source_modified_at",
            "token_count",
        ];
        want.sort_unstable();
        assert_eq!(got, want, "registry facet set drifted from the documented v1 catalog");
    }
}

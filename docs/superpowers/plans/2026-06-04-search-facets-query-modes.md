# Search Facets & Query-Mode Switching — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `/v1/search` clients (esp. MCP agents) multi-facet filtering and query-mode selection (hybrid/vector/fts), with a discoverable, fail-fast filter surface.

**Architecture:** A uniform per-facet match model (`any_of`/`none_of` + ranges) in `mn-retrieval`, driven by a single static **facet registry** that both the server SQL predicate builder and a new `/v1/facets` discovery endpoint read from (so advertised ≡ enforced). A per-request `mode` enum gates which retrieval halves run; `fts` mode skips embedding entirely. MCP gets a typed input schema + `facets` tool; the CLI gets filter flags + a `facets` subcommand.

**Tech Stack:** Rust (stable, MSRV 1.91), `axum`, `sqlx`/`QueryBuilder` (Postgres + pgvector), `serde`/`serde_json`, `clap` v4, `proptest`, `testcontainers`. Errors via `mn_core::error::{Error, ErrorCode}`.

**Spec:** `docs/superpowers/specs/2026-06-04-search-facets-query-modes-design.md`.

---

## Scope Check

This is one subsystem (search), but large. It is structured as five sequential phases. Phase A is pure logic (DB-free, fully unit/proptestable). Phases B–E wire it through the server, MCP, CLI, and contracts. C and D depend only on A+B and could be split into a follow-up plan if desired, but are kept here for one coherent feature. **Out of scope (Phase 2, separate specs):** net-new indexed facets (verified-code, referenced-symbols, difficulty) and the recall harness.

## File Structure

**Created:**
- `crates/mn-retrieval/src/facets.rs` — the facet registry (`FacetDescriptor`, `FacetType`, `facets()`), the single source of truth for keys / closed values / negatability / SQL mapping metadata.
- `crates/mn-server/src/routes/facets.rs` — `GET /v1/facets` handler + TTL cache.
- `crates/mn-server/tests/facets_route.rs` — integration tests for `/v1/facets`.
- `crates/mn-server/tests/search_filters.rs` — integration tests for the new facets + modes against testcontainers.

**Modified:**
- `crates/mn-retrieval/src/filters.rs` — rewrite `SearchFilters` to the per-facet match model + `validate()`.
- `crates/mn-retrieval/src/lib.rs` — add `pub mod facets;`.
- `crates/mn-server/src/routes/search.rs` — add `mode`, rewrite `push_filter_*`/`needs_document_join` to the registry-driven builder, mode-gate retrieval + guards.
- `crates/mn-server/src/routes/mod.rs` + `crates/mn-server/src/app.rs` — register the facets router.
- `crates/mn-mcp/src/tools.rs` — typed `search_input_schema()`, add `facets` tool + handler.
- `crates/mn-mcp/src/cloud_client.rs` — add `mode` to `SearchRequest`; add `get_facets()`.
- `crates/mn-mcp/src/server.rs` — dispatch `facets`; add `McpToolName::Facets`.
- `crates/mn-telemetry/src/events.rs` — add `McpToolName::Facets`.
- `crates/mn-cli/src/commands/search.rs` — filter/mode flags → `SearchFilters`.
- `crates/mn-cli/src/commands/facets.rs` (new) + `crates/mn-cli/src/cli.rs` — `mnm facets` subcommand.
- `specs/001-rag-platform/contracts/openapi.yaml` + `mcp-tools.json` — update to new shapes.

---

# Phase A — Filter model + registry (`mn-retrieval`)

Pure logic, no DB. This is the contract every later phase consumes.

### Task A1: Facet registry

**Files:**
- Create: `crates/mn-retrieval/src/facets.rs`
- Modify: `crates/mn-retrieval/src/lib.rs`

- [ ] **Step 1: Add the module declaration**

In `crates/mn-retrieval/src/lib.rs`, under `pub mod filters;` add:

```rust
pub mod facets;
```

- [ ] **Step 2: Write the failing test**

Create `crates/mn-retrieval/src/facets.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_expected_keys_and_no_dupes() {
        let keys: Vec<&str> = facets().iter().map(|f| f.key).collect();
        // Spot-check representative members of each type.
        for expected in ["attribution", "kind", "language", "symbol", "deprecated", "ingested_at", "token_count"] {
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
    }

    #[test]
    fn ranges_and_semver_are_not_negatable() {
        for k in ["ingested_at", "token_count", "language_target", "sdk_dependency"] {
            assert!(!lookup(k).unwrap().negatable, "`{k}` must not be negatable");
        }
        assert!(lookup("language").unwrap().negatable, "open sets are negatable");
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p mn-retrieval facets::tests -- --nocapture`
Expected: FAIL to compile (`facets`, `FacetType`, `lookup` not defined).

- [ ] **Step 4: Implement the registry**

Prepend to `crates/mn-retrieval/src/facets.rs`:

```rust
//! The facet registry — the single source of truth for which filters exist,
//! their type, whether they support negation, and (for closed enums) their
//! allowed values. The server SQL predicate builder, the `/v1/facets`
//! discovery endpoint, and filter validation all read from here so the
//! advertised facet set can never drift from what is actually enforced.

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

/// One facet's metadata. `sql_target` is an opaque tag the server's predicate
/// builder switches on (kept here so the registry is the only place the facet
/// universe is declared).
#[derive(Debug, Clone, Copy)]
pub struct FacetDescriptor {
    pub key: &'static str,
    pub facet_type: FacetType,
    pub negatable: bool,
    /// `Some` iff `facet_type == Enum`: the exhaustive allowed values.
    pub closed_values: Option<&'static [&'static str]>,
}

/// Closed-enum value lists. Kept as consts so tests and the discovery endpoint
/// reference the same arrays.
pub const KIND_VALUES: &[&str] = &["markdown", "code", "plaintext"];
pub const SOURCE_KIND_VALUES: &[&str] = &["docs_site", "code_repo", "standalone", "mixed"];
pub const ATTRIBUTION_VALUES: &[&str] =
    &["foundation", "partner", "third_party", "community", "unknown"];
pub const CONTENT_TYPE_VALUES: &[&str] = &[
    "doc", "tutorial", "reference", "example", "contract_source", "sdk_source", "test", "readme",
    "other",
];
/// `symbol.kind` is a small closed set even though `symbol.name` is open.
pub const SYMBOL_KIND_VALUES: &[&str] =
    &["fn", "struct", "circuit", "witness", "ledger", "module", "enum"];
/// `package.kind` is closed even though `package.name` is open.
pub const PACKAGE_KIND_VALUES: &[&str] = &["rust", "npm", "compact", "other"];

/// The complete v1 facet registry.
#[must_use]
pub fn facets() -> &'static [FacetDescriptor] {
    use FacetType::{Bool, Enum, ObjectSet, OpenSet, RangeNumeric, RangeTemporal};
    &[
        FacetDescriptor { key: "attribution",  facet_type: Enum,    negatable: true,  closed_values: Some(ATTRIBUTION_VALUES) },
        FacetDescriptor { key: "content_type", facet_type: Enum,    negatable: true,  closed_values: Some(CONTENT_TYPE_VALUES) },
        FacetDescriptor { key: "kind",         facet_type: Enum,    negatable: true,  closed_values: Some(KIND_VALUES) },
        FacetDescriptor { key: "source_kind",  facet_type: Enum,    negatable: true,  closed_values: Some(SOURCE_KIND_VALUES) },
        FacetDescriptor { key: "source_slug",  facet_type: OpenSet, negatable: true,  closed_values: None },
        FacetDescriptor { key: "language",     facet_type: OpenSet, negatable: true,  closed_values: None },
        FacetDescriptor { key: "tags",         facet_type: OpenSet, negatable: true,  closed_values: None },
        FacetDescriptor { key: "heading_path", facet_type: OpenSet, negatable: true,  closed_values: None },
        FacetDescriptor { key: "symbol",       facet_type: ObjectSet, negatable: true, closed_values: None },
        FacetDescriptor { key: "package",      facet_type: ObjectSet, negatable: true, closed_values: None },
        FacetDescriptor { key: "verified",     facet_type: Bool,    negatable: false, closed_values: None },
        FacetDescriptor { key: "deprecated",   facet_type: Bool,    negatable: false, closed_values: None },
        FacetDescriptor { key: "language_target", facet_type: ObjectSet, negatable: false, closed_values: None },
        FacetDescriptor { key: "sdk_dependency",  facet_type: ObjectSet, negatable: false, closed_values: None },
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mn-retrieval facets::tests`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/mn-retrieval/src/facets.rs crates/mn-retrieval/src/lib.rs
git commit -m "feat(retrieval): facet registry — single source of truth for v1 facets"
```

---

### Task A2: Per-facet match types

**Files:**
- Modify: `crates/mn-retrieval/src/filters.rs` (full rewrite of the type definitions; keep the file)

- [ ] **Step 1: Write the failing test**

Replace the existing `#[cfg(test)] mod tests` block in `filters.rs` with this round-trip test (old tests reference removed fields and must go):

```rust
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
        let back: SearchFilters = serde_json::from_value(serde_json::to_value(&f).unwrap()).unwrap();
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
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p mn-retrieval filters::tests::full_shape_round_trips`
Expected: FAIL to compile (new types not defined; old `SearchFilters` fields gone).

- [ ] **Step 3: Replace the type definitions**

Replace everything in `filters.rs` *above* the test module (the old `SearchFilters`, `LanguageTargetFilter`, `SdkDependencyFilter`, `PackageFilter`, `impl`, and `version_satisfies`) with:

```rust
//! Per-facet match model for `POST /v1/search` filters (see
//! docs/superpowers/specs/2026-06-04-search-facets-query-modes-design.md).
//!
//! Every facet is one of: a set-match (`{any_of, none_of}`) over strings or
//! structured elements, a bare bool, or a range. Combination is AND across
//! facets, OR within `any_of`, exclude `none_of`. The semver-bearing facets
//! (`language_target`, `sdk_dependency`) carry a `version_satisfies` field
//! evaluated in [`SearchFilters::semver_post_match`], not SQL.

use mn_core::provenance::Provenance;
use mn_core::scoring::parse_version;
use serde::{Deserialize, Serialize};
use time::Date;

/// Set membership for one facet: OR within `any_of`, exclude `none_of`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMatch<T> {
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub any_of: Vec<T>,
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub none_of: Vec<T>,
}

impl<T> SetMatch<T> {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.any_of.is_empty() && self.none_of.is_empty()
    }
}

/// One `symbol` element matcher (`chunk.symbol_path` JSONB containment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One `package` element matcher (`chunk -> document -> package`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageMatch {
    pub kind: String,
    pub name: String,
}

/// One `language_target` element (semver evaluated in the Rust post-match).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageTargetMatch {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_satisfies: Option<String>,
}

/// One `sdk_dependency` element (semver evaluated in the Rust post-match).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkDependencyMatch {
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_satisfies: Option<String>,
}

/// `{after?, before?}` inclusive temporal range (ISO dates).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Date>,
}

/// `{min?, max?}` inclusive numeric range.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumericRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
}

/// Top-level filter object. Every field defaults to empty/absent so a missing
/// `filters` key means "no constraints". `deny_unknown_fields` makes a
/// misspelled facet a hard error instead of a silent drop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchFilters {
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub attribution: SetMatch<String>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub content_type: SetMatch<String>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub kind: SetMatch<String>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub source_kind: SetMatch<String>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub source_slug: SetMatch<String>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub language: SetMatch<String>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub tags: SetMatch<String>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub heading_path: SetMatch<String>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub symbol: SetMatch<SymbolMatch>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub package: SetMatch<PackageMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub language_target: SetMatch<LanguageTargetMatch>,
    #[serde(default, skip_serializing_if = "SetMatch::is_empty")]
    pub sdk_dependency: SetMatch<SdkDependencyMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingested_at: Option<TemporalRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_modified_at: Option<TemporalRange>,
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
```

> Note: `language_target` is now a `SetMatch` (object-set), a deliberate change from the old single-object field — uniform with the model. No back-compat is required (project is unreleased).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p mn-retrieval filters::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-retrieval/src/filters.rs
git commit -m "feat(retrieval): rewrite SearchFilters to per-facet match model"
```

---

### Task A3: Filter validation

**Files:**
- Modify: `crates/mn-retrieval/src/filters.rs`

- [ ] **Step 1: Write the failing tests**

Add to `filters.rs` `mod tests`:

```rust
    #[test]
    fn rejects_invalid_closed_enum_value() {
        let f = SearchFilters {
            kind: SetMatch { any_of: vec!["binary".to_owned()], none_of: vec![] },
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
                none_of: vec![LanguageTargetMatch { name: "compact".into(), version_satisfies: None }],
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
            ingested_at: Some(TemporalRange { after: Some(after), before: Some(before) }),
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
    }

    #[test]
    fn valid_filter_passes() {
        let f = SearchFilters {
            kind: SetMatch { any_of: vec!["code".into()], none_of: vec![] },
            language: SetMatch { any_of: vec!["compact".into()], none_of: vec!["typescript".into()] },
            ..Default::default()
        };
        assert!(f.validate().is_ok());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mn-retrieval filters::tests::valid_filter_passes`
Expected: FAIL to compile (`validate`, `FilterError` not defined).

- [ ] **Step 3: Implement validation**

Add to `filters.rs` (after the `impl SearchFilters` block), using the registry from Task A1:

```rust
use crate::facets::{self, FacetType};

/// A filter validation failure, naming the offending facet. The server maps
/// this to a 400 `invalid_request` with a remediation pointing at `/v1/facets`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterError {
    pub facet: String,
    pub message: String,
}

impl FilterError {
    fn new(facet: &str, message: impl Into<String>) -> Self {
        Self { facet: facet.to_owned(), message: message.into() }
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
        self.check_string_set("attribution", &self.attribution)?;
        self.check_string_set("content_type", &self.content_type)?;
        self.check_string_set("kind", &self.kind)?;
        self.check_string_set("source_kind", &self.source_kind)?;
        self.check_string_set("source_slug", &self.source_slug)?;
        self.check_string_set("language", &self.language)?;
        self.check_string_set("tags", &self.tags)?;
        self.check_string_set("heading_path", &self.heading_path)?;
        check_negatable("symbol", &self.symbol)?;
        check_negatable("package", &self.package)?;
        // language_target / sdk_dependency: not negatable + semver parseable.
        check_negatable("language_target", &self.language_target)?;
        check_negatable("sdk_dependency", &self.sdk_dependency)?;
        for lt in &self.language_target.any_of {
            check_semver("language_target", lt.version_satisfies.as_deref())?;
        }
        for dep in &self.sdk_dependency.any_of {
            check_semver("sdk_dependency", dep.version_satisfies.as_deref())?;
        }
        check_numeric_range(self.token_count.as_ref())?;
        check_temporal_range("ingested_at", self.ingested_at.as_ref())?;
        check_temporal_range("source_modified_at", self.source_modified_at.as_ref())?;
        Ok(())
    }

    fn check_string_set(&self, key: &str, set: &SetMatch<String>) -> Result<(), FilterError> {
        let desc = facets::lookup(key).expect("registry key");
        if !desc.negatable && !set.none_of.is_empty() {
            return Err(FilterError::new(key, format!("`{key}` does not support `none_of`")));
        }
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
}

fn check_negatable<T>(key: &str, set: &SetMatch<T>) -> Result<(), FilterError> {
    let desc = facets::lookup(key).expect("registry key");
    if !desc.negatable && !set.none_of.is_empty() {
        return Err(FilterError::new(key, format!("`{key}` does not support `none_of`")));
    }
    Ok(())
}

fn check_semver(key: &str, constraint: Option<&str>) -> Result<(), FilterError> {
    if let Some(c) = constraint {
        if parse_version(c).is_none() {
            return Err(FilterError::new(key, format!("`{c}` is not a valid version")));
        }
    }
    Ok(())
}

fn check_numeric_range(r: Option<&NumericRange>) -> Result<(), FilterError> {
    if let Some(r) = r {
        if let (Some(min), Some(max)) = (r.min, r.max) {
            if min > max {
                return Err(FilterError::new("token_count", "min must be <= max"));
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
```

> `parse_version` already exists in `mn_core::scoring` (used by the old `version_satisfies`). Confirm its signature returns `Option<semver::Version>`; if it instead parses a `VersionReq`, swap to `semver::VersionReq::parse(c).is_ok()`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mn-retrieval filters::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-retrieval/src/filters.rs
git commit -m "feat(retrieval): registry-driven filter validation (fail-fast)"
```

---

### Task A4: Preserve the semver post-match

**Files:**
- Modify: `crates/mn-retrieval/src/filters.rs`

The server applies semver refinements in Rust (Postgres has no semver type). Re-add an adapted `semver_post_match` for the new shape.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    use mn_core::provenance::{LanguageTarget, Provenance};

    #[test]
    fn semver_post_match_filters_by_version() {
        let prov = Provenance {
            language_targets: vec![LanguageTarget { name: "compact".into(), version_constraint: Some(">=0.23".into()) }],
            ..Provenance::default()
        };
        let satisfies = SearchFilters {
            language_target: SetMatch {
                any_of: vec![LanguageTargetMatch { name: "compact".into(), version_satisfies: Some("0.31".into()) }],
                none_of: vec![],
            },
            ..Default::default()
        };
        assert!(satisfies.semver_post_match(&prov));
        let misses = SearchFilters {
            language_target: SetMatch {
                any_of: vec![LanguageTargetMatch { name: "compact".into(), version_satisfies: Some("0.10".into()) }],
                none_of: vec![],
            },
            ..Default::default()
        };
        assert!(!misses.semver_post_match(&prov));
    }

    #[test]
    fn empty_filter_post_matches_anything() {
        assert!(SearchFilters::default().semver_post_match(&Provenance::default()));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mn-retrieval filters::tests::semver_post_match_filters_by_version`
Expected: FAIL to compile (`semver_post_match` not defined).

- [ ] **Step 3: Implement (port the old logic to the new shape)**

Add to `impl SearchFilters`:

```rust
    /// Evaluate the semver refinements Postgres can't express against a chunk's
    /// provenance. `language_target` / `sdk_dependency` use OR-within-`any_of`
    /// semantics: the chunk passes if it matches at least one element (name +
    /// version). Facets with empty `any_of` impose no constraint.
    #[must_use]
    pub fn semver_post_match(&self, provenance: &Provenance) -> bool {
        if !self.language_target.any_of.is_empty() {
            let ok = self.language_target.any_of.iter().any(|want| {
                provenance.language_targets.iter().any(|have| {
                    have.name.eq_ignore_ascii_case(&want.name)
                        && version_satisfies(want.version_satisfies.as_deref(), have.version_constraint.as_deref())
                })
            });
            if !ok {
                return false;
            }
        }
        if !self.sdk_dependency.any_of.is_empty() {
            let ok = self.sdk_dependency.any_of.iter().any(|want| {
                provenance.sdk_dependencies.iter().any(|have| {
                    have.kind.eq_ignore_ascii_case(&want.kind)
                        && have.name == want.name
                        && version_satisfies(want.version_satisfies.as_deref(), have.version_constraint.as_deref())
                })
            });
            if !ok {
                return false;
            }
        }
        true
    }
```

And re-add the free function (unchanged from the original implementation):

```rust
/// Does a requested version satisfy a chunk's declared constraint? `None`
/// request always passes; an unconstrained chunk passes any request; a chunk
/// constraint that fails to parse never satisfies.
fn version_satisfies(requested: Option<&str>, chunk_constraint: Option<&str>) -> bool {
    let Some(req) = requested else { return true };
    let Some(candidate) = parse_version(req) else { return true };
    chunk_constraint.is_none_or(|c| semver::VersionReq::parse(c).is_ok_and(|r| r.matches(&candidate)))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mn-retrieval filters::tests`
Expected: PASS (all filter tests).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-retrieval/src/filters.rs
git commit -m "feat(retrieval): port semver post-match to the new object-set shape"
```

---

### Task A5: Property tests

**Files:**
- Create: `crates/mn-retrieval/tests/filter_properties.rs`

- [ ] **Step 1: Write the property tests**

```rust
//! Property tests for the filter model: serde round-trip identity and
//! validation stability.
use mn_retrieval::filters::{SearchFilters, SetMatch};
use proptest::prelude::*;

fn arb_string_set() -> impl Strategy<Value = SetMatch<String>> {
    (
        prop::collection::vec("[a-z_]{1,8}", 0..4),
        prop::collection::vec("[a-z_]{1,8}", 0..4),
    )
        .prop_map(|(any_of, none_of)| SetMatch { any_of, none_of })
}

proptest! {
    /// Any filter built from arbitrary open-set string members round-trips
    /// through JSON unchanged.
    #[test]
    fn open_set_filters_round_trip(language in arb_string_set(), tags in arb_string_set()) {
        let f = SearchFilters { language, tags, ..Default::default() };
        let back: SearchFilters = serde_json::from_value(serde_json::to_value(&f).unwrap()).unwrap();
        prop_assert_eq!(f, back);
    }

    /// validate() never panics on arbitrary open-set input (open sets accept
    /// any string), and only ever returns Err via the typed FilterError path.
    #[test]
    fn validate_is_total_on_open_sets(language in arb_string_set()) {
        let f = SearchFilters { language, ..Default::default() };
        let _ = f.validate(); // must not panic
    }
}
```

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test -p mn-retrieval --test filter_properties`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/mn-retrieval/tests/filter_properties.rs
git commit -m "test(retrieval): property tests for filter round-trip + validation totality"
```

- [ ] **Step 4: Phase gate — full crate check**

Run: `cargo test -p mn-retrieval && cargo clippy -p mn-retrieval --all-targets -- -D warnings`
Expected: PASS, no warnings.

---

# Phase B — Server: query modes, SQL builder, `/v1/facets`

### Task B1: `SortBy` unchanged; add `SearchMode` enum + request field

**Files:**
- Modify: `crates/mn-server/src/routes/search.rs`

- [ ] **Step 1: Write the failing test**

In `search.rs` `mod tests`, add:

```rust
    #[test]
    fn mode_defaults_to_hybrid_and_parses() {
        let body = serde_json::json!({
            "queries": [{ "text": "x", "vector": [0.0] }],
            "client_embedding_model": "voyage-code-3@1"
        });
        let req: SearchRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.mode, SearchMode::Hybrid);

        let body2 = serde_json::json!({
            "query": "x", "vector": [0.0],
            "client_embedding_model": "voyage-code-3@1", "mode": "fts"
        });
        let req2: SearchRequest = serde_json::from_value(body2).unwrap();
        assert_eq!(req2.mode, SearchMode::Fts);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mn-server search::tests::mode_defaults_to_hybrid_and_parses`
Expected: FAIL to compile (`SearchMode`, `req.mode` not defined).

- [ ] **Step 3: Add the enum + field**

After the `SortBy` enum in `search.rs`, add:

```rust
/// Which retrieval halves to run (per-request).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Run both pgvector and FTS, fuse via RRF. The default.
    #[default]
    Hybrid,
    /// pgvector only. Requires `vector` + `client_embedding_model`.
    Vector,
    /// FTS only. `vector` / `client_embedding_model` are optional and ignored.
    Fts,
}
```

In `struct SearchRequest`, add the field (after `sort_by`):

```rust
    /// Which retrieval halves to run. Defaults to `hybrid`.
    #[serde(default)]
    pub mode: SearchMode,
```

Also relax `client_embedding_model` to optional (FTS mode supplies no model):

```rust
    /// The embedding model identifier the client used. REQUIRED for `hybrid`
    /// and `vector` modes; optional/ignored for `fts` mode.
    #[serde(default)]
    pub client_embedding_model: Option<String>,
```

> This changes `client_embedding_model` from `String` to `Option<String>`. Update its existing reads in the handler in Task B2.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p mn-server search::tests::mode_defaults_to_hybrid_and_parses`
Expected: PASS (the handler may not yet compile — fix in B2; if so, this test still needs the crate to build, so do B2 in the same working session before running the whole suite).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/routes/search.rs
git commit -m "feat(server): add SearchMode enum + optional client_embedding_model"
```

---

### Task B2: Mode-gate guards + retrieval loop

**Files:**
- Modify: `crates/mn-server/src/routes/search.rs` (handler body, lines ~269-428)

- [ ] **Step 1: Mode-gate the model + dim guards**

Replace the model-mismatch + vector-dim guard blocks (currently `search.rs:287-317`) so they only run when the mode needs a vector. Insert a helper near the top of the handler after `let cm = ...`:

```rust
    let needs_vector = matches!(req.mode, SearchMode::Hybrid | SearchMode::Vector);
```

Wrap the existing model-match guard:

```rust
    if needs_vector {
        let Some(client_model) = req.client_embedding_model.as_deref() else {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message("client_embedding_model is required for hybrid/vector mode")
                    .remediation("supply client_embedding_model, or use mode=fts to skip embedding")
                    .build(),
                rid,
            );
        };
        if client_model != cm.wire {
            return error::into_response(
                CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                    .message(format!(
                        "client_embedding_model `{client_model}` does not match corpus model `{}`",
                        cm.wire,
                    ))
                    .remediation("re-run `mnm models pull` to fetch the corpus model")
                    .context("corpus_model", cm.wire.clone())
                    .context("client_model", client_model.to_owned())
                    .build(),
                rid,
            );
        }
        // Vector-dim guard (moved inside `needs_vector`).
        for (i, q) in queries.iter().enumerate() {
            if q.vector.len() != cm.dim {
                return error::into_response(
                    CoreError::builder(ErrorCode::InvalidRequest)
                        .message(format!(
                            "queries[{i}].vector has {} dimensions; expected {}",
                            q.vector.len(), cm.dim,
                        ))
                        .remediation("re-embed with the corpus model (run `mnm models pull`)")
                        .build(),
                    rid,
                );
            }
        }
    }
```

> `QueryPair.vector` stays a `Vec<f32>` on the wire but is allowed to be empty in `fts` mode. Make it `#[serde(default)]` in the `QueryPair` struct so `fts` callers can omit it.

- [ ] **Step 2: Add filter validation at the boundary**

Immediately after computing `limit`, validate filters (uses Task A3):

```rust
    if let Err(e) = req.filters.validate() {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!("invalid filter `{}`: {}", e.facet, e.message))
                .remediation("see GET /v1/facets for valid facets and values")
                .context("facet", e.facet)
                .build(),
            rid,
        );
    }
```

- [ ] **Step 3: Mode-gate the retrieval loop**

In the `for (i, q) in distinct.iter().enumerate()` loop (`search.rs:375-428`), run each half conditionally:

```rust
        let run_vector = matches!(req.mode, SearchMode::Hybrid | SearchMode::Vector);
        let run_fts = matches!(req.mode, SearchMode::Hybrid | SearchMode::Fts);

        let mut vector_hits = Vec::new();
        let mut vector_latency_ms = 0.0;
        if run_vector {
            let t0 = std::time::Instant::now();
            vector_hits = match vector_search(&state.pool, &q.vector, &req.filters, corpus_model_id).await {
                Ok(hits) => hits,
                Err(e) => {
                    tracing::warn!(request_id = rid, error = %e, query_index = i, "vector search failed");
                    return error::service_unavailable(format!("vector search failed for query {i}"), rid);
                }
            };
            vector_latency_ms = t0.elapsed().as_secs_f64() * 1000.0;
        }

        let mut fts_hits = Vec::new();
        let mut fts_latency_ms = 0.0;
        if run_fts {
            let t1 = std::time::Instant::now();
            fts_hits = match fts_search(&state.pool, &q.text, &req.filters, corpus_model_id).await {
                Ok(hits) => hits,
                Err(e) => {
                    tracing::warn!(request_id = rid, error = %e, query_index = i, "fts search failed");
                    return error::service_unavailable(format!("fts search failed for query {i}"), rid);
                }
            };
            fts_latency_ms = t1.elapsed().as_secs_f64() * 1000.0;
        }
```

Keep the rest of the loop body (per_query push, `matched`/`best_similarity` updates, `ranked_lists.push`) — but only push a list when its half ran:

```rust
        if run_vector { ranked_lists.push(vector_ids); }
        if run_fts { ranked_lists.push(fts_hits); }
```

> The empty-text guard at `search.rs:258` rejects all-empty text. In `fts` mode that is still correct (FTS needs text). In `vector` mode, empty text is harmless but the guard still requires non-empty text — relax it to only apply when `run_fts`:
> ```rust
> if matches!(req.mode, SearchMode::Hybrid | SearchMode::Fts)
>     && queries.iter().all(|q| q.text.trim().is_empty()) { /* ...existing 400... */ }
> ```

- [ ] **Step 4: Run unit tests**

Run: `cargo test -p mn-server search::tests`
Expected: PASS. Fix any remaining `client_embedding_model` (now `Option`) read sites the compiler flags.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/routes/search.rs
git commit -m "feat(server): mode-gate guards and retrieval halves; validate filters"
```

---

### Task B3: Registry-driven SQL predicate builder

**Files:**
- Modify: `crates/mn-server/src/routes/search.rs` (`push_filter_joins`, `push_filter_predicates`, `needs_document_join`)

- [ ] **Step 1: Write the failing SQL-shape test**

Add to `search.rs` `mod tests` (these assert the built SQL string contains the right fragments — mirrors how `QueryBuilder` is testable via `.into_sql()` after `build()`; if `into_sql` is unavailable, assert via a small helper that pushes into a fresh builder and inspects `sql()`):

```rust
    use mn_retrieval::filters::{SearchFilters, SetMatch, NumericRange};

    fn built_sql(filters: &SearchFilters) -> String {
        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> =
            sqlx::QueryBuilder::new("SELECT chunk.id FROM chunk JOIN source_version sv ON sv.id = chunk.source_version_id");
        super::push_filter_joins(&mut qb, filters);
        qb.push(" WHERE true");
        super::push_filter_predicates(&mut qb, filters);
        qb.sql().to_owned()
    }

    #[test]
    fn kind_emits_document_join_and_any_predicate() {
        let f = SearchFilters { kind: SetMatch { any_of: vec!["code".into()], none_of: vec![] }, ..Default::default() };
        let sql = built_sql(&f);
        assert!(sql.contains("JOIN document d"), "kind needs the document join");
        assert!(sql.contains("d.kind = ANY("), "got: {sql}");
    }

    #[test]
    fn language_none_of_emits_not_predicate() {
        let f = SearchFilters { language: SetMatch { any_of: vec![], none_of: vec!["typescript".into()] }, ..Default::default() };
        let sql = built_sql(&f);
        assert!(sql.contains("d.language") && sql.contains("<> ALL("), "got: {sql}");
    }

    #[test]
    fn token_count_min_emits_range() {
        let f = SearchFilters { token_count: Some(NumericRange { min: Some(50), max: None }), ..Default::default() };
        let sql = built_sql(&f);
        assert!(sql.contains("chunk.token_count >="), "got: {sql}");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mn-server search::tests::kind_emits_document_join_and_any_predicate`
Expected: FAIL (old builder doesn't emit `d.kind`).

- [ ] **Step 3: Rewrite the three functions**

Replace `needs_document_join`, `push_filter_joins`, and `push_filter_predicates`. Map each facet to its column:

| Facet | Join | Predicate column |
|---|---|---|
| attribution, content_type, deprecated, verified, kind, language, language_target*, sdk_dependency* | `document d` | `d.provenance->>...` / `d.kind` / `d.language` |
| source_slug, source_kind | `source s` | `s.slug` / `s.kind` |
| package | `document d` + `package p` | `p.kind`, `p.name` |
| tags | `document d` | `d.provenance->'tags'` (JSONB `?\|`) |
| heading_path | — | `chunk.heading_path` (text[] `&&`) |
| symbol | — | `chunk.symbol_path` (JSONB `@>`) |
| ingested_at | — | `sv.ingested_at` |
| source_modified_at | `document d` | `d.source_modified_at` |
| token_count | — | `chunk.token_count` |

\* applied in SQL by name membership; version refined by `semver_post_match` post-fetch.

```rust
const fn needs_document_join(f: &SearchFilters) -> bool {
    !f.attribution.is_empty() || f.verified.is_some() || f.deprecated.is_some()
        || !f.content_type.is_empty() || !f.kind.is_empty() || !f.language.is_empty()
        || !f.tags.is_empty() || !f.package.is_empty() || !f.language_target.is_empty()
        || !f.sdk_dependency.is_empty() || f.source_modified_at.is_some()
}

fn push_filter_joins(qb: &mut QueryBuilder<'_, Postgres>, f: &SearchFilters) {
    if needs_document_join(f) {
        qb.push(" JOIN document d ON d.id = chunk.document_id");
    }
    if !f.source_slug.is_empty() || !f.source_kind.is_empty() {
        qb.push(" JOIN source s ON s.id = sv.source_id");
    }
    if !f.package.is_empty() {
        qb.push(" LEFT JOIN package p ON p.id = d.package_id");
    }
}

fn push_filter_predicates(qb: &mut QueryBuilder<'_, Postgres>, f: &SearchFilters) {
    // -- enum / open-set string facets backed by a column --
    push_text_set(qb, "d.kind", &f.kind);
    push_text_set(qb, "d.language", &f.language);
    push_text_set(qb, "s.slug", &f.source_slug);
    push_text_set(qb, "s.kind", &f.source_kind);
    // provenance-backed enums (default-coalesced to match the old behaviour)
    push_prov_set(qb, "attribution", "unknown", &f.attribution);
    push_prov_set(qb, "content_type", "other", &f.content_type);

    // -- bools --
    if let Some(v) = f.verified {
        qb.push(" AND COALESCE((d.provenance->>'verified')::boolean, false) = ");
        qb.push_bind(v);
    }
    if let Some(v) = f.deprecated {
        qb.push(" AND COALESCE((d.provenance->'deprecation'->>'is_deprecated')::boolean, false) = ");
        qb.push_bind(v);
    }

    // -- tags: JSONB array overlap (any_of) / NOT overlap (none_of) --
    if !f.tags.any_of.is_empty() {
        qb.push(" AND d.provenance->'tags' ?| ");
        qb.push_bind(f.tags.any_of.clone());
    }
    if !f.tags.none_of.is_empty() {
        qb.push(" AND NOT (d.provenance->'tags' ?| ");
        qb.push_bind(f.tags.none_of.clone());
        qb.push(")");
    }

    // -- heading_path: text[] overlap --
    if !f.heading_path.any_of.is_empty() {
        qb.push(" AND chunk.heading_path && ");
        qb.push_bind(f.heading_path.any_of.clone());
    }
    if !f.heading_path.none_of.is_empty() {
        qb.push(" AND NOT (chunk.heading_path && ");
        qb.push_bind(f.heading_path.none_of.clone());
        qb.push(")");
    }

    // -- symbol: JSONB containment per element (OR within any_of) --
    push_symbol(qb, &f.symbol);

    // -- package: (kind,name) tuples (OR within any_of) --
    push_package(qb, &f.package);

    // -- language_target / sdk_dependency: name membership in SQL --
    push_language_target_names(qb, &f.language_target);
    push_sdk_dependency_names(qb, &f.sdk_dependency);

    // -- ranges --
    if let Some(r) = &f.ingested_at {
        if let Some(a) = r.after  { qb.push(" AND sv.ingested_at >= "); qb.push_bind(date_to_dt(a)); }
        if let Some(b) = r.before { qb.push(" AND sv.ingested_at <= "); qb.push_bind(date_to_dt(b)); }
    }
    if let Some(r) = &f.source_modified_at {
        if let Some(a) = r.after  { qb.push(" AND d.source_modified_at >= "); qb.push_bind(date_to_dt(a)); }
        if let Some(b) = r.before { qb.push(" AND d.source_modified_at <= "); qb.push_bind(date_to_dt(b)); }
    }
    if let Some(r) = &f.token_count {
        if let Some(min) = r.min { qb.push(" AND chunk.token_count >= "); qb.push_bind(min as i32); }
        if let Some(max) = r.max { qb.push(" AND chunk.token_count <= "); qb.push_bind(max as i32); }
    }
}
```

Add the small helpers in the same module:

```rust
fn push_text_set(qb: &mut QueryBuilder<'_, Postgres>, col: &str, set: &mn_retrieval::filters::SetMatch<String>) {
    if !set.any_of.is_empty() {
        qb.push(format!(" AND {col} = ANY("));
        qb.push_bind(set.any_of.clone());
        qb.push(")");
    }
    if !set.none_of.is_empty() {
        qb.push(format!(" AND {col} <> ALL("));
        qb.push_bind(set.none_of.clone());
        qb.push(")");
    }
}

fn push_prov_set(qb: &mut QueryBuilder<'_, Postgres>, key: &str, default: &str, set: &mn_retrieval::filters::SetMatch<String>) {
    if !set.any_of.is_empty() {
        qb.push(format!(" AND COALESCE(d.provenance->>'{key}', '{default}') = ANY("));
        qb.push_bind(set.any_of.clone());
        qb.push(")");
    }
    if !set.none_of.is_empty() {
        qb.push(format!(" AND COALESCE(d.provenance->>'{key}', '{default}') <> ALL("));
        qb.push_bind(set.none_of.clone());
        qb.push(")");
    }
}

fn push_symbol(qb: &mut QueryBuilder<'_, Postgres>, set: &mn_retrieval::filters::SetMatch<mn_retrieval::filters::SymbolMatch>) {
    if !set.any_of.is_empty() {
        qb.push(" AND (");
        for (i, s) in set.any_of.iter().enumerate() {
            if i > 0 { qb.push(" OR "); }
            qb.push("chunk.symbol_path @> ");
            qb.push_bind(symbol_json(s));
        }
        qb.push(")");
    }
    for s in &set.none_of {
        qb.push(" AND NOT (chunk.symbol_path @> ");
        qb.push_bind(symbol_json(s));
        qb.push(")");
    }
}

/// Build a one-element JSONB containment doc, e.g. `[{"kind":"circuit"}]`.
fn symbol_json(s: &mn_retrieval::filters::SymbolMatch) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    if let Some(k) = &s.kind { obj.insert("kind".into(), serde_json::Value::String(k.clone())); }
    if let Some(n) = &s.name { obj.insert("name".into(), serde_json::Value::String(n.clone())); }
    serde_json::Value::Array(vec![serde_json::Value::Object(obj)])
}

fn push_package(qb: &mut QueryBuilder<'_, Postgres>, set: &mn_retrieval::filters::SetMatch<mn_retrieval::filters::PackageMatch>) {
    if !set.any_of.is_empty() {
        qb.push(" AND (");
        for (i, p) in set.any_of.iter().enumerate() {
            if i > 0 { qb.push(" OR "); }
            qb.push("(p.kind = "); qb.push_bind(p.kind.clone());
            qb.push(" AND p.name = "); qb.push_bind(p.name.clone()); qb.push(")");
        }
        qb.push(")");
    }
    // none_of for package omitted: registry marks package negatable but the
    // common agent case is positive; if none_of is set, emit NOT(...) per element.
    for p in &set.none_of {
        qb.push(" AND NOT (p.kind = "); qb.push_bind(p.kind.clone());
        qb.push(" AND p.name = "); qb.push_bind(p.name.clone()); qb.push(")");
    }
}

fn push_language_target_names(qb: &mut QueryBuilder<'_, Postgres>, set: &mn_retrieval::filters::SetMatch<mn_retrieval::filters::LanguageTargetMatch>) {
    if set.any_of.is_empty() { return; }
    let names: Vec<String> = set.any_of.iter().map(|t| t.name.clone()).collect();
    // EXISTS over the provenance.language_targets JSONB array by name.
    qb.push(" AND EXISTS (SELECT 1 FROM jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt WHERE lt->>'name' = ANY(");
    qb.push_bind(names);
    qb.push("))");
}

fn push_sdk_dependency_names(qb: &mut QueryBuilder<'_, Postgres>, set: &mn_retrieval::filters::SetMatch<mn_retrieval::filters::SdkDependencyMatch>) {
    if set.any_of.is_empty() { return; }
    let names: Vec<String> = set.any_of.iter().map(|d| d.name.clone()).collect();
    qb.push(" AND EXISTS (SELECT 1 FROM jsonb_array_elements(COALESCE(d.provenance->'sdk_dependencies','[]'::jsonb)) dep WHERE dep->>'name' = ANY(");
    qb.push_bind(names);
    qb.push("))");
}

fn date_to_dt(d: time::Date) -> time::OffsetDateTime {
    d.with_hms(0, 0, 0).unwrap().assume_utc()
}
```

> The `semver_post_match` step already runs after candidate fetch (`search.rs` post-RRF scoring loop). Confirm it is still called with `&req.filters` for each candidate's provenance; it now reads `language_target`/`sdk_dependency` as object-sets (Task A4).

- [ ] **Step 4: Run unit tests**

Run: `cargo test -p mn-server search::tests`
Expected: PASS (SQL-shape tests + existing).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/routes/search.rs
git commit -m "feat(server): registry-aligned SQL predicates for all v1 facets"
```

---

### Task B4: `/v1/facets` endpoint (closed values)

**Files:**
- Create: `crates/mn-server/src/routes/facets.rs`
- Modify: `crates/mn-server/src/routes/mod.rs`, `crates/mn-server/src/app.rs`

- [ ] **Step 1: Register the module + router**

In `routes/mod.rs` add `pub mod facets;`. In `app.rs` after the search merge (`app.rs:247`) add:

```rust
        .merge(crate::routes::facets::router())
```

- [ ] **Step 2: Write the handler (closed values first; open values in B5)**

Create `crates/mn-server/src/routes/facets.rs`:

```rust
//! `GET /v1/facets` — advertise the filterable facets, their types, and (for
//! closed enums) their allowed values, so clients can construct valid filters.
//! Open-set values are filled from the active corpus in `corpus_values`.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mn_retrieval::facets::{self, FacetType};
use serde_json::json;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;
use axum::Extension;

#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/facets", get(get_facets))
}

async fn get_facets(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let open = match corpus_values(&state.pool).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "facet value query failed");
            return error::service_unavailable("facet enumeration failed", rid);
        }
    };

    let filters: Vec<_> = facets::facets().iter().map(|d| {
        let type_str = match d.facet_type {
            FacetType::Enum => "enum",
            FacetType::OpenSet => "open_set",
            FacetType::ObjectSet => "object_set",
            FacetType::Bool => "bool",
            FacetType::RangeTemporal => "range_temporal",
            FacetType::RangeNumeric => "range_numeric",
        };
        let mut entry = json!({ "key": d.key, "type": type_str, "negatable": d.negatable });
        if let Some(vals) = d.closed_values {
            entry["values"] = json!(vals);
        } else if let Some(v) = open.get(d.key) {
            entry["values"] = v.values.clone();
            entry["truncated"] = json!(v.truncated);
            entry["total"] = json!(v.total);
        }
        entry
    }).collect();

    Json(json!({
        "modes": ["hybrid", "vector", "fts"],
        "filters": filters,
    }))
    .into_response()
}

/// Bounded distinct values for open-set facets, keyed by facet name.
struct OpenValues { values: serde_json::Value, truncated: bool, total: i64 }

async fn corpus_values(
    _pool: &sqlx::PgPool,
) -> Result<std::collections::HashMap<String, OpenValues>, sqlx::Error> {
    // Filled in Task B5. Empty map → open-set facets advertise type only.
    Ok(std::collections::HashMap::new())
}
```

- [ ] **Step 3: Write the failing integration test**

Create `crates/mn-server/tests/facets_route.rs` (follow the existing `search_route.rs` test harness — reuse its app/builder helper; the snippet below assumes a `spawn_app()` style helper exists in the test support module, matching the other `mn-server/tests/*.rs`):

```rust
#![cfg(feature = "integration")]
//! Integration test for GET /v1/facets.
mod support; // reuse the existing test support module pattern

#[tokio::test]
async fn facets_lists_modes_and_closed_enums() {
    let app = support::spawn_app().await;
    let resp = app.get("/v1/facets").await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await;
    assert_eq!(body["modes"], serde_json::json!(["hybrid", "vector", "fts"]));
    let kind = body["filters"].as_array().unwrap().iter()
        .find(|f| f["key"] == "kind").expect("kind facet");
    assert_eq!(kind["type"], "enum");
    assert_eq!(kind["values"], serde_json::json!(["markdown", "code", "plaintext"]));
}
```

> If `mn-server/tests` has no shared `support` module, inline the app bootstrap exactly as `search_route.rs` does (read it for the exact `testcontainers` + `AppState` setup, then copy that prologue).

- [ ] **Step 4: Run (unit build + integration in CI)**

Run locally (compile + unit): `cargo build -p mn-server && cargo test -p mn-server`
Run in CI (integration): `cargo test -p mn-server --features integration --test facets_route`
Expected: build PASS locally; integration PASS in CI (sandbox has no Docker — see project norms).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/routes/facets.rs crates/mn-server/src/routes/mod.rs crates/mn-server/src/app.rs crates/mn-server/tests/facets_route.rs
git commit -m "feat(server): GET /v1/facets endpoint (modes + closed-enum values)"
```

---

### Task B5: `/v1/facets` open-set values + TTL cache

**Files:**
- Modify: `crates/mn-server/src/routes/facets.rs`

- [ ] **Step 1: Implement `corpus_values` with DISTINCT queries**

Replace the stub `corpus_values` with real, bounded queries against the active corpus. Cap high-cardinality sets at 200 by document frequency:

```rust
const VALUE_CAP: i64 = 200;

async fn corpus_values(
    pool: &sqlx::PgPool,
) -> Result<std::collections::HashMap<String, OpenValues>, sqlx::Error> {
    use sqlx::Row as _;
    let mut out = std::collections::HashMap::new();

    // language (low cardinality, no cap needed)
    let rows = sqlx::query(
        "SELECT DISTINCT d.language FROM document d \
         JOIN source_version sv ON sv.id = d.source_version_id \
         WHERE sv.is_active = true AND d.language IS NOT NULL ORDER BY d.language",
    ).fetch_all(pool).await?;
    let langs: Vec<String> = rows.iter().filter_map(|r| r.try_get::<String, _>("language").ok()).collect();
    out.insert("language".into(), OpenValues { total: langs.len() as i64, truncated: false, values: serde_json::json!(langs) });

    // source_slug
    let rows = sqlx::query(
        "SELECT s.slug FROM source s WHERE s.retired_at IS NULL ORDER BY s.slug",
    ).fetch_all(pool).await?;
    let slugs: Vec<String> = rows.iter().filter_map(|r| r.try_get::<String, _>("slug").ok()).collect();
    out.insert("source_slug".into(), OpenValues { total: slugs.len() as i64, truncated: false, values: serde_json::json!(slugs) });

    // tags (high cardinality → top-N by frequency)
    let rows = sqlx::query(
        "SELECT tag, count(*) AS n FROM document d \
         JOIN source_version sv ON sv.id = d.source_version_id \
         CROSS JOIN LATERAL jsonb_array_elements_text(COALESCE(d.provenance->'tags','[]'::jsonb)) AS tag \
         WHERE sv.is_active = true GROUP BY tag ORDER BY n DESC, tag LIMIT $1",
    ).bind(VALUE_CAP + 1).fetch_all(pool).await?;
    let mut tags: Vec<String> = rows.iter().filter_map(|r| r.try_get::<String, _>("tag").ok()).collect();
    let truncated = tags.len() as i64 > VALUE_CAP;
    tags.truncate(VALUE_CAP as usize);
    out.insert("tags".into(), OpenValues { total: tags.len() as i64, truncated, values: serde_json::json!(tags) });

    // package names (top-N)
    let rows = sqlx::query(
        "SELECT p.name, count(*) AS n FROM package p \
         JOIN source_version sv ON sv.id = p.source_version_id \
         WHERE sv.is_active = true GROUP BY p.name ORDER BY n DESC, p.name LIMIT $1",
    ).bind(VALUE_CAP + 1).fetch_all(pool).await?;
    let mut pkgs: Vec<String> = rows.iter().filter_map(|r| r.try_get::<String, _>("name").ok()).collect();
    let pkg_trunc = pkgs.len() as i64 > VALUE_CAP;
    pkgs.truncate(VALUE_CAP as usize);
    out.insert("package".into(), OpenValues { total: pkgs.len() as i64, truncated: pkg_trunc, values: serde_json::json!(pkgs) });

    Ok(out)
}
```

> `symbol` and `heading_path` open values are omitted from v1 enumeration (very high cardinality; the type + `symbol.kind` closed enum are enough for agents). Note this in the response by leaving them without `values` — and log nothing (it is intentional, not a cap).

- [ ] **Step 2: Add a 60s TTL cache**

Add a module-level cache so repeated calls don't re-run the DISTINCT queries:

```rust
use std::sync::RwLock;
use std::time::{Duration, Instant};

static CACHE: RwLock<Option<(Instant, serde_json::Value)>> = RwLock::new(None);
const TTL: Duration = Duration::from_secs(60);
```

In `get_facets`, check the cache before querying and store the assembled body after. (Keyed only on time — corpus changes are admin-driven and infrequent; the design accepts a ≤60s staleness window.)

```rust
    if let Some((at, body)) = &*CACHE.read().expect("facets cache poisoned") {
        if at.elapsed() < TTL {
            return Json(body.clone()).into_response();
        }
    }
    // ... build `body` (the json!({modes, filters}) value) ...
    *CACHE.write().expect("facets cache poisoned") = Some((Instant::now(), body.clone()));
    Json(body).into_response()
```

- [ ] **Step 3: Extend the integration test**

Add to `facets_route.rs` a test that seeds two documents with languages `compact` and `rust` and asserts `language` values contain both, and a `tags` document asserting `truncated` is `false` for a small fixture.

- [ ] **Step 4: Run**

Run in CI: `cargo test -p mn-server --features integration --test facets_route`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/routes/facets.rs crates/mn-server/tests/facets_route.rs
git commit -m "feat(server): /v1/facets open-set values from active corpus + 60s TTL cache"
```

---

### Task B6: End-to-end filter + mode integration tests

**Files:**
- Create: `crates/mn-server/tests/search_filters.rs`

- [ ] **Step 1: Write integration tests** (seed a fixture corpus; reuse the `search_route.rs` bootstrap)

```rust
#![cfg(feature = "integration")]
mod support;

// Seed: 1 markdown(compact) doc + 1 code(rust) doc, distinct chunks.
// Assertions:
#[tokio::test]
async fn kind_filter_narrows_to_code() { /* POST /v1/search with filters.kind.any_of=["code"]; assert all results are code chunks */ }

#[tokio::test]
async fn language_none_of_excludes() { /* filters.language.none_of=["typescript"]; assert no ts chunks */ }

#[tokio::test]
async fn fts_mode_runs_without_vector() {
    // POST {query:"...", mode:"fts"} with NO vector / NO client_embedding_model.
    // Assert 200 and lexical hits; proves embedding-skip end-to-end.
}

#[tokio::test]
async fn unknown_filter_key_is_rejected_400() {
    // POST with filters: { "langauge": {...} } (typo) → 400 invalid_request,
    // proving the silent-drop behaviour is gone (deny_unknown_fields).
}

#[tokio::test]
async fn vector_mode_requires_vector_400() {
    // POST { query:"x", mode:"vector" } with no vector → 400.
}
```

Fill each body using the exact request/response helpers from `search_route.rs`.

- [ ] **Step 2: Run in CI**

Run: `cargo test -p mn-server --features integration --test search_filters`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/mn-server/tests/search_filters.rs
git commit -m "test(server): integration coverage for facets, modes, and fail-fast validation"
```

- [ ] **Step 4: Phase gate**

Run: `cargo test -p mn-server && cargo clippy -p mn-server --all-targets -- -D warnings`
Expected: PASS, no warnings. (Integration tests run in CI.)

---

# Phase C — MCP surface

### Task C1: Typed `search` input schema + `mode` passthrough

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs` (`search_input_schema`), `crates/mn-mcp/src/cloud_client.rs`

- [ ] **Step 1: Add `mode` to the cloud client request**

In `cloud_client.rs` `struct SearchRequest`, add:

```rust
    /// Query mode forwarded to the cloud (`hybrid` | `vector` | `fts`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<&'static str>,
```

- [ ] **Step 2: Replace the opaque filters schema**

In `tools.rs`, rewrite `search_input_schema()` so `filters` is typed and `mode` is a top-level enum. Build the closed-enum value arrays from the registry to avoid drift:

```rust
fn search_input_schema() -> serde_json::Value {
    use mn_retrieval::facets;
    let set_of = |values: Option<&[&str]>| {
        let items = match values {
            Some(v) => json!({ "type": "string", "enum": v }),
            None => json!({ "type": "string" }),
        };
        json!({
            "type": "object",
            "properties": { "any_of": { "type": "array", "items": items }, "none_of": { "type": "array", "items": items } },
            "additionalProperties": false,
        })
    };
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "minLength": 1 },
            "queries": { "type": "array", "minItems": 1, "maxItems": 50, "items": { "type": "string", "minLength": 1 } },
            "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
            "rerank": { "type": "boolean", "default": true },
            "mode": { "type": "string", "enum": ["hybrid", "vector", "fts"], "default": "hybrid",
                      "description": "fts skips embedding entirely (lowest latency); vector is semantic-only; hybrid (default) fuses both." },
            "filters": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind":         set_of(Some(facets::KIND_VALUES)),
                    "source_kind":  set_of(Some(facets::SOURCE_KIND_VALUES)),
                    "attribution":  set_of(Some(facets::ATTRIBUTION_VALUES)),
                    "content_type": set_of(Some(facets::CONTENT_TYPE_VALUES)),
                    "language":     set_of(None),
                    "tags":         set_of(None),
                    "source_slug":  set_of(None),
                    "heading_path": set_of(None),
                    "verified":   { "type": "boolean" },
                    "deprecated": { "type": "boolean" },
                    "symbol": { "type": "object", "properties": { "any_of": { "type": "array", "items": {
                        "type": "object", "properties": { "kind": { "type": "string", "enum": facets::SYMBOL_KIND_VALUES }, "name": { "type": "string" } },
                        "additionalProperties": false } } }, "additionalProperties": false },
                    "package": { "type": "object", "properties": { "any_of": { "type": "array", "items": {
                        "type": "object", "required": ["kind","name"], "properties": { "kind": { "type": "string", "enum": facets::PACKAGE_KIND_VALUES }, "name": { "type": "string" } },
                        "additionalProperties": false } } }, "additionalProperties": false },
                    "ingested_at": { "type": "object", "properties": { "after": { "type": "string", "format": "date" }, "before": { "type": "string", "format": "date" } }, "additionalProperties": false },
                    "source_modified_at": { "type": "object", "properties": { "after": { "type": "string", "format": "date" }, "before": { "type": "string", "format": "date" } }, "additionalProperties": false },
                    "token_count": { "type": "object", "properties": { "min": { "type": "integer" }, "max": { "type": "integer" } }, "additionalProperties": false }
                },
                "description": "Per-facet filters. AND across keys, OR within any_of, exclude none_of. See the `facets` tool for corpus-derived values."
            }
        },
        "oneOf": [ { "required": ["query"] }, { "required": ["queries"] } ]
    })
}
```

> `mn-mcp` must depend on `mn-retrieval` (add to `crates/mn-mcp/Cargo.toml` `[dependencies]` if not present: `mn-retrieval = { path = "../mn-retrieval" }`).

- [ ] **Step 3: Parse + forward `mode` and validate filters at the MCP boundary**

In `parse_search_args` (`tools.rs:699`), add `mode` parsing (default hybrid; reject unknown), and validate the `filters` object by deserializing into `mn_retrieval::filters::SearchFilters` and calling `.validate()` — returning the `FilterError` message as the `Err(String)` the dispatcher maps to `InvalidParams`:

```rust
    let mode = match obj.get("mode").and_then(|m| m.as_str()) {
        None => "hybrid",
        Some(m @ ("hybrid" | "vector" | "fts")) => m,
        Some(other) => return Err(format!("unknown mode `{other}` (expected hybrid|vector|fts)")),
    };
    // Validate filters against the registry before forwarding (fail fast).
    if let Some(fv) = obj.get("filters") {
        let parsed: mn_retrieval::filters::SearchFilters = serde_json::from_value(fv.clone())
            .map_err(|e| format!("invalid filters: {e}"))?;
        parsed.validate().map_err(|e| format!("invalid filter `{}`: {}", e.facet, e.message))?;
    }
```

Add `mode` to `ParsedSearchArgs` and thread it into the `SearchRequest` built in `run_search` (`mode: Some(parsed.mode)`).

- [ ] **Step 4: Write a unit test for schema rejection**

Add to `tools.rs` `mod tests`:

```rust
    #[test]
    fn parse_rejects_unknown_mode_and_bad_filter() {
        let bad_mode = serde_json::json!({ "query": "x", "mode": "fuzzy" });
        assert!(parse_search_args(&bad_mode).is_err());
        let bad_filter = serde_json::json!({ "query": "x", "filters": { "kind": { "any_of": ["binary"] } } });
        assert!(parse_search_args(&bad_filter).is_err());
        let ok = serde_json::json!({ "query": "x", "mode": "fts", "filters": { "kind": { "any_of": ["code"] } } });
        assert!(parse_search_args(&ok).is_ok());
    }
```

- [ ] **Step 5: Run + commit**

Run: `cargo test -p mn-mcp tools::tests::parse_rejects_unknown_mode_and_bad_filter` (set `VOYAGE_API_KEY=` empty per project norms).
Expected: PASS.

```bash
git add crates/mn-mcp/src/tools.rs crates/mn-mcp/src/cloud_client.rs crates/mn-mcp/Cargo.toml
git commit -m "feat(mcp): typed search input schema + mode + boundary filter validation"
```

---

### Task C2: `facets` MCP tool

**Files:**
- Modify: `crates/mn-mcp/src/cloud_client.rs`, `crates/mn-mcp/src/tools.rs`, `crates/mn-mcp/src/server.rs`, `crates/mn-telemetry/src/events.rs`

- [ ] **Step 1: Add `get_facets` to the cloud client**

In `cloud_client.rs` impl:

```rust
    /// `GET /v1/facets`.
    pub async fn get_facets(&self) -> Result<serde_json::Value, CloudError> {
        self.get_json("/v1/facets").await
    }
```

- [ ] **Step 2: Register the tool**

In `tools.rs` `list()`, add a `ToolDescription` after `list_sources`:

```rust
            ToolDescription {
                name: "facets",
                description: "List the filterable facets for `search`, their types, whether they support exclusion (none_of), and the values present in the active corpus (languages, tags, sources, packages). Call this before constructing a `filters` object to learn valid values. Closed-enum facets (kind, content_type, attribution, source_kind) carry their full value list; high-cardinality sets (tags, package) are top-N with `truncated`/`total`.",
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            },
```

- [ ] **Step 3: Add the telemetry variant + dispatch**

In `crates/mn-telemetry/src/events.rs` `enum McpToolName`, add `Facets` (matching the snake_case rename convention used by the others).

In `crates/mn-mcp/src/server.rs`:
- In the name→`McpToolName` match (~line 266), add `"facets" => Some(McpToolName::Facets),`.
- In `dispatch_tool_inner` (~line 297), route `"facets"` to a passthrough call. Add `"facets"` to the `run_passthrough_tool` arm if that helper calls a fixed endpoint, OR add a dedicated arm:

```rust
        "facets" => match state.cloud.get_facets().await {
            Ok(v) => crate::server::ok_json(&id, v),
            Err(e) => crate::server::cloud_error(&id, e),
        },
```

> Match the exact success/error helpers `run_passthrough_tool` uses (read `server.rs:283-330` for the precise `ok`/`error` constructors and copy that shape).

- [ ] **Step 4: Run + commit**

Run: `cargo build -p mn-mcp -p mn-telemetry && cargo test -p mn-mcp` (with `VOYAGE_API_KEY=`).
Expected: PASS.

```bash
git add crates/mn-mcp/src/cloud_client.rs crates/mn-mcp/src/tools.rs crates/mn-mcp/src/server.rs crates/mn-telemetry/src/events.rs
git commit -m "feat(mcp): facets discovery tool (GET /v1/facets passthrough)"
```

---

### Task C3: MCP phase gate

- [ ] **Step 1: Run the MCP suite + clippy**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp && cargo clippy -p mn-mcp --all-targets -- -D warnings`
Expected: PASS, no warnings.

> Note (project norm): the sandbox sets `VOYAGE_API_KEY`, which breaks BYOK-path mn-mcp tests; run with `VOYAGE_API_KEY=` empty.

---

# Phase D — CLI surface

### Task D1: Filter + mode flags on `mnm search`

**Files:**
- Modify: `crates/mn-cli/src/commands/search.rs`

- [ ] **Step 1: Write the failing flag-mapping test**

Add to `search.rs` `mod tests`:

```rust
    #[test]
    fn flags_map_to_filters_and_mode() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Probe { #[command(flatten)] inner: Args }
        let p = Probe::parse_from([
            "search", "q", "--mode", "fts",
            "--kind", "code", "--language", "compact", "--exclude-language", "typescript",
            "--tag", "quickstart", "--symbol", "circuit:deployContract", "--no-deprecated",
            "--min-tokens", "50",
        ]);
        let (mode, filters) = build_filters(&p.inner);
        assert_eq!(mode, "fts");
        assert_eq!(filters.kind.any_of, vec!["code".to_owned()]);
        assert_eq!(filters.language.none_of, vec!["typescript".to_owned()]);
        assert_eq!(filters.symbol.any_of[0].kind.as_deref(), Some("circuit"));
        assert_eq!(filters.symbol.any_of[0].name.as_deref(), Some("deployContract"));
        assert_eq!(filters.deprecated, Some(false));
        assert_eq!(filters.token_count.unwrap().min, Some(50));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mn-cli search::tests::flags_map_to_filters_and_mode`
Expected: FAIL to compile (`build_filters`, new flags not defined).

- [ ] **Step 3: Add flags to `Args` + a `build_filters` mapper**

Add fields to `struct Args` (after `reranker`):

```rust
    /// Query mode: hybrid (default), vector, or fts.
    #[arg(long, default_value = "hybrid", value_parser = ["hybrid", "vector", "fts"])]
    pub mode: String,
    #[arg(long = "kind")] pub kind: Vec<String>,
    #[arg(long = "language")] pub language: Vec<String>,
    #[arg(long = "exclude-language")] pub exclude_language: Vec<String>,
    #[arg(long = "tag")] pub tag: Vec<String>,
    #[arg(long = "exclude-tag")] pub exclude_tag: Vec<String>,
    #[arg(long = "symbol")] pub symbol: Vec<String>,
    #[arg(long = "source")] pub source: Vec<String>,
    #[arg(long = "content-type")] pub content_type: Vec<String>,
    #[arg(long = "attribution")] pub attribution: Vec<String>,
    #[arg(long = "no-deprecated")] pub no_deprecated: bool,
    #[arg(long = "verified")] pub verified: bool,
    #[arg(long = "ingested-after")] pub ingested_after: Option<String>,
    #[arg(long = "ingested-before")] pub ingested_before: Option<String>,
    #[arg(long = "min-tokens")] pub min_tokens: Option<i64>,
    #[arg(long = "max-tokens")] pub max_tokens: Option<i64>,
    /// Full filter object as JSON (mutually exclusive with the granular flags).
    #[arg(long = "filter-json", conflicts_with_all = ["kind","language","exclude_language","tag","exclude_tag","symbol","source","content_type","attribution","no_deprecated","verified","ingested_after","ingested_before","min_tokens","max_tokens"])]
    pub filter_json: Option<String>,
```

Add the mapper (returns the mode string + the assembled `SearchFilters`):

```rust
fn build_filters(args: &Args) -> (String, mn_retrieval::filters::SearchFilters) {
    use mn_retrieval::filters::*;
    if let Some(js) = &args.filter_json {
        let f: SearchFilters = serde_json::from_str(js).unwrap_or_default();
        return (args.mode.clone(), f);
    }
    let set = |any_of: &[String], none_of: &[String]| SetMatch {
        any_of: any_of.to_vec(), none_of: none_of.to_vec(),
    };
    let symbols = args.symbol.iter().map(|s| {
        let (k, n) = s.split_once(':').map_or((s.as_str(), ""), |(k, n)| (k, n));
        SymbolMatch {
            kind: if k.is_empty() { None } else { Some(k.to_owned()) },
            name: if n.is_empty() { None } else { Some(n.to_owned()) },
        }
    }).collect();
    let parse_date = |s: &Option<String>| s.as_deref().and_then(|d| time::Date::parse(d, &time::format_description::well_known::Iso8601::DATE).ok());
    let ingested = (args.ingested_after.is_some() || args.ingested_before.is_some()).then(|| TemporalRange {
        after: parse_date(&args.ingested_after), before: parse_date(&args.ingested_before),
    });
    let token_count = (args.min_tokens.is_some() || args.max_tokens.is_some()).then(|| NumericRange { min: args.min_tokens, max: args.max_tokens });
    let f = SearchFilters {
        kind: set(&args.kind, &[]),
        language: set(&args.language, &args.exclude_language),
        tags: set(&args.tag, &args.exclude_tag),
        source_slug: set(&args.source, &[]),
        content_type: set(&args.content_type, &[]),
        attribution: set(&args.attribution, &[]),
        symbol: SetMatch { any_of: symbols, none_of: vec![] },
        deprecated: args.no_deprecated.then_some(false),
        verified: args.verified.then_some(true),
        ingested_at: ingested,
        token_count,
        ..Default::default()
    };
    (args.mode.clone(), f)
}
```

- [ ] **Step 4: Wire into the request build**

In `build_search_request` (`search.rs:297`), stop hardcoding `SearchFilters::default()`. Thread `(mode, filters)` from `build_filters(&args)` into the `SearchRequest` (add a `mode: String` and real `filters` to the CLI-side `SearchRequest` struct — `search.rs:651` — and include them in the body). Validate filters client-side too and bail early with a friendly error on `validate()` failure.

- [ ] **Step 5: Run + commit**

Run: `cargo test -p mn-cli search::tests`
Expected: PASS.

```bash
git add crates/mn-cli/src/commands/search.rs
git commit -m "feat(cli): facet + mode flags for mnm search"
```

---

### Task D2: `mnm facets` subcommand

**Files:**
- Create: `crates/mn-cli/src/commands/facets.rs`
- Modify: `crates/mn-cli/src/commands/mod.rs`, `crates/mn-cli/src/cli.rs`

- [ ] **Step 1: Implement the command** (mirror `commands/sources.rs` — read it for the exact server-URL resolution + render conventions)

```rust
//! `mnm facets` — print the corpus's filterable facets (GET /v1/facets).
use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Emit raw JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: Args, server_flag: Option<&str>) -> Result<()> {
    let server = crate::shared::resolve_server_url(server_flag);
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let body: serde_json::Value = client.get(format!("{server}/v1/facets")).send().await
        .context("GET /v1/facets")?.json().await.context("parse /v1/facets")?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("modes: {}", body["modes"]);
    for f in body["filters"].as_array().cloned().unwrap_or_default() {
        let neg = if f["negatable"].as_bool().unwrap_or(false) { " (negatable)" } else { "" };
        let vals = f.get("values").map(|v| format!(" — {v}")).unwrap_or_default();
        println!("  {} [{}]{neg}{vals}", f["key"].as_str().unwrap_or("?"), f["type"].as_str().unwrap_or("?"));
    }
    Ok(())
}
```

- [ ] **Step 2: Register the subcommand**

In `commands/mod.rs` add `pub mod facets;`. In `cli.rs`:
- Add `Facets(commands::facets::Args)` to the `Command` enum (after `Search`).
- Add the dispatch arm in the match (~`cli.rs:165`): `Command::Facets(args) => commands::facets::run(args, server_flag).await,` (match the exact `run` signature/args other read commands use).
- Add `Command::Facets(_) => CliCommandName::...` to the telemetry mapping (~`cli.rs:268`) — add a `Facets` variant to `CliCommandName` in `mn-telemetry` if the mapping is exhaustive, or reuse the closest existing read variant if adding an enum is out of scope (prefer adding `Facets`).

- [ ] **Step 3: Run + commit**

Run: `cargo build -p mn-cli && cargo test -p mn-cli`
Expected: PASS.

```bash
git add crates/mn-cli/src/commands/facets.rs crates/mn-cli/src/commands/mod.rs crates/mn-cli/src/cli.rs crates/mn-telemetry/src/events.rs
git commit -m "feat(cli): mnm facets subcommand"
```

---

### Task D3: CLI phase gate

- [ ] **Step 1: Run CLI suite + clippy**

Run: `cargo test -p mn-cli && cargo clippy -p mn-cli --all-targets -- -D warnings`
Expected: PASS, no warnings. (Two `auth_integration` loopback tests fail in the sandbox per project norms — not a regression.)

---

# Phase E — Contracts & docs

### Task E1: Update OpenAPI + MCP contract docs

**Files:**
- Modify: `specs/001-rag-platform/contracts/openapi.yaml`, `specs/001-rag-platform/contracts/mcp-tools.json`

- [ ] **Step 1: Update OpenAPI**

In `openapi.yaml`: replace the old `SearchFilters` schema with the per-facet match model (mirror `crates/mn-retrieval/src/filters.rs`); add the `mode` enum to the `/v1/search` request body; add a `GET /v1/facets` path with the `{modes, filters}` response schema. Set `additionalProperties: false` on `SearchFilters`.

- [ ] **Step 2: Update mcp-tools.json**

Replace the `search` tool `inputSchema` with the typed schema from Task C1; add the `facets` tool entry from Task C2.

- [ ] **Step 3: Run contract tests** (if `mn-mcp` has a contract test asserting `list()` matches `mcp-tools.json`)

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp contract`
Expected: PASS (the schema in code now matches the document).

- [ ] **Step 4: Commit**

```bash
git add specs/001-rag-platform/contracts/openapi.yaml specs/001-rag-platform/contracts/mcp-tools.json
git commit -m "docs(contracts): per-facet filters, mode, and /v1/facets in OpenAPI + mcp-tools"
```

---

# Final Phase Gate

- [ ] **Step 1: Full workspace check (matches CI)**

Run: `just check` (or `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`)
Expected: PASS. Integration tests (`--features integration`) run in CI per project norms (no Docker in the sandbox).

- [ ] **Step 2: Confirm `symbol_path` population (risk from the spec)**

Verify `mn-content`'s code chunker populates `chunk.symbol_path` for code chunks (migration 0007 noted "no code chunks exist yet"). If it does not, the `symbol` facet matches nothing — open a follow-up to populate it; the facet machinery is still correct and ships.

---

## Self-Review

**Spec coverage** — every design section maps to a task:
- §4.1 filter model → A2; registry → A1; validation → A3; semver → A4.
- §4.2 query modes + embedding-skip → B1, B2.
- §4.3 discoverability (static enums) → C1; `/v1/facets` + open values + cache → B4, B5; `facets` tool → C2.
- §4.4 surfaces → server B2/B3, MCP C1/C2, CLI D1/D2.
- §4.5 error handling → A3 + B2 (server boundary) + C1 (MCP boundary).
- §4.6 testing → A5 (property), B6/B4 (integration), C1/D1 (unit).
- §6 risks → Final Phase Gate Step 2 (`symbol_path` population).
- Out-of-scope (net-new facets, recall harness) → intentionally absent.

**Placeholder scan:** No "TODO/TBD". The two integration-test bodies in B6 are described with exact assertions and reference the concrete `search_route.rs` harness to copy; acceptable as they depend on a pre-existing test prologue not reproduced here.

**Type consistency:** `SearchFilters`/`SetMatch`/`SymbolMatch`/`PackageMatch`/`LanguageTargetMatch`/`SdkDependencyMatch`/`TemporalRange`/`NumericRange` are defined once in A2 and used identically in A3/A4 (retrieval), B3 (server SQL), C1 (MCP parse), D1 (CLI). `FacetType`/`FacetDescriptor`/`facets()`/`lookup()` defined in A1, consumed in A3, B4, C1. `SearchMode` defined in B1, consumed in B2; forwarded as `Option<&'static str>` in C1. `McpToolName::Facets` added in C2.

**Known adaptation points flagged inline:** `parse_version` return type (A3), `QueryBuilder::sql()` availability for SQL-shape assertions (B3), the `mn-server/tests` support-module shape (B4/B6), and `server.rs` ok/error helper names (C2) — each notes "read the neighbouring code and match."

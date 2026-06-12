# Version Provenance & Matching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Populate version provenance from code at ingest (pragma → `language_targets`, allowlisted deps → `sdk_dependencies`, `package.version`), accept concrete-or-range `version_satisfies`, add strict/permissive version matching with distance-scaled penalties (permissive default), enumerate version facets, and rewrite the search skill.

**Architecture:** Spec at `docs/superpowers/specs/2026-06-12-version-provenance-design.md`. New `mn_core::version_match` module owns interval parsing + mismatch classification (incl. the 0.x role shift). `mn-retrieval` classifies per-candidate outcomes; the server route drops/penalizes per mode and feeds a precomputed version input to scoring. Extraction lives in `mn-content`, wired caller-side in `mn-cli` like package detection.

**Tech Stack:** Rust workspace (see CLAUDE.md). `semver = "1"` (already a workspace dep), `proptest` (dev-dep in mn-core/mn-retrieval), `compactp_ast` 0.1.0-beta.1 (`Pragma::name()`, `AstNode::syntax()`). Verify with `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`.

**Conventions for every task:**
- TDD: write the failing test, run it, implement, run again, commit.
- Unit tests: `cargo test -p <crate> <filter>`. mn-server integration tests (`crates/mn-server/tests/`) need Docker/`DATABASE_URL` — get them compiling (`cargo test -p mn-server --no-run --features integration`) and rely on CI.
- Sandbox quirk: run mn-cli/mn-mcp BYOK-path tests with `VOYAGE_API_KEY=` cleared.
- Doc comments on all new public items (several crates deny missing docs — mirror sibling files).
- Pre-1.0: hard cutovers are fine; do not add compatibility shims.

**Spec clarification (recorded here, applied in Task 5):** spec §2.2 says "the winning element also drives the derived rerank instruction", but the instruction is derived once per request, before any candidate exists — per-candidate "winning" cannot apply. Implemented behavior: derive from the **first `language_target.any_of` element that carries `version_satisfies`** (today: blindly the first element, even when version-less).

---

### Task 1: `mn_core::version_match` — intervals, parsing, classification

**Files:**
- Create: `crates/mn-core/src/version_match.rs`
- Modify: `crates/mn-core/src/lib.rs` (add `pub mod version_match;` next to the existing `pub mod scoring;`)

- [x] **Step 1: Write the failing tests** (inside the new file; module skeleton + tests first)

Create `crates/mn-core/src/version_match.rs` with this skeleton and tests (implementation bodies `todo!()` for now):

```rust
//! Version-interval parsing and match classification (version-provenance spec §2).
//!
//! The request side of `version_satisfies` is a concrete version OR a semver
//! range; the declared side (`provenance.*.version_constraint`) is a
//! `semver::VersionReq`. The `semver` crate's grammar has no OR operator, so
//! every requirement is a conjunction of comparators — one contiguous interval.

use semver::{Comparator, Op, Version, VersionReq};

use crate::scoring::parse_version;

/// One interval bound: a version plus whether the bound itself is included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    /// The bounding version.
    pub version: Version,
    /// `true` for `>=`/`<=`-style bounds, `false` for `>`/`<`.
    pub inclusive: bool,
}

/// A contiguous version interval. `None` bounds are unbounded sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInterval {
    /// Lower bound (`None` = unbounded below).
    pub lo: Option<Bound>,
    /// Upper bound (`None` = unbounded above).
    pub hi: Option<Bound>,
}

/// A parsed request-side `version_satisfies` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedVersion {
    /// A concrete version (bare `"0.31"`, `"v1.4.2"` — via [`parse_version`]).
    Concrete(Version),
    /// An explicit range (`">=0.23"`, `"^1.2"`, `"~1.4.2"`, ...).
    Range(VersionInterval),
}

/// Outcome of classifying a request against one declared constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchClass {
    /// The request satisfies / intersects the declared constraint.
    Satisfies,
    /// Disjoint at patch level (after the 0.x role shift); distance in patches.
    NearMissPatch(u32),
    /// Disjoint at minor level (after the 0.x role shift); distance in minors.
    NearMissMinor(u32),
    /// Disjoint at major level (or 0.x minor / 0.0.x patch): incompatible.
    Breaking,
    /// The declared constraint failed to parse — unknowable.
    Unknown,
}

/// Parse a request-side `version_satisfies` string: concrete first (bare
/// versions stay concrete — spec decision 5), else a semver range desugared to
/// an interval. `None` = neither form parses, or the range is empty.
#[must_use]
pub fn parse_request(raw: &str) -> Option<RequestedVersion> {
    todo!()
}

/// Classify a parsed request against a declared constraint string.
/// `declared = None` (a matching-name target with no constraint) ⇒ Satisfies.
#[must_use]
pub fn classify(requested: &RequestedVersion, declared: Option<&str>) -> MatchClass {
    todo!()
}

/// Rank for "best class wins" across elements: lower is better.
#[must_use]
pub fn class_rank(c: &MatchClass) -> (u8, u32) {
    todo!()
}

impl VersionInterval {
    /// The interval containing every version.
    #[must_use]
    pub const fn unbounded() -> Self {
        Self { lo: None, hi: None }
    }

    /// Degenerate single-point interval `[v, v]`.
    #[must_use]
    pub fn point(v: Version) -> Self {
        todo!()
    }

    /// Desugar a `VersionReq` (conjunction of comparators) to an interval.
    /// `None` when a comparator op is unsupported (future semver ops).
    #[must_use]
    pub fn from_req(req: &VersionReq) -> Option<Self> {
        todo!()
    }

    /// Whether `v` lies inside the interval.
    #[must_use]
    pub fn contains(&self, v: &Version) -> bool {
        todo!()
    }

    /// Whether two intervals share at least one version.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        todo!()
    }

    /// Whether the interval contains no versions (contradictory comparators).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn v(s: &str) -> Version {
        parse_version(s).unwrap()
    }
    fn req(s: &str) -> RequestedVersion {
        parse_request(s).unwrap()
    }

    #[test]
    fn parse_request_bare_is_concrete() {
        assert_eq!(req("0.31"), RequestedVersion::Concrete(v("0.31")));
        assert_eq!(req("v1.4.2"), RequestedVersion::Concrete(v("1.4.2")));
    }

    #[test]
    fn parse_request_operators_are_ranges() {
        for s in [">=0.23", "^1.2", "~1.4.2", ">=1.0, <2.0", "1.2.*"] {
            assert!(matches!(parse_request(s), Some(RequestedVersion::Range(_))), "{s}");
        }
    }

    #[test]
    fn parse_request_rejects_garbage_and_empty_ranges() {
        assert_eq!(parse_request("not-a-version"), None);
        // contradictory conjunction → empty interval → None
        assert_eq!(parse_request(">=2.0, <1.0"), None);
    }

    #[test]
    fn classify_concrete_against_range() {
        // matches: oracle semantics unchanged from today
        assert_eq!(classify(&req("0.31"), Some(">=0.23")), MatchClass::Satisfies);
        // 0.x: minor mismatch is breaking (role shift)
        assert_eq!(classify(&req("0.10"), Some(">=0.23")), MatchClass::Breaking);
        // 0.x patch mismatch scales as minor: declared <0.23.5 covers ≤0.23.4,
        // so the nearest member is 0.23.4 → distance 5
        assert_eq!(
            classify(&req("0.23.9"), Some(">=0.23.0, <0.23.5")),
            MatchClass::NearMissMinor(5)
        );
        // major>0: minor mismatch scales; ^1.4 = [1.4.0, 2.0.0) so 1.6 satisfies
        assert_eq!(classify(&req("1.6.0"), Some("^1.4")), MatchClass::Satisfies);
        // declared <1.5 covers 1.4.x: request 1.6 → minor distance 2 (1.6 vs 1.4)
        assert_eq!(
            classify(&req("1.6.0"), Some(">=1.4, <1.5")),
            MatchClass::NearMissMinor(2)
        );
        // patch-level miss: declared <1.4.3, request 1.4.5 → patch distance 2
        assert_eq!(
            classify(&req("1.4.5"), Some(">=1.4.0, <1.4.3")),
            MatchClass::NearMissPatch(2)
        );
        // major mismatch always breaking
        assert_eq!(classify(&req("2.0.0"), Some("^1.4")), MatchClass::Breaking);
        // 0.0.x: every mismatch breaking
        assert_eq!(classify(&req("0.0.9"), Some("=0.0.3")), MatchClass::Breaking);
    }

    #[test]
    fn classify_edge_cases() {
        // declared None = unconstrained target → Satisfies
        assert_eq!(classify(&req("0.31"), None), MatchClass::Satisfies);
        // unparseable declared → Unknown
        assert_eq!(classify(&req("0.31"), Some("banana")), MatchClass::Unknown);
        // request sits exactly on an exclusive ceiling: declared covers 1.4.x,
        // so 1.5.0 is one minor past the nearest member → NearMissMinor(1)
        assert_eq!(
            classify(&req("1.5.0"), Some(">=1.4.0, <1.5.0")),
            MatchClass::NearMissMinor(1)
        );
        // range request vs range declared: intersection
        assert_eq!(classify(&req("^1.4"), Some(">=1.0, <2.0")), MatchClass::Satisfies);
        assert_eq!(classify(&req(">=2.0"), Some("^1.4")), MatchClass::Breaking);
    }

    #[test]
    fn class_rank_orders_best_first() {
        let mut classes = vec![
            MatchClass::Breaking,
            MatchClass::NearMissMinor(2),
            MatchClass::Satisfies,
            MatchClass::Unknown,
            MatchClass::NearMissPatch(1),
            MatchClass::NearMissMinor(1),
        ];
        classes.sort_by_key(class_rank);
        assert_eq!(
            classes,
            vec![
                MatchClass::Satisfies,
                MatchClass::NearMissPatch(1),
                MatchClass::NearMissMinor(1),
                MatchClass::NearMissMinor(2),
                MatchClass::Unknown,
                MatchClass::Breaking,
            ]
        );
    }

    proptest! {
        /// Interval desugaring agrees with `VersionReq::matches` (the oracle)
        /// for non-prerelease versions across all comparator ops.
        #[test]
        fn interval_agrees_with_matches_oracle(
            major in 0u64..3, minor in 0u64..50, patch in 0u64..50,
            req_major in 0u64..3, req_minor in 0u64..50, req_patch in 0u64..50,
            op in prop::sample::select(vec!["=", ">", ">=", "<", "<=", "~", "^"]),
        ) {
            let candidate = Version::new(major, minor, patch);
            let raw = format!("{op}{req_major}.{req_minor}.{req_patch}");
            let parsed = VersionReq::parse(&raw).unwrap();
            let interval = VersionInterval::from_req(&parsed).unwrap();
            prop_assert_eq!(interval.contains(&candidate), parsed.matches(&candidate), "{}", raw);
        }
    }
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p mn-core version_match 2>&1 | tail -5`
Expected: panics on `todo!()` (or compile errors until lib.rs is updated — add `pub mod version_match;` first).

- [x] **Step 3: Implement** (replace the `todo!()` bodies)

```rust
pub fn parse_request(raw: &str) -> Option<RequestedVersion> {
    // Concrete first: bare versions stay concrete (spec decision 5).
    if let Some(v) = parse_version(raw) {
        // parse_version accepts bare numeric cores only when no operator
        // characters are present — but it also strips a leading 'v'. Reject the
        // concrete path when the string contains range syntax so e.g. "<1.0"
        // (whose numeric core parses) is not misread. parse_version already
        // returns None for those (non-numeric first char), so this is direct:
        return Some(RequestedVersion::Concrete(v));
    }
    let parsed = VersionReq::parse(raw.trim()).ok()?;
    let interval = VersionInterval::from_req(&parsed)?;
    if interval.is_empty() {
        return None;
    }
    Some(RequestedVersion::Range(interval))
}

pub fn classify(requested: &RequestedVersion, declared: Option<&str>) -> MatchClass {
    let Some(constraint) = declared else {
        return MatchClass::Satisfies; // unconstrained target applies to everything
    };
    let Ok(declared_req) = VersionReq::parse(constraint) else {
        return MatchClass::Unknown;
    };
    // Satisfies check uses the oracle directly for concrete requests so the
    // pre-existing semantics (incl. pre-release subtleties) are unchanged.
    match requested {
        RequestedVersion::Concrete(v) => {
            if declared_req.matches(v) {
                return MatchClass::Satisfies;
            }
        }
        RequestedVersion::Range(ri) => {
            let Some(di) = VersionInterval::from_req(&declared_req) else {
                return MatchClass::Unknown;
            };
            if ri.intersects(&di) {
                return MatchClass::Satisfies;
            }
        }
    }
    let Some(di) = VersionInterval::from_req(&declared_req) else {
        return MatchClass::Unknown;
    };
    let ri = match requested {
        RequestedVersion::Concrete(v) => VersionInterval::point(v.clone()),
        RequestedVersion::Range(r) => r.clone(),
    };
    // Disjoint: compare the nearest actual MEMBERS across the gap (exclusive
    // bounds step to their adjacent representable version — a `<2.0.0` ceiling's
    // nearest member is 1.x, so `>=2.0` vs `^1.4` is Breaking, not a near-miss).
    let Some((a, b)) = gap_pair(&ri, &di) else {
        return MatchClass::Unknown; // defensive; disjoint intervals have a gap side
    };
    mismatch_class(&a, &b)
}

/// The nearest actual member of an interval on the gap side. Inclusive bounds
/// are themselves members; exclusive bounds step one version inward.
fn representative(b: &Bound, is_upper: bool) -> Version {
    if b.inclusive {
        return b.version.clone();
    }
    let v = &b.version;
    if is_upper {
        // Step down at the finest non-zero component.
        if v.patch > 0 {
            Version::new(v.major, v.minor, v.patch - 1)
        } else if v.minor > 0 {
            Version::new(v.major, v.minor - 1, u64::MAX)
        } else if v.major > 0 {
            Version::new(v.major - 1, u64::MAX, u64::MAX)
        } else {
            v.clone() // `<0.0.0` — empty interval; parse_request rejects it
        }
    } else {
        // Step up: the next representable version.
        Version::new(v.major, v.minor, v.patch + 1)
    }
}

/// Nearest-member pair `(request_side, declared_side)` across the gap.
fn gap_pair(req: &VersionInterval, decl: &VersionInterval) -> Option<(Version, Version)> {
    // Request entirely below declared: request's ceiling vs declared's floor.
    if let (Some(r), Some(d)) = (&req.hi, &decl.lo) {
        if r.version <= d.version {
            return Some((representative(r, true), representative(d, false)));
        }
    }
    // Request entirely above declared: request's floor vs declared's ceiling.
    if let (Some(r), Some(d)) = (&req.lo, &decl.hi) {
        if r.version >= d.version {
            return Some((representative(r, false), representative(d, true)));
        }
    }
    None
}

/// The 0.x role shift (spec decision 6): major==0 → minor acts as major
/// (Breaking), patch acts as minor; 0.0.x → every mismatch is Breaking.
fn mismatch_class(a: &Version, b: &Version) -> MatchClass {
    #[allow(clippy::cast_possible_truncation)]
    fn dist(x: u64, y: u64) -> u32 {
        x.abs_diff(y).min(u64::from(u32::MAX)) as u32
    }
    if a == b {
        // Defensive: representatives of valid disjoint intervals differ.
        return MatchClass::NearMissPatch(1);
    }
    if a.major != b.major {
        return MatchClass::Breaking;
    }
    if a.major == 0 {
        if a.minor != b.minor || a.minor == 0 {
            return MatchClass::Breaking; // 0.x minor mismatch / 0.0.x anything
        }
        return MatchClass::NearMissMinor(dist(a.patch, b.patch).max(1));
    }
    if a.minor != b.minor {
        return MatchClass::NearMissMinor(dist(a.minor, b.minor));
    }
    MatchClass::NearMissPatch(dist(a.patch, b.patch).max(1))
}

pub fn class_rank(c: &MatchClass) -> (u8, u32) {
    match c {
        MatchClass::Satisfies => (0, 0),
        MatchClass::NearMissPatch(d) => (1, *d),
        MatchClass::NearMissMinor(d) => (2, *d),
        MatchClass::Unknown => (3, 0),
        MatchClass::Breaking => (4, 0),
    }
}
```

Interval methods:

```rust
impl VersionInterval {
    pub fn point(v: Version) -> Self {
        Self {
            lo: Some(Bound { version: v.clone(), inclusive: true }),
            hi: Some(Bound { version: v, inclusive: true }),
        }
    }

    pub fn from_req(req: &VersionReq) -> Option<Self> {
        let mut iv = Self::unbounded();
        for c in &req.comparators {
            iv = iv.intersect(&comparator_interval(c)?);
        }
        Some(iv)
    }

    pub fn contains(&self, v: &Version) -> bool {
        if let Some(lo) = &self.lo {
            if *v < lo.version || (*v == lo.version && !lo.inclusive) {
                return false;
            }
        }
        if let Some(hi) = &self.hi {
            if *v > hi.version || (*v == hi.version && !hi.inclusive) {
                return false;
            }
        }
        true
    }

    pub fn intersects(&self, other: &Self) -> bool {
        !self.intersect(other).is_empty()
    }

    pub fn is_empty(&self) -> bool {
        match (&self.lo, &self.hi) {
            (Some(lo), Some(hi)) => {
                lo.version > hi.version
                    || (lo.version == hi.version && !(lo.inclusive && hi.inclusive))
            }
            _ => false,
        }
    }

    /// Tightest interval inside both: max of lower bounds, min of upper bounds.
    fn intersect(&self, other: &Self) -> Self {
        let lo = match (&self.lo, &other.lo) {
            (Some(a), Some(b)) => Some(if (b.version.clone(), !b.inclusive) > (a.version.clone(), !a.inclusive) { b.clone() } else { a.clone() }),
            (Some(a), None) => Some(a.clone()),
            (None, b) => b.clone(),
        };
        let hi = match (&self.hi, &other.hi) {
            (Some(a), Some(b)) => Some(if (b.version.clone(), b.inclusive) < (a.version.clone(), a.inclusive) { b.clone() } else { a.clone() }),
            (Some(a), None) => Some(a.clone()),
            (None, b) => b.clone(),
        };
        Self { lo, hi }
    }
}

/// Desugar one comparator per cargo rules. `None` for ops this version of the
/// `semver` crate may add in future (`Op` is non_exhaustive).
fn comparator_interval(c: &Comparator) -> Option<VersionInterval> {
    let maj = c.major;
    let min = c.minor;
    let pat = c.patch;
    let base = Version::new(maj, min.unwrap_or(0), pat.unwrap_or(0));
    let lo_incl = |v: Version| Some(Bound { version: v, inclusive: true });
    let hi_excl = |v: Version| Some(Bound { version: v, inclusive: false });
    let next_major = Version::new(maj + 1, 0, 0);
    let next_minor = |j: u64| Version::new(maj, j + 1, 0);
    Some(match c.op {
        Op::Exact => match (min, pat) {
            (Some(_), Some(_)) => VersionInterval::point(base),
            (Some(j), None) => VersionInterval { lo: lo_incl(base), hi: hi_excl(next_minor(j)) },
            (None, _) => VersionInterval { lo: lo_incl(base), hi: hi_excl(next_major) },
        },
        Op::Greater => match (min, pat) {
            (Some(_), Some(_)) => VersionInterval {
                lo: Some(Bound { version: base, inclusive: false }),
                hi: None,
            },
            (Some(j), None) => VersionInterval { lo: lo_incl(next_minor(j)), hi: None },
            (None, _) => VersionInterval { lo: lo_incl(next_major), hi: None },
        },
        Op::GreaterEq => VersionInterval { lo: lo_incl(base), hi: None },
        Op::Less => VersionInterval { lo: None, hi: hi_excl(base) },
        Op::LessEq => match (min, pat) {
            (Some(_), Some(_)) => VersionInterval {
                lo: None,
                hi: Some(Bound { version: base, inclusive: true }),
            },
            (Some(j), None) => VersionInterval { lo: None, hi: hi_excl(next_minor(j)) },
            (None, _) => VersionInterval { lo: None, hi: hi_excl(next_major) },
        },
        Op::Tilde => match (min, pat) {
            (Some(j), _) => VersionInterval { lo: lo_incl(base), hi: hi_excl(next_minor(j)) },
            (None, _) => VersionInterval { lo: lo_incl(base), hi: hi_excl(next_major) },
        },
        Op::Caret => {
            let hi = if maj > 0 {
                next_major
            } else if min.unwrap_or(0) > 0 {
                next_minor(min.unwrap_or(0))
            } else if pat.is_some() {
                Version::new(0, 0, pat.unwrap_or(0) + 1)
            } else {
                // ^0 == [0.0.0, 1.0.0)
                Version::new(1, 0, 0)
            };
            VersionInterval { lo: lo_incl(base), hi: hi_excl(hi) }
        }
        Op::Wildcard => match (min, pat) {
            (Some(j), _) => VersionInterval { lo: lo_incl(base), hi: hi_excl(next_minor(j)) },
            (None, _) => VersionInterval { lo: lo_incl(base), hi: hi_excl(next_major) },
        },
        _ => return None,
    })
}
```

Note on `^0.J` partials: `^0.2` desugars to `[0.2.0, 0.3.0)` — the `min.unwrap_or(0) > 0` arm. Trust the proptest oracle to catch any caret/tilde partial-form slips and adjust arms until it is green.

- [x] **Step 4: Run to verify pass**

Run: `cargo test -p mn-core version_match`
Expected: all tests PASS including the proptest oracle. If the oracle finds a counterexample, fix the comparator arm it names — the oracle is right.

- [x] **Step 5: Commit**

```bash
git add crates/mn-core/src/version_match.rs crates/mn-core/src/lib.rs
git commit -m "feat(mn-core): version interval parsing + match classification (0.x role shift)"
```

---

### Task 2: Scoring-policy knobs — `floor`/`patch_step`/`minor_step` replace `unsatisfied`

**Files:**
- Modify: `crates/mn-core/src/scoring_policy.rs:87-96` (struct), `:130-134` (default), `:166-183` (validate list), tests `:283-292`

- [x] **Step 1: Write the failing test** (append to the tests module in `scoring_policy.rs`)

```rust
#[test]
fn version_match_knobs_v2() {
    let p = ScoringPolicy::default();
    assert!((p.version_match.satisfies - 1.15).abs() < 1e-12);
    assert!((p.version_match.neutral - 1.00).abs() < 1e-12);
    assert!((p.version_match.floor - 0.30).abs() < 1e-12);
    assert!((p.version_match.patch_step - 0.05).abs() < 1e-12);
    assert!((p.version_match.minor_step - 0.15).abs() < 1e-12);
}

#[test]
fn rejects_legacy_unsatisfied_key() {
    // Hard cutover: a stale policy TOML still carrying `unsatisfied` must fail
    // loudly at startup (deny_unknown_fields).
    let mut body = toml::to_string(&ScoringPolicy::default()).unwrap();
    body.push_str("\n[version_match]\nunsatisfied = 0.7\n");
    // (toml will reject the duplicate table; that IS the loud failure we want —
    // assert any Parse error.)
    assert!(matches!(ScoringPolicy::parse(&body).unwrap_err(), ScoringPolicyError::Parse(_)));
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p mn-core scoring_policy 2>&1 | tail -5`
Expected: FAIL — no field `floor` on `VersionMatchMultipliers`.

- [x] **Step 3: Implement.** Replace `VersionMatchMultipliers` (scoring_policy.rs:89-96):

```rust
/// `[version_match]` — multipliers when query-side version filters are checked
/// against the chunk's declared version constraints (spec §3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionMatchMultipliers {
    /// Chunk's target satisfies the query constraint.
    pub satisfies: f64,
    /// No constraint provided / target absent / unknowable. Neutral.
    pub neutral: f64,
    /// Lower clamp on the permissive near-miss penalty (replaces `unsatisfied`).
    pub floor: f64,
    /// Penalty subtracted per patch-level distance step (permissive mode).
    pub patch_step: f64,
    /// Penalty subtracted per minor-level distance step (permissive mode).
    pub minor_step: f64,
}
```

Update `Default` (`:130-134`) to `satisfies: 1.15, neutral: 1.00, floor: 0.30, patch_step: 0.05, minor_step: 0.15`. In `validate_finite` (`:166-183`) replace the `version_match.unsatisfied` entry with three entries (`floor`, `patch_step`, `minor_step`) and bump the array size `[(&str, f64); 16]` → `18`. Update the old test `rejects_negative_neutral_or_unsatisfied` to mutate `floor` instead of `unsatisfied` (rename it `rejects_negative_neutral_or_floor`). `cargo build -p mn-core` will flag every other `unsatisfied` use — fix them as Task 3 directs (scoring.rs) or temporarily `todo!()`-free by completing Task 3 before pushing.

- [x] **Step 4: Run** — `cargo test -p mn-core scoring_policy` → PASS (scoring.rs may not compile yet; that is Task 3 — if so, do Tasks 2+3 as one commit).

- [x] **Step 5: Commit** (possibly squashed with Task 3)

```bash
git add crates/mn-core/src/scoring_policy.rs
git commit -m "feat(mn-core): version_match policy knobs floor/patch_step/minor_step"
```

---

### Task 3: Scoring takes a precomputed version input; new factor fields

**Files:**
- Modify: `crates/mn-core/src/scoring.rs` (remove `VersionQuery` + `version_match_multiplier`; new input struct; `ConfidenceFactors` fields; `score()` signature; tests)

- [x] **Step 1: Write the failing tests** (replace `version_match_boosts_penalizes_and_neutral` and add):

```rust
#[test]
fn score_applies_precomputed_version_input(/* spec §3.5 */) {
    let p = policy();
    let prov = prov_with(Attribution::Foundation);
    let vin = VersionScoreInput {
        multiplier: 0.85,
        class: "near_miss",
        distance: Some(1),
        query: Some(LanguageTargetQueryFactor {
            name: "compact".into(),
            version_constraint_satisfies: Some("0.31".into()),
        }),
    };
    let r = p.score(&prov, Some(&vin), 0, 0.5, RelevanceSource::Rrf);
    assert!((r.factors.version_match_multiplier - 0.85).abs() < 1e-12);
    assert_eq!(r.factors.version_match_class, Some("near_miss"));
    assert_eq!(r.factors.version_distance, Some(1));
    // absent input → neutral, fields omitted
    let r2 = p.score(&prov, None, 0, 0.5, RelevanceSource::Rrf);
    assert!((r2.factors.version_match_multiplier - 1.0).abs() < 1e-12);
    assert_eq!(r2.factors.version_match_class, None);
    let v = serde_json::to_value(&r2.factors).unwrap();
    assert!(v.get("version_match_class").is_none());
    assert!(v.get("version_distance").is_none());
}

#[test]
fn multiplier_for_class_scales_with_distance(/* spec §3.3 */) {
    let p = policy();
    use crate::version_match::MatchClass;
    assert!((p.version_multiplier(&MatchClass::Satisfies) - 1.15).abs() < 1e-12);
    assert!((p.version_multiplier(&MatchClass::Unknown) - 1.00).abs() < 1e-12);
    assert!((p.version_multiplier(&MatchClass::NearMissPatch(2)) - 0.90).abs() < 1e-12);
    assert!((p.version_multiplier(&MatchClass::NearMissMinor(3)) - 0.55).abs() < 1e-12);
    // floor clamps
    assert!((p.version_multiplier(&MatchClass::NearMissMinor(20)) - 0.30).abs() < 1e-12);
}
```

- [x] **Step 2: Run to verify failure** — `cargo test -p mn-core scoring 2>&1 | tail -5` → compile errors (expected).

- [x] **Step 3: Implement.**

1. Delete `VersionQuery` (scoring.rs:25-33) and `version_match_multiplier` (scoring.rs:161-195). Keep `LanguageTargetQueryFactor` (`:36-43`) — it remains the echo type.
2. Add after `LanguageTargetQueryFactor`:

```rust
/// Precomputed version-match input for [`ScoringPolicy::score`] (spec §3.5).
/// Computed by the search route from the mode + per-facet classification.
#[derive(Debug, Clone)]
pub struct VersionScoreInput {
    /// The multiplier to apply to trust.
    pub multiplier: f64,
    /// `"satisfies" | "near_miss" | "silent" | "unknown"`.
    pub class: &'static str,
    /// Component distance for near misses.
    pub distance: Option<u32>,
    /// Echo of the query-side element that drove the outcome.
    pub query: Option<LanguageTargetQueryFactor>,
}
```

3. Extend `ConfidenceFactors` after `version_match_multiplier` (`:76`):

```rust
    /// Match class, present only when the request carried a version filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_match_class: Option<&'static str>,
    /// Near-miss component distance, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_distance: Option<u32>,
```

4. Add the class→multiplier mapping on `ScoringPolicy` (replacing the deleted method):

```rust
    /// Map a [`crate::version_match::MatchClass`] to its trust multiplier
    /// using the `[version_match]` policy knobs (spec §3.3): linear steps with
    /// a floor. `Breaking` maps to `floor` for completeness — callers drop
    /// Breaking candidates before scoring.
    #[must_use]
    pub fn version_multiplier(&self, class: &crate::version_match::MatchClass) -> f64 {
        use crate::version_match::MatchClass as C;
        let m = &self.version_match;
        match class {
            C::Satisfies => m.satisfies,
            C::Unknown => m.neutral,
            C::Breaking => m.floor,
            C::NearMissPatch(d) => (1.0 - m.patch_step * f64::from(*d)).max(m.floor),
            C::NearMissMinor(d) => (1.0 - m.minor_step * f64::from(*d)).max(m.floor),
        }
    }
```

5. Change `score()` (`:213-220`): parameter `query: Option<&VersionQuery<'_>>` becomes `version: Option<&VersionScoreInput>`; the multiplier line becomes:

```rust
        let version_match_multiplier =
            version.map_or(self.version_match.neutral, |v| v.multiplier);
```

and the factors fill becomes:

```rust
            language_target_query: version.and_then(|v| v.query.clone()),
            language_targets_chunk: provenance.language_targets.clone(),
            version_match_multiplier,
            version_match_class: version.map(|v| v.class),
            version_distance: version.and_then(|v| v.distance),
```

6. Fix remaining mn-core tests: `trust_clamps_when_boost_exceeds_one` and `factors_serialize_with_spec_keys` construct a `VersionScoreInput { multiplier: 1.15, class: "satisfies", distance: None, query: Some(...) }` instead of `VersionQuery`. `factors_serialize_with_spec_keys` keeps asserting `v["language_target_query"]["version_constraint_satisfies"] == "0.31"`.

- [x] **Step 4: Run** — `cargo test -p mn-core` → PASS. (`cargo build -p mn-server` will now fail at search.rs:779/813 — that's Task 5.)

- [x] **Step 5: Commit**

```bash
git add crates/mn-core/src
git commit -m "feat(mn-core): precomputed VersionScoreInput + class/distance factors"
```

---

### Task 4: mn-retrieval — range-accepting validation, mode enum, per-candidate outcomes

**Files:**
- Modify: `crates/mn-retrieval/src/filters.rs` (`check_semver` :359-366, `version_satisfies` :394-401, `semver_post_match` :279-313 replaced, new types, tests)

- [x] **Step 1: Write the failing tests** (append to filters.rs tests; adjust `rejects_malformed_version_satisfies` to add range-acceptance pins):

```rust
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
        Some(FacetVersionOutcome::Classified { class: MatchClass::Satisfies, element: 0 })
    ));
    assert!(out.sdk_dependency.is_none()); // unconstrained facet
    // breaking (0.x minor mismatch)
    let out = mk("0.10").version_outcomes(&prov);
    assert!(matches!(
        out.language_target,
        Some(FacetVersionOutcome::Classified { class: MatchClass::Breaking, .. })
    ));
    // silent: no matching-name target
    let out = mk("0.31").version_outcomes(&Provenance::default());
    assert!(matches!(out.language_target, Some(FacetVersionOutcome::Silent)));
    // name-only element (no version) against a declaring chunk → Satisfies
    let f = SearchFilters {
        language_target: SetMatch {
            any_of: vec![LanguageTargetMatch { name: "compact".into(), version_satisfies: None }],
            none_of: vec![],
        },
        ..Default::default()
    };
    assert!(matches!(
        f.version_outcomes(&prov).language_target,
        Some(FacetVersionOutcome::Classified { class: MatchClass::Satisfies, .. })
    ));
}
```

- [x] **Step 2: Run to verify failure** — `cargo test -p mn-retrieval filters 2>&1 | tail -5` → compile errors.

- [x] **Step 3: Implement.**

1. Mode enum (new, in filters.rs above `SearchFilters`):

```rust
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
```

2. `check_semver` (`:359-366`) now accepts concrete-or-range and rejects empty intervals:

```rust
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
```

3. Outcomes (replaces the boolean `semver_post_match` + private `version_satisfies` fn — delete both):

```rust
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

impl SearchFilters {
    /// Classify a candidate's provenance against the version-bearing facets.
    /// Callers must have run [`SearchFilters::validate`] (unparseable
    /// `version_satisfies` values are treated as absent here).
    #[must_use]
    pub fn version_outcomes(&self, provenance: &Provenance) -> VersionOutcomes {
        use mn_core::version_match::{class_rank, classify, parse_request, MatchClass};

        fn best<'t>(
            elements: impl Iterator<Item = (usize, Option<&'t str>, Vec<Option<&'t str>>)>,
        ) -> Option<FacetVersionOutcome> {
            // (element idx, requested version, matching targets' constraints)
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

        let language_target = if self.language_target.any_of.is_empty() {
            None
        } else {
            best(self.language_target.any_of.iter().enumerate().map(|(i, want)| {
                let constraints: Vec<Option<&str>> = provenance
                    .language_targets
                    .iter()
                    .filter(|have| have.name.eq_ignore_ascii_case(&want.name))
                    .map(|have| have.version_constraint.as_deref())
                    .collect();
                (i, want.version_satisfies.as_deref(), constraints)
            }))
        };
        let sdk_dependency = if self.sdk_dependency.any_of.is_empty() {
            None
        } else {
            best(self.sdk_dependency.any_of.iter().enumerate().map(|(i, want)| {
                let constraints: Vec<Option<&str>> = provenance
                    .sdk_dependencies
                    .iter()
                    .filter(|have| {
                        have.kind.eq_ignore_ascii_case(&want.kind) && have.name == want.name
                    })
                    .map(|have| have.version_constraint.as_deref())
                    .collect();
                (i, want.version_satisfies.as_deref(), constraints)
            }))
        };
        VersionOutcomes { language_target, sdk_dependency }
    }
}
```

4. Update the two existing tests that called `semver_post_match` (`semver_post_match_filters_by_version`, `empty_filter_post_matches_anything`) to use `version_outcomes` semantics (Satisfies vs Breaking; empty filters → both `None`).

- [x] **Step 4: Run** — `cargo test -p mn-retrieval` → PASS. `cargo build -p mn-server` still broken (Task 5).

- [x] **Step 5: Commit**

```bash
git add crates/mn-retrieval/src/filters.rs
git commit -m "feat(mn-retrieval): range version_satisfies + per-facet version outcomes"
```

---

### Task 5: Server route — `version_match` mode, conditional SQL gate, drop/penalize, factors

**Files:**
- Modify: `crates/mn-server/src/routes/search.rs` (request struct :44-100, scoring loop :775-844, rerank derivation :1013-1018, metadata :247-271, predicates :1580-1582)
- Test: `crates/mn-server/tests/search_route.rs` (extend the FR-033 tests)

- [x] **Step 1: Add the request field + metadata echo.** In `SearchRequest` after `include_scores` (search.rs:80):

```rust
    /// Version-matching mode for the semver-bearing facets (spec §3): strict
    /// hard-filters; permissive (default) biases, dropping only breaking
    /// mismatches among version-declaring content.
    #[serde(default)]
    pub version_match: mn_retrieval::filters::VersionMatchMode,
```

In `SearchMetadata` after `code_mode` (`:268`):

```rust
    /// The version-matching mode applied (echoes the request; default permissive).
    pub version_match: mn_retrieval::filters::VersionMatchMode,
```

and populate it in the response assembly (`:926`): `version_match: req.version_match,`. (`VersionMatchMode` already derives `Serialize`.)

- [x] **Step 2: Gate the SQL name predicates on strict mode.** In `push_filter_predicates` the two name-gate calls (`:1581-1582`) move behind the mode. `push_filter_predicates` and `push_filter_joins`/`needs_document_join` gain a `mode: VersionMatchMode` parameter (thread it through from `vector_search`/`code_vector_search`/`fts_search`, which gain the same parameter from the handler — `req.version_match`):

```rust
    // -- language_target / sdk_dependency: name membership in SQL (strict only;
    //    permissive is a pure ranking signal, spec §3.3) --
    if mode == mn_retrieval::filters::VersionMatchMode::Strict {
        push_language_target_names(qb, &f.language_target);
        push_sdk_dependency_names(qb, &f.sdk_dependency);
    }
```

In `needs_document_join` (`:1489-1501`) the `language_target`/`sdk_dependency` terms also become mode-aware — permissive must NOT force the document join for these two facets (provenance is fetched post-RRF anyway): change the signature to `needs_document_join(f: &SearchFilters, mode: VersionMatchMode)` and wrap the two terms in `(mode == VersionMatchMode::Strict && (...))`.

- [x] **Step 3: Replace the post-match + scoring-input block.** Replace search.rs:775-802 (the `version_query` construction and the `semver_post_match` gate inside the loop) with:

```rust
    let mode = req.version_match;
    let now = OffsetDateTime::now_utc();

    let mut scored: Vec<ScoredCandidate> = Vec::with_capacity(fused.len());
    for (chunk_id, rrf_score) in fused {
        let Some(row) = rows.get(&chunk_id) else {
            continue;
        };
        // Version-bearing facets (FR-033, spec §3): classify, then drop per
        // mode — strict drops everything not Satisfies; permissive drops only
        // Breaking. Scalar facets were already enforced in SQL.
        let outcomes = req.filters.version_outcomes(&row.provenance);
        let version_input = match version_decision(&req.filters, outcomes, mode, &state.scoring_policy) {
            VersionDecision::Drop => continue,
            VersionDecision::Score(v) => v,
        };
```

and pass it to scoring (`:813-819` becomes):

```rust
        let score = state.scoring_policy.score(
            &row.provenance,
            version_input.as_ref(),
            age_days,
            relevance,
            RelevanceSource::Rrf,
        );
```

Add the decision helper near the bottom of the file (above the tests):

```rust
/// What the version facets decided for one candidate.
enum VersionDecision {
    /// Candidate is removed (strict non-satisfies, or permissive Breaking).
    Drop,
    /// Candidate is scored with this input (`None` = no version filter).
    Score(Option<mn_core::scoring::VersionScoreInput>),
}

/// Combine per-facet outcomes into a drop/score decision (spec §3.2/§3.3).
/// Combined multiplier = min across constrained facets (worst offender).
fn version_decision(
    filters: &SearchFilters,
    outcomes: mn_retrieval::filters::VersionOutcomes,
    mode: mn_retrieval::filters::VersionMatchMode,
    policy: &mn_core::scoring_policy::ScoringPolicy,
) -> VersionDecision {
    use mn_core::scoring::{LanguageTargetQueryFactor, VersionScoreInput};
    use mn_core::version_match::MatchClass;
    use mn_retrieval::filters::{FacetVersionOutcome, VersionMatchMode};

    let constrained: Vec<FacetVersionOutcome> = [outcomes.language_target, outcomes.sdk_dependency]
        .into_iter()
        .flatten()
        .collect();
    if constrained.is_empty() {
        return VersionDecision::Score(None);
    }
    match mode {
        VersionMatchMode::Strict => {
            // Anything not Satisfies (incl. Silent/Unknown) drops — unchanged
            // hard-filter semantics.
            let all_satisfy = constrained.iter().all(|o| {
                matches!(o, FacetVersionOutcome::Classified { class: MatchClass::Satisfies, .. })
            });
            if !all_satisfy {
                return VersionDecision::Drop;
            }
        }
        VersionMatchMode::Permissive => {
            if constrained.iter().any(|o| {
                matches!(o, FacetVersionOutcome::Classified { class: MatchClass::Breaking, .. })
            }) {
                return VersionDecision::Drop;
            }
        }
    }
    // Per-facet multiplier; Silent → neutral. Track the worst (min) facet.
    let facet_eval = |o: &FacetVersionOutcome| -> (f64, &'static str, Option<u32>) {
        match o {
            FacetVersionOutcome::Silent => (policy.version_match.neutral, "silent", None),
            FacetVersionOutcome::Classified { class, .. } => {
                let m = policy.version_multiplier(class);
                let (label, dist) = match class {
                    MatchClass::Satisfies => ("satisfies", None),
                    MatchClass::NearMissPatch(d) | MatchClass::NearMissMinor(d) => {
                        ("near_miss", Some(*d))
                    }
                    MatchClass::Unknown => ("unknown", None),
                    MatchClass::Breaking => ("near_miss", None), // dropped above; unreachable
                };
                (m, label, dist)
            }
        }
    };
    let (multiplier, class, distance) = constrained
        .iter()
        .map(facet_eval)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .expect("constrained is non-empty");
    // Echo the language element that won (when the language facet is constrained).
    let query = match outcomes.language_target {
        Some(FacetVersionOutcome::Classified { element, .. }) => filters
            .language_target
            .any_of
            .get(element)
            .map(|lt| LanguageTargetQueryFactor {
                name: lt.name.clone(),
                version_constraint_satisfies: lt.version_satisfies.clone(),
            }),
        _ => filters.language_target.any_of.first().map(|lt| LanguageTargetQueryFactor {
            name: lt.name.clone(),
            version_constraint_satisfies: lt.version_satisfies.clone(),
        }),
    };
    VersionDecision::Score(Some(VersionScoreInput { multiplier, class, distance, query }))
}
```

- [x] **Step 4: Rerank derivation uses the first version-bearing element** (search.rs:1013-1017, per the spec clarification in the header):

```rust
        let version = req
            .filters
            .language_target
            .any_of
            .iter()
            .find(|lt| lt.version_satisfies.is_some())
            .and_then(|lt| lt.version_satisfies.as_deref().map(|v| (lt.name.as_str(), v)));
```

- [x] **Step 5: Compile + unit tests**

Run: `cargo build -p mn-server && cargo test -p mn-server --lib 2>&1 | tail -3` (route unit tests live in the binary crate's modules)
Expected: builds; existing unit tests pass.

- [x] **Step 6: Integration tests** (append to `crates/mn-server/tests/search_route.rs`, modeled on the existing `semver_filters_language_target_and_sdk_dependency` fixture at :1148-1172 which seeds a chunk with `language_targets: [{compact, >=0.23}]`):

Cover, with exact assertions:
1. `permissive_is_default_and_soft`: a `language_target {name:"compact", version_satisfies:"0.31"}` filter with NO `version_match` field returns BOTH the declaring chunk (factors: `version_match_class == "satisfies"`, `version_match_multiplier > 1.0`) and a provenance-less chunk (`version_match_class == "silent"`, multiplier 1.0). `search_metadata.version_match == "permissive"`.
2. `permissive_drops_breaking`: request `version_satisfies:"0.10"` (0.x minor mismatch vs `>=0.23`) → the declaring chunk is absent; the silent chunk still returns.
3. `permissive_near_miss_penalized`: seed a chunk with `version_constraint: ">=1.4.0, <1.5.0"` (covers 1.4.x), request `"1.5.0"` → present with `version_match_class == "near_miss"`, `version_distance == 1`, multiplier `0.85 ± 1e-9` (minor_step 0.15 × 1).
4. `strict_mode_preserves_hard_filtering`: same fixtures, body `"version_match": "strict"` → only the satisfying chunk returns (silent chunk excluded by the SQL name gate); request `"0.10"` strict → zero results.
5. `range_request_accepted`: `version_satisfies: ">=0.23"` validates and matches the declaring chunk (Satisfies via intersection).
6. `invalid_version_match_value_400s`: body `"version_match": "fuzzy"` → 400.

- [x] **Step 7: Compile integration tests, run unit suite**

Run: `cargo test -p mn-server --no-run --features integration && cargo test -p mn-server`
Expected: compiles; unit tests pass (integration runs in CI).

- [x] **Step 8: Commit**

```bash
git add crates/mn-server/src crates/mn-server/tests
git commit -m "feat(mn-server): strict/permissive version matching in /v1/search (permissive default)"
```

---

### Task 6: `/v1/facets` — two-level drill for version facets + package versions

**Files:**
- Modify: `crates/mn-server/src/routes/facets.rs` (`FacetsQuery` :55-62, `drill_queries` :161-192, `facet_values_page` :198-274, overview descriptors)
- Test: `crates/mn-server/tests/facets_route.rs` (or the existing facets integration test file — check `ls crates/mn-server/tests/ | grep -i facet` and append there)

- [x] **Step 1: Extend the query params.** `FacetsQuery` gains:

```rust
    /// Second drill level: the level-1 value to enumerate within (e.g. the
    /// language-target name, the `kind:name` dependency composite, or the
    /// package name).
    within: Option<String>,
```

- [x] **Step 2: Rework `drill_queries`.** New signature returns whether a `within` bind is used; level-1 vs level-2 SQL per facet:

```rust
/// Per-facet drill SQL. Level 1 (`within = None`) enumerates names; level 2
/// (`within = Some`) enumerates version values inside one name (spec §4).
/// Returns `(page_sql, count_sql, takes_within)`.
fn drill_queries(facet: &str, within: bool) -> Option<(&'static str, &'static str, bool)> {
    match (facet, within) {
        ("source_slug", false) => Some((/* existing pair */, false)),
        ("language", false) => Some((/* existing pair */, false)),
        ("tags", false) => Some((/* existing pair */, false)),
        ("package", false) => Some((/* existing pair */, false)),
        ("package", true) => Some((
            "SELECT DISTINCT p.version AS v FROM package p \
             JOIN source_version sv ON sv.id = p.source_version_id \
             WHERE sv.is_active = true AND p.name = $3 AND p.version IS NOT NULL \
             AND p.version > $1 ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT p.version) FROM package p \
             JOIN source_version sv ON sv.id = p.source_version_id \
             WHERE sv.is_active = true AND p.name = $1 AND p.version IS NOT NULL",
            true,
        )),
        ("language_target", false) => Some((
            "SELECT DISTINCT lt->>'name' AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt \
             WHERE sv.is_active = true AND lt->>'name' IS NOT NULL AND lt->>'name' > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT lt->>'name') FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt \
             WHERE sv.is_active = true AND lt->>'name' IS NOT NULL",
            false,
        )),
        ("language_target", true) => Some((
            "SELECT DISTINCT lt->>'version_constraint' AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt \
             WHERE sv.is_active = true AND lt->>'name' = $3 \
             AND lt->>'version_constraint' IS NOT NULL AND lt->>'version_constraint' > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT lt->>'version_constraint') FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt \
             WHERE sv.is_active = true AND lt->>'name' = $1 \
             AND lt->>'version_constraint' IS NOT NULL",
            true,
        )),
        ("sdk_dependency", false) => Some((
            "SELECT DISTINCT (dep->>'kind') || ':' || (dep->>'name') AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'sdk_dependencies','[]'::jsonb)) dep \
             WHERE sv.is_active = true AND dep->>'name' IS NOT NULL \
             AND (dep->>'kind') || ':' || (dep->>'name') > $1 ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT (dep->>'kind') || ':' || (dep->>'name')) FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'sdk_dependencies','[]'::jsonb)) dep \
             WHERE sv.is_active = true AND dep->>'name' IS NOT NULL",
            false,
        )),
        ("sdk_dependency", true) => Some((
            "SELECT DISTINCT dep->>'version_constraint' AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'sdk_dependencies','[]'::jsonb)) dep \
             WHERE sv.is_active = true AND (dep->>'kind') || ':' || (dep->>'name') = $3 \
             AND dep->>'version_constraint' IS NOT NULL AND dep->>'version_constraint' > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT dep->>'version_constraint') FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'sdk_dependencies','[]'::jsonb)) dep \
             WHERE sv.is_active = true AND (dep->>'kind') || ':' || (dep->>'name') = $1 \
             AND dep->>'version_constraint' IS NOT NULL",
            true,
        )),
        _ => None,
    }
}
```

(Keep the four existing level-1 strings verbatim from :163-189 — they're elided above only for plan brevity; copy them in place.)

- [x] **Step 3: Thread `within` through `facet_values_page`.** Resolve `(page_sql, count_sql, takes_within) = drill_queries(facet, q.within.is_some())`; when `None` and `q.within.is_some()` the error message reads `facet `{facet}` has no `within` drill level`; the no-within not-drillable message becomes `"drillable facets: source_slug, language, tags, package, language_target, sdk_dependency"`. Bind order: count query binds `within` as `$1` when `takes_within`; page query binds `(after, limit+1, within)`. Response JSON gains `"within": q.within` when present.

- [x] **Step 4: Overview advertises the levels.** Where the overview assembles descriptors for object-set facets (find the loop over `facets::facets()` in this file building the JSON body), add for the two version facets and package:

```rust
// language_target / sdk_dependency / package gain drill metadata:
//   "drill_levels": ["name", "version_constraint"]   (lt / dep)
//   "drill_levels": ["name", "version"]               (package)
```

(Exact splice point: read the overview-builder function in this file first; it iterates the registry and pattern-matches keys for `values`/`total` — add a `drill_levels` key in the same match.)

- [x] **Step 5: Integration tests** (CI): seed provenance-bearing documents (reuse the search_route.rs fixture style) and assert: level-1 `?facet=language_target` returns `["compact"]`; level-2 `?facet=language_target&within=compact` returns `[">=0.23"]`; `?facet=sdk_dependency` returns `["npm:@midnight-ntwrk/midnight-js"]`; `?facet=package&within=<name>` returns the version; `?facet=verified` still 400s; `?facet=language&within=x` 400s with the no-level message.

- [x] **Step 6: Compile + commit**

```bash
cargo test -p mn-server --no-run --features integration && cargo test -p mn-server
git add crates/mn-server/src/routes/facets.rs crates/mn-server/tests
git commit -m "feat(mn-server): two-level facet drill for language_target/sdk_dependency/package versions"
```

---

### Task 7: `PackageRef.version` flows end to end

**Files:**
- Modify: `crates/mn-core/src/types.rs:241-248` (PackageRef), `crates/mn-content/src/package.rs` (DetectedPackage + detect), `crates/mn-content/src/code/compact.rs:211-215` (struct literal), `crates/mn-cli/src/commands/ingest/run.rs:1458-1472` (mapping), `crates/mn-server/src/routes/admin_ingest.rs:915` (upsert call)

- [ ] **Step 1: Failing test** (append to `crates/mn-content/src/package.rs` tests):

```rust
#[test]
fn version_extracted_from_manifests() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"midnight-foo\"\nversion = \"0.3.1\"\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    let f = dir.path().join("src/lib.rs");
    fs::write(&f, "fn x() {}").unwrap();
    assert_eq!(detect(&f, dir.path()).unwrap().version.as_deref(), Some("0.3.1"));

    let dir2 = tempfile::tempdir().unwrap();
    fs::write(dir2.path().join("package.json"), r#"{"name":"@scope/web","version":"2.1.0"}"#)
        .unwrap();
    let f2 = dir2.path().join("src/index.ts");
    fs::create_dir_all(f2.parent().unwrap()).unwrap();
    fs::write(&f2, "export const x=1;").unwrap();
    assert_eq!(detect(&f2, dir2.path()).unwrap().version.as_deref(), Some("2.1.0"));
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p mn-content package 2>&1 | tail -3` → no field `version`.

- [ ] **Step 3: Implement.**

1. `DetectedPackage` gains `pub version: Option<String>` (doc: `/// [package].version / .version from the manifest, when declared.`). In `detect`, read it alongside the name: cargo arm `v.get("package").and_then(|p| p.get("version")).and_then(toml::Value::as_str).map(str::to_owned)`; npm arm `v.get("version").and_then(serde_json::Value::as_str).map(str::to_owned)`.
2. `PackageRef` (types.rs:241-248) gains:

```rust
    /// Manifest-declared version of the package itself, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
```

3. Fix struct literals (compiler-driven): `run.rs:1467-1471` adds `version: p.version,`; `compact.rs:211-215` adds `version: None,`; any test fixtures the build flags.
4. `admin_ingest.rs:915` passes it through:

```rust
            package::upsert(pool, sv_id, kind, &pkg.name, pkg.version.as_deref(), pkg.manifest_path.as_deref())
```

- [ ] **Step 4: Run** — `cargo test -p mn-content && cargo build --workspace` → PASS/builds.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-core/src/types.rs crates/mn-content/src crates/mn-cli/src crates/mn-server/src
git commit -m "feat: populate package.version from Cargo.toml/package.json (was hardcoded NULL)"
```

---

### Task 8: mn-content extraction module — pragma + allowlisted deps

**Files:**
- Create: `crates/mn-content/src/extract.rs`
- Modify: `crates/mn-content/src/lib.rs` (`pub mod extract;`), `crates/mn-content/src/code/compact.rs` (pragma helper, behind the existing `compact` feature)

- [ ] **Step 1: Confirm the OpenZeppelin Compact npm scope** (allowlist accuracy):

Run: `npm search openzeppelin compact --searchlimit 5 2>/dev/null || true` and check the scope used by OpenZeppelin's Compact contracts repo (e.g. `@openzeppelin-compact/...`). Use whatever scope exists in the allowlist below; if none exists on npm, keep only `@midnight-ntwrk/`.

- [ ] **Step 2: Failing tests.** In `crates/mn-content/src/code/compact.rs` tests (feature-gated module):

```rust
#[test]
fn language_version_pragma_extracted() {
    let body = "pragma language_version >= 0.23;\nledger x: Uint<8>;\n";
    assert_eq!(detect_language_version(body).as_deref(), Some(">=0.23"));
    // legacy compiler pragma is NOT extracted (spec §1.1)
    assert_eq!(detect_language_version("pragma compact 0.15.0;\n"), None);
    // conjunction normalizes && → comma
    let body = "pragma language_version >= 0.13 && <= 0.17;\n";
    assert_eq!(detect_language_version(body).as_deref(), Some(">=0.13,<=0.17"));
    // garbage expr (won't parse as VersionReq) → None
    assert_eq!(detect_language_version("pragma language_version banana;\n"), None);
}
```

In the new `crates/mn-content/src/extract.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn npm_deps_filtered_by_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"app","version":"1.0.0","dependencies":{
                "@midnight-ntwrk/midnight-js":"^1.4.0","react":"^19.0.0"},
                "devDependencies":{"@midnight-ntwrk/compact-runtime":"^0.9.0"}}"#,
        )
        .unwrap();
        let deps = extract_manifest_deps(&dir.path().join("package.json"), dir.path());
        assert_eq!(deps.len(), 1, "dev-deps and non-allowlisted excluded");
        assert_eq!(deps[0].kind, "npm");
        assert_eq!(deps[0].name, "@midnight-ntwrk/midnight-js");
        assert_eq!(deps[0].version_constraint.as_deref(), Some("^1.4.0"));
    }

    #[test]
    fn cargo_deps_with_workspace_inheritance() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers=[\"a\"]\n[workspace.dependencies]\nmidnight-ledger = \"2.1\"\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("a")).unwrap();
        fs::write(
            dir.path().join("a/Cargo.toml"),
            "[package]\nname=\"a\"\nversion=\"0.1.0\"\n[dependencies]\nmidnight-ledger = { workspace = true }\nserde = \"1\"\n",
        )
        .unwrap();
        let deps = extract_manifest_deps(&dir.path().join("a/Cargo.toml"), dir.path());
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].kind, "cargo");
        assert_eq!(deps[0].name, "midnight-ledger");
        assert_eq!(deps[0].version_constraint.as_deref(), Some("2.1"));
    }
}
```

- [ ] **Step 3: Run to verify failure** — `cargo test -p mn-content extract compact 2>&1 | tail -3` → compile errors.

- [ ] **Step 4: Implement.**

`compact.rs` (next to `detect_module_package`, same feature gate):

```rust
/// Extract the `pragma language_version <expr>;` constraint from a Compact
/// file (spec §1.1). Only the `language_version` pragma is read — the legacy
/// `pragma compact X` form states a compiler version. The expression is
/// normalized (whitespace stripped, `&&` → `,`) and must parse as a
/// `semver::VersionReq`, else `None` (warn-and-skip, never fatal).
#[must_use]
pub fn detect_language_version(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return None;
    }
    let parsed = compactp_parser::parse(body);
    let root = SyntaxNode::new_root(parsed.green);
    let file = SourceFile::cast(root)?;
    for pragma in file.pragmas() {
        let Some(name) = pragma.name() else { continue };
        if name.text() != "language_version" {
            continue;
        }
        let full = pragma.syntax().text().to_string();
        let expr = full
            .trim_start()
            .strip_prefix("pragma")?
            .trim_start()
            .strip_prefix("language_version")?
            .trim()
            .trim_end_matches(';')
            .trim()
            .replace("&&", ",");
        let normalized: String = expr.split_whitespace().collect::<Vec<_>>().join("");
        if normalized.is_empty() || semver::VersionReq::parse(&normalized).is_err() {
            tracing::warn!(expr = %expr, "unparseable language_version pragma; skipping extraction");
            return None;
        }
        return Some(normalized);
    }
    None
}
```

(`AstNode` must be in scope for `.syntax()` — it already is for `cast`; add `semver` to `mn-content`'s `[dependencies]` as `semver = { workspace = true }`.)

`extract.rs`:

```rust
//! Version-provenance extraction from code manifests (spec §1.1). Pure
//! filesystem readers — called from the ingest CLI alongside package detection.

use std::path::Path;

use mn_core::provenance::SdkDependency;

/// npm scopes/prefixes whose dependencies are compatibility-relevant.
pub const NPM_ALLOWLIST_PREFIXES: &[&str] = &["@midnight-ntwrk/"];
/// cargo crate-name prefixes whose dependencies are compatibility-relevant.
pub const CARGO_ALLOWLIST_PREFIXES: &[&str] = &["midnight-", "mn-"];

fn allowlisted(kind: &str, name: &str) -> bool {
    let prefixes = if kind == "npm" { NPM_ALLOWLIST_PREFIXES } else { CARGO_ALLOWLIST_PREFIXES };
    prefixes.iter().any(|p| name.starts_with(p))
}

/// Extract allowlisted dependencies (npm `dependencies` / cargo
/// `[dependencies]`; dev-deps excluded) from one manifest. `source_root`
/// bounds the upward walk for cargo `workspace = true` resolution. Read or
/// parse failures return empty — extraction is never fatal (spec §6).
#[must_use]
pub fn extract_manifest_deps(manifest_abs: &Path, source_root: &Path) -> Vec<SdkDependency> {
    match manifest_abs.file_name().and_then(|n| n.to_str()) {
        Some("package.json") => extract_npm(manifest_abs),
        Some("Cargo.toml") => extract_cargo(manifest_abs, source_root),
        _ => Vec::new(),
    }
}

fn extract_npm(path: &Path) -> Vec<SdkDependency> {
    let Ok(txt) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { return Vec::new() };
    let Some(deps) = v.get("dependencies").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    deps.iter()
        .filter(|(name, _)| allowlisted("npm", name))
        .map(|(name, range)| SdkDependency {
            kind: "npm".into(),
            name: name.clone(),
            version_constraint: range.as_str().map(str::to_owned),
        })
        .collect()
}

fn extract_cargo(path: &Path, source_root: &Path) -> Vec<SdkDependency> {
    let Ok(txt) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(v) = txt.parse::<toml::Value>() else { return Vec::new() };
    let Some(deps) = v.get("dependencies").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    deps.iter()
        .filter(|(name, _)| allowlisted("cargo", name))
        .map(|(name, spec)| SdkDependency {
            kind: "cargo".into(),
            name: name.clone(),
            version_constraint: cargo_constraint(spec)
                .or_else(|| workspace_constraint(path, source_root, name)),
        })
        .collect()
}

/// `"1.4"` or `{ version = "1.4", ... }` → the constraint string.
fn cargo_constraint(spec: &toml::Value) -> Option<String> {
    match spec {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Table(t) => t.get("version").and_then(toml::Value::as_str).map(str::to_owned),
        _ => None,
    }
}

/// Resolve `{ workspace = true }` by walking up to the nearest
/// `[workspace.dependencies]` table within `source_root`.
fn workspace_constraint(manifest: &Path, source_root: &Path, dep: &str) -> Option<String> {
    let mut dir = manifest.parent()?.parent();
    while let Some(d) = dir {
        let candidate = d.join("Cargo.toml");
        if candidate.is_file() {
            if let Ok(txt) = std::fs::read_to_string(&candidate) {
                if let Ok(v) = txt.parse::<toml::Value>() {
                    if let Some(spec) = v
                        .get("workspace")
                        .and_then(|w| w.get("dependencies"))
                        .and_then(|t| t.get(dep))
                    {
                        return cargo_constraint(spec);
                    }
                }
            }
        }
        if d == source_root {
            break;
        }
        dir = d.parent();
    }
    None
}
```

- [ ] **Step 5: Run** — `cargo test -p mn-content` → PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-content
git commit -m "feat(mn-content): pragma + allowlisted-dependency version extraction"
```

---

### Task 9: Three-layer provenance merge + `no_extract` manifest flag

**Files:**
- Modify: `crates/mn-content/src/ingest/plan.rs` (`WalkContext` :177-193, `merge_provenance` :332-370, call site :315-318), `crates/mn-content/src/manifest/mod.rs:44-70` (node flag), `crates/mn-content/src/manifest/resolve.rs` (ResolvedLeaf + inheritance)

- [ ] **Step 1: Failing tests** (append to plan.rs tests module):

```rust
#[test]
fn merge_precedence_frontmatter_extracted_manifest() {
    use mn_core::provenance::{LanguageTarget, Provenance};
    let fm = Provenance {
        language_targets: vec![LanguageTarget { name: "compact".into(), version_constraint: Some(">=0.30".into()) }],
        ..Provenance::default()
    };
    let extracted = Provenance {
        language_targets: vec![LanguageTarget { name: "compact".into(), version_constraint: Some(">=0.23".into()) }],
        sdk_dependencies: vec![mn_core::provenance::SdkDependency {
            kind: "npm".into(), name: "@midnight-ntwrk/midnight-js".into(),
            version_constraint: Some("^1.4.0".into()),
        }],
        ..Provenance::default()
    };
    let manifest = Provenance::attributed_to(mn_core::provenance::Attribution::Foundation);
    let merged = merge_provenance(&fm, &extracted, &manifest);
    // frontmatter beats extracted
    assert_eq!(merged.language_targets[0].version_constraint.as_deref(), Some(">=0.30"));
    // extracted fills what frontmatter lacks
    assert_eq!(merged.sdk_dependencies.len(), 1);
    // manifest fills what both lack
    assert_eq!(merged.attribution, mn_core::provenance::Attribution::Foundation);
    // no frontmatter → extracted wins the lists
    let merged2 = merge_provenance(&Provenance::default(), &extracted, &manifest);
    assert_eq!(merged2.language_targets[0].version_constraint.as_deref(), Some(">=0.23"));
}
```

And in `resolve.rs` tests (mirror `leaf_provenance_overrides_ancestor_fieldwise` at :373):

```rust
#[test]
fn no_extract_inherits_and_leaf_overrides() {
    let yaml = r"
manifest_version: 1
root:
  path: docs
  no_extract: true
  children:
    - file: docs/a.compact
    - file: docs/b.compact
      no_extract: false
";
    let m = Manifest::parse(yaml).unwrap();
    // (write the two files under a tempdir `docs/` before resolving)
    // a inherits true; b overrides to false
}
```

(Complete the fixture with the same tempdir scaffolding the neighboring resolve tests use — read them first and mirror exactly.)

- [ ] **Step 2: Run to verify failure** — `cargo test -p mn-content merge_precedence no_extract 2>&1 | tail -3`.

- [ ] **Step 3: Implement.**

1. `ManifestNode` gains (after `provenance`):

```rust
    /// Disable pipeline version extraction for this subtree (spec §1.3).
    /// Inheritable; a child's explicit value overrides the ancestor's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_extract: Option<bool>,
```

2. `ResolvedLeaf` gains `pub no_extract: bool` (doc: `/// Inherited extraction opt-out (default false).`). In `resolve.rs`, thread it the same way `published_url` inherits: the walk carries the current effective value (`node.no_extract.unwrap_or(inherited)`), and both `ResolvedLeaf` push sites (:81, :109) set it. Read the function before editing — mirror the existing inheritance parameter style exactly.
3. `WalkContext` gains `pub extracted: Provenance` (doc: `/// Machine-extracted version provenance (computed by the caller; spec §1).`).
4. `merge_provenance` becomes three-layer (keep the existing body as the `overlay` helper):

```rust
/// Most-specific wins: frontmatter > extracted > manifest ancestor (spec §1.2).
fn merge_provenance(frontmatter: &Provenance, extracted: &Provenance, ancestor: &Provenance) -> Provenance {
    overlay(frontmatter, &overlay(extracted, ancestor))
}

/// `top` wins per-field over `base`; non-empty lists replace wholesale.
fn overlay(top: &Provenance, base: &Provenance) -> Provenance {
    // (this is the existing merge_provenance body verbatim, with
    //  `frontmatter` renamed `top` and `ancestor` renamed `base`)
}
```

Call site (:315-318): `merge_provenance(&walked.split.provenance, &walked.extracted, &walked.resolved.provenance_override)`. Fix every `WalkContext` construction the compiler flags (plan.rs tests, mn-cli run.rs — for now pass `Provenance::default()`; Task 10 fills it).

- [ ] **Step 4: Run** — `cargo test -p mn-content && cargo build --workspace` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content crates/mn-cli
git commit -m "feat(mn-content): three-layer provenance merge + no_extract manifest flag"
```

---

### Task 10: mn-cli wiring — build extracted provenance per document + report counts

**Files:**
- Modify: `crates/mn-cli/src/commands/ingest/run.rs` (walk loop :400-413, summary output, new helper near `detect_package_ref` :1458)
- Test: `crates/mn-cli/src/commands/ingest/run.rs` tests module (next to the existing `detect_package_ref` tests :2078)

- [ ] **Step 1: Failing test** (append to run.rs tests):

```rust
#[test]
fn extracted_provenance_for_code_files() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("package.json"),
        r#"{"name":"app","version":"1.0.0","dependencies":{"@midnight-ntwrk/midnight-js":"^1.4.0"}}"#,
    )
    .unwrap();
    std::fs::write(root.path().join("src/x.compact"), "pragma language_version >= 0.23;\n").unwrap();
    std::fs::write(root.path().join("src/y.ts"), "export const x = 1;").unwrap();
    std::fs::write(root.path().join("README.md"), "# hi").unwrap();

    let compact = build_extracted(root.path(), Path::new("src/x.compact"),
        "pragma language_version >= 0.23;\n", mn_core::types::DocumentKind::Code);
    assert_eq!(compact.language_targets[0].name, "compact");
    assert_eq!(compact.language_targets[0].version_constraint.as_deref(), Some(">=0.23"));

    let ts = build_extracted(root.path(), Path::new("src/y.ts"), "export const x = 1;",
        mn_core::types::DocumentKind::Code);
    assert_eq!(ts.sdk_dependencies.len(), 1);

    // prose: never extracted (spec §1)
    let md = build_extracted(root.path(), Path::new("README.md"), "# hi",
        mn_core::types::DocumentKind::Markdown);
    assert!(md.language_targets.is_empty() && md.sdk_dependencies.is_empty());
}
```

(Adjust the `DocumentKind` path to wherever the enum actually lives — `grep -rn "enum DocumentKind" crates/` first; it is the same type `WalkContext.kind` uses.)

- [ ] **Step 2: Run to verify failure** — `cargo test -p mn-cli extracted_provenance 2>&1 | tail -3`.

- [ ] **Step 3: Implement.** Helper next to `detect_package_ref`:

```rust
/// Machine-extract version provenance for one walked file (spec §1.1): code
/// documents only — pragma constraints for `.compact`, allowlisted manifest
/// dependencies for files in an npm/cargo package. Prose gets nothing.
fn build_extracted(
    source_root: &std::path::Path,
    rel_path: &std::path::Path,
    content: &str,
    kind: mn_core::types::DocumentKind,
) -> mn_core::provenance::Provenance {
    use mn_core::provenance::{LanguageTarget, Provenance};
    if kind != mn_core::types::DocumentKind::Code {
        return Provenance::default();
    }
    let mut out = Provenance::default();
    if rel_path.extension().and_then(|e| e.to_str()) == Some("compact") {
        if let Some(expr) = mn_content::code::compact::detect_language_version(content) {
            out.language_targets =
                vec![LanguageTarget { name: "compact".into(), version_constraint: Some(expr) }];
        }
    }
    let abs = source_root.join(rel_path);
    if let Some(pkg) = mn_content::package::detect(&abs, source_root) {
        let manifest_abs = source_root.join(&pkg.manifest_path);
        out.sdk_dependencies = mn_content::extract::extract_manifest_deps(&manifest_abs, source_root);
    }
    out
}
```

(If `mn_content::code::compact` isn't publicly reachable, re-export the function from `mn-content/src/lib.rs` as `pub use code::compact::detect_language_version;` guarded by the `compact` feature, with a `#[cfg(not(feature = "compact"))]` stub returning `None` — mirror how `detect_compact_package` at lib.rs:30 is exposed.)

Walk loop (:401-409): respect the opt-out and pass the field —

```rust
        let extracted = if doc.resolved.no_extract {
            mn_core::provenance::Provenance::default()
        } else {
            build_extracted(&source_root, &doc.rel_path, &doc.content, doc.resolved.kind)
        };
        let ctx = WalkContext {
            // ...existing fields...
            extracted,
            package: detect_package_ref(&source_root, &doc.rel_path, &doc.content),
        };
```

Report counts: after `builder.finalize()`, compute and add to the `phase_done("chunk", ...)` JSON (and the final summary if one exists — grep `documents_added` for the summary site):

```rust
    let docs_with_language_targets =
        plan.new_documents.iter().filter(|d| !d.provenance.language_targets.is_empty()).count();
    let docs_with_sdk_dependencies =
        plan.new_documents.iter().filter(|d| !d.provenance.sdk_dependencies.is_empty()).count();
```

…emitted as `"docs_with_language_targets"` / `"docs_with_sdk_dependencies"` keys.

- [ ] **Step 4: Run** — `cargo test -p mn-cli && cargo clippy -p mn-cli --all-targets -- -D warnings` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-cli crates/mn-content
git commit -m "feat(mn-cli): wire version extraction into ingest with report counts"
```

---

### Task 11: Client surfaces — MCP `version_match` + facets `within`; CLI `--version-match`

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs` (advanced_search input schema :271-289, parse fn ~:1114, facets tool schema ~:133-155), `crates/mn-mcp/src/cloud_client.rs:42-82` (SearchRequest), `crates/mn-cli/src/commands/search.rs` (flag + request field), `specs/001-rag-platform/contracts/mcp-tools.json` (regenerated)

- [ ] **Step 1: Failing test.** In mn-mcp, find the existing advanced_search arg-parsing tests (`grep -n "parse_advanced_search_args" crates/mn-mcp/src/tools.rs | head`) and add:

```rust
#[test]
fn version_match_parsed_and_forwarded() {
    let args = serde_json::json!({
        "queries": ["q"],
        "version_match": "strict",
        "filters": { "language_target": { "any_of": [{ "name": "compact", "version_satisfies": ">=0.23" }] } }
    });
    let parsed = parse_advanced_search_args(&args).unwrap();
    assert_eq!(parsed.version_match.as_deref(), Some("strict"));
    // range syntax now validates (was a 400 under concrete-only semantics)
}
```

- [ ] **Step 2: Run to verify failure**, then implement:

1. `advanced_search_input_schema()` gains (next to `rerank`):

```json
"version_match": { "type": "string", "enum": ["strict", "permissive"], "default": "permissive",
  "description": "Version-filter semantics: permissive (default) biases ranking and drops only breaking mismatches among version-declaring content; strict hard-filters to satisfying content only." }
```

2. The parsed-args struct (where `rerank_instructions: Option<String>` lives, ~:1005) gains `pub version_match: Option<String>`; `parse_advanced_search_args` reads it (validating against the two allowed values, error message naming them); `build_search_request` (~:823) forwards it.
3. `cloud_client.rs` `SearchRequest` gains:

```rust
    /// Version-matching mode (`strict` | `permissive`); omitted = server default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_match: Option<String>,
```

(compiler flags every construction site — fill `None`/the parsed value.)
4. Facets tool: the `facet` enum gains `"language_target"` and `"sdk_dependency"`; add a `"within": { "type": "string" }` param with description `"Second drill level: enumerate version values within one name (language_target/sdk_dependency) or one package name."`; forward it on the cloud call (mirror how `cursor` is forwarded — read the facets tool fn first).
5. CLI: `mnm search` gains a flag next to `--mode` (grep `arg(long, value_enum)` in `commands/search.rs`):

```rust
    /// Version-filter semantics: permissive (default) biases ranking; strict
    /// hard-filters. Only meaningful with a version-bearing --filter-json.
    #[arg(long, value_parser = ["strict", "permissive"])]
    pub version_match: Option<String>,
```

The CLI `SearchRequest` (:1235-1273) gains the same `Option<String>` field with `skip_serializing_if`; request assembly copies the flag through.

- [ ] **Step 3: Regenerate the MCP contract**

Run: `REGENERATE_CONTRACT=1 cargo test -p mn-mcp mcp_tools_json_mirrors_tools_list && cargo test -p mn-mcp`
Expected: contract file rewritten; all tests pass. `git diff specs/001-rag-platform/contracts/mcp-tools.json` shows only the new fields.

- [ ] **Step 4: Run full client tests** — `VOYAGE_API_KEY= cargo test -p mn-mcp -p mn-cli` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-mcp crates/mn-cli specs/001-rag-platform/contracts/mcp-tools.json
git commit -m "feat(clients): version_match on advanced_search/mnm search; facets within param"
```

---

### Task 12: Skill rewrite + drift guard

**Files:**
- Modify: `crates/mn-skills/assets/midnight-advanced-search/SKILL.md` (§"Match the user's version & freshness" ~:177-207, filter ladder, trust-weighted selection :245-247), `references/filters-and-modes.md` (:80-96 catalog rows, :175-209 sharp edges + CLI mapping), `references/advanced-techniques.md` (:25-64 technique B), `references/rerank-instructions.md` (:56-70)
- Create: `crates/mn-skills/tests/version_examples_validate.rs`
- Modify: `crates/mn-skills/Cargo.toml` (dev-deps: `mn-retrieval`, `mn-core`, `regex` if not present — check `grep -n regex Cargo.toml`)

- [ ] **Step 1: Write the drift-guard test first**

```rust
//! Every `version_satisfies` example value shipped in the skill must be
//! accepted by the real request parser (the guard the 2026-06 range-syntax
//! doc bug lacked).

use std::path::Path;

#[test]
fn skill_version_satisfies_examples_parse() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/midnight-advanced-search");
    let mut checked = 0usize;
    let re = regex::Regex::new(r#""version_satisfies"\s*:\s*"([^"]+)""#).unwrap();
    for entry in walkdir::WalkDir::new(&assets).into_iter().filter_map(Result::ok) {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let body = std::fs::read_to_string(entry.path()).unwrap();
        for cap in re.captures_iter(&body) {
            let value = &cap[1];
            assert!(
                mn_core::version_match::parse_request(value).is_some(),
                "{}: `{value}` is not a valid version/range",
                entry.path().display()
            );
            checked += 1;
        }
    }
    assert!(checked >= 4, "expected version_satisfies examples in the skill (found {checked})");
}
```

(`walkdir` is already used elsewhere in the workspace — `grep -n walkdir Cargo.toml`; add `regex`/`walkdir` to mn-skills dev-deps as needed.)

- [ ] **Step 2: Run** — passes already (the existing range examples now parse!). It pins the contract for future edits. If `regex` adds dependency weight concerns, hand-roll the scan with `split("version_satisfies")` — keep the assertion the same.

- [ ] **Step 3: Rewrite the skill content.** Required edits (keep each file's existing heading style and the facet-key backticks the `catalog_documents_every_facet_key` test scans for):

1. **filters-and-modes.md catalog rows** — `version_satisfies` column text becomes: `a concrete version (e.g. "0.31") or a semver range (e.g. ">=0.23"); matched against the target's declared constraint`. Sharp-edges section replaces the "cannot be negated"-adjacent semver sentence with the **two modes**: permissive default (bias; breaking drops; version-silent content unaffected), `version_match: "strict"` for hard pinning. Document the new `within` drill levels under the facets section.
2. **SKILL.md "Match the user's version & freshness"** — rewrite around the two-regime model:
   - Version filters are safe on any search by default (permissive biases; only breaking mismatches among declaring content drop).
   - For exact pinning (and typically with `code_mode`), add `"version_match": "strict"`.
   - Prose (tutorials/guides) rarely declares targets: put the version in the query text and use `ingested_at`/`source_modified_at` floors + `"deprecated": false`.
   - **Support-matrix playbook** (new ~3 lines): for compatibility questions ("what SDK works with node X?"), first retrieve the support matrix page (`source_slug: midnight-docs`, query "support matrix"), derive the concrete versions, then issue version-pinned follow-ups.
   - Recovery ladder gains a rung: zero results under `strict` → retry permissive → drop the version filter.
   - Discovery: "confirm corpus version coverage via `facets` with `facet=language_target` then `within=<name>`" (now true).
3. **advanced-techniques.md technique B** — keep the worked example (its range syntax is now valid); add a strict-mode variant line and the new recovery rung.
4. **rerank-instructions.md** — note the derived default now comes from the first version-bearing `language_target` element; the "re-state the version preference when overriding" guidance stands.

- [ ] **Step 4: Run the skill drift guards** — `cargo test -p mn-skills` → PASS (catalog coverage + the new example guard).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-skills
git commit -m "docs(mn-skills): two-regime version guidance + matrix playbook + example drift guard"
```

---

### Task 13: Contracts & docs — openapi, README, cookbooks, CLAUDE.md

**Files:**
- Modify: `specs/001-rag-platform/contracts/openapi.yaml` (:1108-1120 region for `version_match`; :1330/:1340 `version_satisfies` descriptions; ConfidenceFactors :1492-1508), `README.md` (:733 ranking bullet), `docs/cookbook/query-enhancement.md` (:193-197), `docs/cookbook/ingesting-content.md`, `CLAUDE.md` (Recent Changes)

- [ ] **Step 1: openapi.yaml** (best-effort, unenforced — note that in the commit message):
  - `SearchRequest` gains `version_match: { type: string, enum: [strict, permissive], default: permissive }` with the two-sentence semantics description.
  - Both `version_satisfies` descriptions (:1330, :1340) become: `Concrete version (e.g. "0.31") or semver range (e.g. ">=0.23"), matched against the document's declared constraint. Unparseable values are a 400.`
  - `ConfidenceFactors` gains `version_match_class: { type: string, enum: [satisfies, near_miss, silent, unknown] }` and `version_distance: { type: integer }` (both optional).
  - `search_metadata` gains `version_match`.
- [ ] **Step 2: README.md** — the version-match ranking bullet (:733) updates to the boost/penalty/drop triple: satisfying content boosted, near-miss penalized by distance, breaking mismatches excluded; strict mode hard-filters. One sentence on extraction ("version targets are extracted from Compact pragmas and package manifests at ingest").
- [ ] **Step 3: Cookbooks** — query-enhancement.md's version paragraph (:193-197) gains a worked permissive + strict example pair (concrete `"0.31"` and range `">=0.23"`); ingesting-content.md gains an **authoring guideline** subsection: "State your toolchain versions in the first paragraph of tutorials (e.g. 'This tutorial targets Compact 0.31 and midnight-js 2.x') — contextualized embeddings spread that statement across all chunks, and prose carries no machine-extractable version metadata."
- [ ] **Step 4: CLAUDE.md** Recent Changes (top of list):

```markdown
- 2026-06-XX — Version provenance & matching: per-document extraction at ingest
  (Compact `language_version` pragma → `language_targets`, allowlisted
  @midnight-ntwrk/midnight-* deps → `sdk_dependencies`, `package.version`
  populated); `version_satisfies` accepts concrete or range; `version_match`
  strict|permissive (permissive default — boost/scaled-penalty/breaking-drop,
  0.x role shift); `/v1/facets` two-level drill for version facets; search
  skill rewritten (two-regime guidance + support-matrix playbook). Re-ingest
  required to populate provenance.
```

(use the actual date)
- [ ] **Step 5: Commit**

```bash
git add specs/001-rag-platform/contracts/openapi.yaml README.md docs CLAUDE.md
git commit -m "docs: version-provenance contract + README/cookbook/CLAUDE.md updates (openapi best-effort)"
```

---

### Task 14: Recall probe doc + final verification

**Files:**
- Create: `docs/cookbook/version-recall-probe.md`

- [ ] **Step 1: Write the probe doc** — a manual experiment (real embeddings; not CI):

```markdown
# Version-recall probe (manual)

Checks whether contextualized embeddings actually discriminate version-qualified
queries — gates the skill's wording about semantic version matching (spec §7).

## Setup
1. Ingest a probe source with paired docs: identical tutorial bodies whose first
   paragraph states "This tutorial targets Compact 0.23" vs "... Compact 0.31"
   (plus one no-statement control). `mnm ingest run --source-slug version-probe ...`
2. Queries: "how to declare a ledger in compact 0.31", "... in compact 0.23",
   and the unqualified "how to declare a ledger in compact".

## Measure
For each query × mode (hybrid, vector, fts): note the rank order of the three
docs (`mnm search --json | jq '.results[].source_path'`).

## Interpretation
- Version-stated docs ranking above control for matching queries in **fts** but
  not **vector** ⇒ keep the skill's "put the version in your query text" wording
  (FTS-driven), do NOT claim semantic version matching.
- Discrimination in vector mode too ⇒ the skill may state contextualized
  embeddings carry version context.
Record the outcome here with the date and corpus model.
```

- [ ] **Step 2: Full CI-surface check** (per the project rule — package builds miss test targets and feature-gated files):

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p mn-server --no-run --features integration
```

Expected: all green (integration executes in CI). Fix anything flagged.

- [ ] **Step 3: Spec coverage re-read** — open `docs/superpowers/specs/2026-06-12-version-provenance-design.md` and confirm: §1 → Tasks 7-10, §2 → Task 1+4, §3 → Tasks 2+3+5, §4 → Task 6, §5 → Tasks 12-13, §6 → Tasks 4+5+8 error paths, §7 → tests throughout + Task 14, §9 → CLAUDE.md note. Fix anything missed before declaring done.

- [ ] **Step 4: Commit stragglers; do NOT push** — integration tests run in CI on the PR.

```bash
git add -A && git commit -m "docs: version-recall probe + final checks"
```

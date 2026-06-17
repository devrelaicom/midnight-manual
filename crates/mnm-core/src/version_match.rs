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

/// Parse a request-side `version_satisfies` string into a [`RequestedVersion`].
///
/// Concrete first: bare versions stay concrete (spec decision 5), else a semver
/// range desugared to an interval. `None` = neither form parses, or the range
/// is empty.
#[must_use]
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

/// Classify a parsed request against a declared constraint string.
/// `declared = None` (a matching-name target with no constraint) ⇒ Satisfies.
#[must_use]
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

/// Rank for "best class wins" across elements: lower is better.
#[must_use]
pub const fn class_rank(c: &MatchClass) -> (u8, u32) {
    match c {
        MatchClass::Satisfies => (0, 0),
        MatchClass::NearMissPatch(d) => (1, *d),
        MatchClass::NearMissMinor(d) => (2, *d),
        MatchClass::Unknown => (3, 0),
        MatchClass::Breaking => (4, 0),
    }
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
        Self {
            lo: Some(Bound {
                version: v.clone(),
                inclusive: true,
            }),
            hi: Some(Bound { version: v, inclusive: true }),
        }
    }

    /// Desugar a `VersionReq` (conjunction of comparators) to an interval.
    /// `None` when a comparator op is unsupported (future semver ops).
    #[must_use]
    pub fn from_req(req: &VersionReq) -> Option<Self> {
        let mut iv = Self::unbounded();
        for c in &req.comparators {
            iv = iv.intersect(&comparator_interval(c)?);
        }
        Some(iv)
    }

    /// Whether `v` lies inside the interval.
    #[must_use]
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

    /// Whether two intervals share at least one version.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        !self.intersect(other).is_empty()
    }

    /// Whether the interval contains no versions (contradictory comparators).
    #[must_use]
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
            (Some(a), Some(b)) => {
                Some(if (b.version.clone(), !b.inclusive) > (a.version.clone(), !a.inclusive) {
                    b.clone()
                } else {
                    a.clone()
                })
            }
            (Some(a), None) => Some(a.clone()),
            (None, b) => b.clone(),
        };
        let hi = match (&self.hi, &other.hi) {
            (Some(a), Some(b)) => {
                Some(if (b.version.clone(), b.inclusive) < (a.version.clone(), a.inclusive) {
                    b.clone()
                } else {
                    a.clone()
                })
            }
            (Some(a), None) => Some(a.clone()),
            (None, b) => b.clone(),
        };
        Self { lo, hi }
    }
}

/// Desugar one comparator per cargo rules. `None` for ops this version of the
/// `semver` crate may add in future (`Op` is `non_exhaustive`).
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
            (Some(j), None) => VersionInterval {
                lo: lo_incl(base),
                hi: hi_excl(next_minor(j)),
            },
            (None, _) => VersionInterval {
                lo: lo_incl(base),
                hi: hi_excl(next_major),
            },
        },
        Op::Greater => match (min, pat) {
            (Some(_), Some(_)) => VersionInterval {
                lo: Some(Bound {
                    version: base,
                    inclusive: false,
                }),
                hi: None,
            },
            (Some(j), None) => VersionInterval {
                lo: lo_incl(next_minor(j)),
                hi: None,
            },
            (None, _) => VersionInterval {
                lo: lo_incl(next_major),
                hi: None,
            },
        },
        Op::GreaterEq => VersionInterval { lo: lo_incl(base), hi: None },
        Op::Less => VersionInterval { lo: None, hi: hi_excl(base) },
        Op::LessEq => match (min, pat) {
            (Some(_), Some(_)) => VersionInterval {
                lo: None,
                hi: Some(Bound { version: base, inclusive: true }),
            },
            (Some(j), None) => VersionInterval {
                lo: None,
                hi: hi_excl(next_minor(j)),
            },
            (None, _) => VersionInterval {
                lo: None,
                hi: hi_excl(next_major),
            },
        },
        // `~` and `*`/`.x` desugar identically for the partial forms we accept.
        Op::Tilde | Op::Wildcard => match (min, pat) {
            (Some(j), _) => VersionInterval {
                lo: lo_incl(base),
                hi: hi_excl(next_minor(j)),
            },
            (None, _) => VersionInterval {
                lo: lo_incl(base),
                hi: hi_excl(next_major),
            },
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
            VersionInterval {
                lo: lo_incl(base),
                hi: hi_excl(hi),
            }
        }
        _ => return None,
    })
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
        assert_eq!(classify(&req("1.6.0"), Some(">=1.4, <1.5")), MatchClass::NearMissMinor(2));
        // patch-level miss: declared <1.4.3 covers ≤1.4.2, so the nearest member
        // is 1.4.2; request 1.4.5 → patch distance 3 (same stepping rule as the
        // 0.23.9 vs <0.23.5 case above: exclusive ceiling steps inward first).
        assert_eq!(classify(&req("1.4.5"), Some(">=1.4.0, <1.4.3")), MatchClass::NearMissPatch(3));
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
        assert_eq!(classify(&req("1.5.0"), Some(">=1.4.0, <1.5.0")), MatchClass::NearMissMinor(1));
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

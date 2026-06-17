//! Scoring-policy TOML loader (Phase-2 stub; full validation lands in Phase 9 / US6).
//!
//! Per spec §"Scoring policy TOML schema": loaded once at server startup from
//! `MIDNIGHT_MANUAL_SCORING_POLICY`. If the env is unset the compiled-in
//! defaults below are used. Invalid TOML fails the load (Constitution VI fail-fast).
//!
//! Phase 9 wires this into [`crate::types::Chunk`] scoring; until then it sits
//! here so callers can already type their config and unit-test the defaults.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Canonical schema version for scoring-policy TOML.
pub const SCHEMA_VERSION: u32 = 1;

/// Full scoring-policy shape — every section is independently overridable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoringPolicy {
    /// Schema sentinel. Always `1` in v1.
    pub schema_version: u32,
    /// Attribution-based trust multipliers.
    pub attribution: AttributionMultipliers,
    /// Verification-based trust multipliers.
    pub verification: VerificationMultipliers,
    /// Freshness decay parameters.
    pub freshness: FreshnessParams,
    /// Deprecation penalty.
    pub deprecation: DeprecationParams,
    /// Version-target match multipliers.
    pub version_match: VersionMatchMultipliers,
    /// Trust × relevance blend weights.
    pub blend: BlendWeights,
}

/// `[attribution]` multipliers (highest trust to lowest).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttributionMultipliers {
    /// Foundation-attributed content.
    pub foundation: f64,
    /// Partner-attributed content.
    pub partner: f64,
    /// Third-party-attributed content.
    pub third_party: f64,
    /// Community-attributed content.
    pub community: f64,
    /// Unknown attribution.
    pub unknown: f64,
}

/// `[verification]` — multipliers based on who verified the content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationMultipliers {
    /// Verified by the Midnight Foundation.
    pub verified_by_foundation: f64,
    /// Verified by a partner.
    pub verified_by_partner: f64,
    /// Verified by any other principal.
    pub verified_by_other: f64,
    /// Unverified content.
    pub unverified: f64,
}

/// `[freshness]` — exponential-decay model parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreshnessParams {
    /// Half-life in days (default 180).
    pub half_life_days: f64,
    /// Which timestamp to use when `document.source_modified_at` is null.
    /// `"ingested_at"` falls back to the `source_version.ingested_at` row.
    pub fallback_age_source: String,
}

/// `[deprecation]` — penalty when `provenance.deprecation.is_deprecated = true`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeprecationParams {
    /// Multiplier (default 0.30 → -70%).
    pub penalty_multiplier: f64,
}

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

/// `[blend]` — exponents in the geometric-mean blend `trust^w_t * relevance^w_r`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlendWeights {
    /// Trust-side exponent (default 0.55).
    pub trust_weight: f64,
    /// Relevance-side exponent (default 0.45).
    pub relevance_weight: f64,
}

impl Default for ScoringPolicy {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            attribution: AttributionMultipliers {
                foundation: 1.00,
                partner: 0.85,
                third_party: 0.60,
                community: 0.40,
                unknown: 0.30,
            },
            verification: VerificationMultipliers {
                verified_by_foundation: 1.00,
                verified_by_partner: 0.90,
                verified_by_other: 0.80,
                unverified: 0.70,
            },
            freshness: FreshnessParams {
                half_life_days: 180.0,
                fallback_age_source: "ingested_at".into(),
            },
            deprecation: DeprecationParams { penalty_multiplier: 0.30 },
            version_match: VersionMatchMultipliers {
                satisfies: 1.15,
                neutral: 1.00,
                floor: 0.30,
                patch_step: 0.05,
                minor_step: 0.15,
            },
            blend: BlendWeights {
                trust_weight: 0.55,
                relevance_weight: 0.45,
            },
        }
    }
}

impl ScoringPolicy {
    /// Parse a scoring-policy TOML body.
    ///
    /// # Errors
    ///
    /// Returns [`ScoringPolicyError::Parse`] if the TOML is malformed,
    /// [`ScoringPolicyError::SchemaVersionMismatch`] if the schema sentinel
    /// disagrees, or [`ScoringPolicyError::NonFiniteWeight`] if any numeric
    /// weight is not a finite, non-negative `f64`.
    pub fn parse(body: &str) -> Result<Self, ScoringPolicyError> {
        let policy: Self =
            toml::from_str(body).map_err(|e| ScoringPolicyError::Parse(e.to_string()))?;
        if policy.schema_version != SCHEMA_VERSION {
            return Err(ScoringPolicyError::SchemaVersionMismatch {
                found: policy.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        policy.validate_finite()?;
        Ok(policy)
    }

    fn validate_finite(&self) -> Result<(), ScoringPolicyError> {
        let weights: [(&str, f64); 18] = [
            ("attribution.foundation", self.attribution.foundation),
            ("attribution.partner", self.attribution.partner),
            ("attribution.third_party", self.attribution.third_party),
            ("attribution.community", self.attribution.community),
            ("attribution.unknown", self.attribution.unknown),
            ("verification.verified_by_foundation", self.verification.verified_by_foundation),
            ("verification.verified_by_partner", self.verification.verified_by_partner),
            ("verification.verified_by_other", self.verification.verified_by_other),
            ("verification.unverified", self.verification.unverified),
            ("freshness.half_life_days", self.freshness.half_life_days),
            ("deprecation.penalty_multiplier", self.deprecation.penalty_multiplier),
            ("version_match.satisfies", self.version_match.satisfies),
            ("version_match.neutral", self.version_match.neutral),
            ("version_match.floor", self.version_match.floor),
            ("version_match.patch_step", self.version_match.patch_step),
            ("version_match.minor_step", self.version_match.minor_step),
            ("blend.trust_weight", self.blend.trust_weight),
            ("blend.relevance_weight", self.blend.relevance_weight),
        ];
        for (name, w) in weights {
            if !w.is_finite() || w < 0.0 {
                return Err(ScoringPolicyError::NonFiniteWeight {
                    field: name.to_owned(),
                    value: w,
                });
            }
        }
        if self.freshness.half_life_days <= 0.0 {
            return Err(ScoringPolicyError::NonFiniteWeight {
                field: "freshness.half_life_days".into(),
                value: self.freshness.half_life_days,
            });
        }
        Ok(())
    }
}

/// All the ways scoring-policy parsing can fail.
#[derive(Debug, Error)]
pub enum ScoringPolicyError {
    /// TOML body did not parse against the [`ScoringPolicy`] shape.
    #[error("failed to parse scoring policy: {0}")]
    Parse(String),
    /// `schema_version` did not match [`SCHEMA_VERSION`].
    #[error("scoring policy schema_version={found}; expected {expected}")]
    SchemaVersionMismatch {
        /// The schema version we found.
        found: u32,
        /// The version we expected.
        expected: u32,
    },
    /// A numeric weight was non-finite or negative.
    #[error("scoring policy field `{field}` has non-finite or negative value {value}")]
    NonFiniteWeight {
        /// Dotted-path name of the offending field.
        field: String,
        /// The offending value.
        value: f64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_well_formed() {
        let p = ScoringPolicy::default();
        assert_eq!(p.schema_version, 1);
        assert!((p.blend.trust_weight + p.blend.relevance_weight - 1.0).abs() < 1e-9);
        assert!(p.attribution.foundation > p.attribution.community);
    }

    #[test]
    fn round_trips_through_toml() {
        let body = toml::to_string(&ScoringPolicy::default()).unwrap();
        let back = ScoringPolicy::parse(&body).unwrap();
        assert_eq!(back, ScoringPolicy::default());
    }

    #[test]
    fn rejects_schema_mismatch() {
        let p = ScoringPolicy {
            schema_version: 99,
            ..ScoringPolicy::default()
        };
        let body = toml::to_string(&p).unwrap();
        let err = ScoringPolicy::parse(&body).unwrap_err();
        assert!(matches!(
            err,
            ScoringPolicyError::SchemaVersionMismatch { found: 99, expected: 1 }
        ));
    }

    #[test]
    fn rejects_negative_weight() {
        let p = ScoringPolicy {
            attribution: AttributionMultipliers {
                foundation: -1.0,
                ..ScoringPolicy::default().attribution
            },
            ..ScoringPolicy::default()
        };
        let body = toml::to_string(&p).unwrap();
        let err = ScoringPolicy::parse(&body).unwrap_err();
        assert!(matches!(err, ScoringPolicyError::NonFiniteWeight { .. }));
    }

    #[test]
    fn rejects_unknown_key() {
        // Acceptance #11: unknown keys fail the load (fail-fast, Constitution VIII).
        let mut body = toml::to_string(&ScoringPolicy::default()).unwrap();
        body.push_str("\nbogus_top_level_key = 42\n");
        let err = ScoringPolicy::parse(&body).unwrap_err();
        assert!(matches!(err, ScoringPolicyError::Parse(_)));
    }

    #[test]
    fn rejects_negative_neutral_or_floor() {
        for mutate in [
            |p: &mut ScoringPolicy| p.version_match.neutral = -0.1,
            |p: &mut ScoringPolicy| p.version_match.floor = -0.1,
        ] {
            let mut p = ScoringPolicy::default();
            mutate(&mut p);
            assert!(p.validate_finite().is_err());
        }
    }

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
        // loudly at startup. Inject the stale key INTO the existing
        // `[version_match]` table so the failure is raised by
        // `deny_unknown_fields` (the real guard) rather than an incidental
        // duplicate-table-header error.
        let body = toml::to_string(&ScoringPolicy::default())
            .unwrap()
            .replace("[version_match]", "[version_match]\nunsatisfied = 0.7");
        let err = ScoringPolicy::parse(&body).unwrap_err();
        assert!(
            matches!(err, ScoringPolicyError::Parse(_)),
            "stale `unsatisfied` key must fail loudly: {err:?}"
        );
    }

    #[test]
    fn rejects_zero_half_life() {
        let p = ScoringPolicy {
            freshness: FreshnessParams {
                half_life_days: 0.0,
                ..ScoringPolicy::default().freshness
            },
            ..ScoringPolicy::default()
        };
        let body = toml::to_string(&p).unwrap();
        let err = ScoringPolicy::parse(&body).unwrap_err();
        assert!(matches!(err, ScoringPolicyError::NonFiniteWeight { .. }));
    }
}

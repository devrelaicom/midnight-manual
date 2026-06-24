//! Server-side injection-scoring policy TOML loader.
//!
//! Mirrors [`crate::scoring_policy`] in style: a schema-versioned, fail-fast
//! TOML shape with `deny_unknown_fields`, finite/range validation, and a
//! thiserror error enum. Loaded once at server startup; invalid TOML fails the
//! load (Constitution fail-fast). The policy controls how the pattern score and
//! the optional model score are blended into a single reject decision, and which
//! source attributions trigger the (more expensive) model pass.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Canonical schema version for injection-policy TOML.
pub const SCHEMA_VERSION: u32 = 1;

/// What to do when the model scorer is unavailable or errors.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum FailMode {
    /// Admit the content when the model pass cannot run (availability-first).
    #[default]
    Open,
    /// Reject the content when the model pass cannot run (security-first).
    Closed,
}

/// Full injection-policy shape — every knob is independently overridable.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct InjectionPolicy {
    /// Schema sentinel. Always `1` in v1.
    pub schema_version: u32,
    /// Blended-score threshold at or above which content is rejected.
    pub reject_threshold: f64,
    /// Weight of the pattern score in the blend.
    pub pattern_weight: f64,
    /// Weight of the model score in the blend.
    pub model_weight: f64,
    /// Model score at or above which the model considers content injection.
    pub model_threshold: f64,
    /// Source attributions that trigger the model pass (e.g. untrusted tiers).
    pub gate_attributions: Vec<String>,
    /// Behaviour when the model pass cannot run.
    pub fail_mode: FailMode,
}

impl Default for InjectionPolicy {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            reject_threshold: 0.85,
            pattern_weight: 0.5,
            model_weight: 0.5,
            model_threshold: 0.5,
            gate_attributions: vec![
                "third_party".to_owned(),
                "community".to_owned(),
                "unknown".to_owned(),
            ],
            fail_mode: FailMode::Open,
        }
    }
}

impl InjectionPolicy {
    /// Parse an injection-policy TOML body.
    ///
    /// # Errors
    ///
    /// Returns [`InjectionPolicyError::Parse`] if the TOML is malformed or
    /// carries unknown keys, [`InjectionPolicyError::SchemaVersionMismatch`] if
    /// the schema sentinel disagrees, or [`InjectionPolicyError::InvalidWeight`]
    /// if any weight is non-finite/negative or a threshold falls outside
    /// `0.0..=1.0`.
    pub fn parse(body: &str) -> Result<Self, InjectionPolicyError> {
        let policy: Self =
            toml::from_str(body).map_err(|e| InjectionPolicyError::Parse(e.to_string()))?;
        if policy.schema_version != SCHEMA_VERSION {
            return Err(InjectionPolicyError::SchemaVersionMismatch {
                found: policy.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), InjectionPolicyError> {
        let weights: [(&str, f64); 4] = [
            ("reject_threshold", self.reject_threshold),
            ("pattern_weight", self.pattern_weight),
            ("model_weight", self.model_weight),
            ("model_threshold", self.model_threshold),
        ];
        for (field, value) in weights {
            if !value.is_finite() || value < 0.0 {
                return Err(InjectionPolicyError::InvalidWeight { field: field.to_owned(), value });
            }
        }
        for (field, value) in [
            ("reject_threshold", self.reject_threshold),
            ("model_threshold", self.model_threshold),
        ] {
            if value > 1.0 {
                return Err(InjectionPolicyError::InvalidWeight { field: field.to_owned(), value });
            }
        }
        Ok(())
    }

    /// Blend the pattern score with an optional model score into `0.0..=1.0`.
    ///
    /// With no model score, the clamped pattern score is returned. Otherwise the
    /// weighted mean `(pw·pattern + mw·model) / (pw + mw)` is returned, clamped.
    /// If both weights are zero (degenerate config), the pattern score is used.
    #[must_use]
    pub fn blend(&self, pattern: f64, model: Option<f64>) -> f64 {
        let Some(model) = model else {
            return pattern.clamp(0.0, 1.0);
        };
        let denom = self.pattern_weight + self.model_weight;
        if denom <= 0.0 {
            return pattern.clamp(0.0, 1.0);
        }
        let weighted = self
            .pattern_weight
            .mul_add(pattern, self.model_weight * model);
        (weighted / denom).clamp(0.0, 1.0)
    }

    /// Whether the (more expensive) model pass should run for `attribution`.
    #[must_use]
    pub fn model_gated_for(&self, attribution: &str) -> bool {
        self.gate_attributions.iter().any(|a| a == attribution)
    }
}

/// All the ways injection-policy parsing can fail.
#[derive(Debug, Error)]
pub enum InjectionPolicyError {
    /// TOML body did not parse against the [`InjectionPolicy`] shape.
    #[error("failed to parse injection policy: {0}")]
    Parse(String),
    /// `schema_version` did not match [`SCHEMA_VERSION`].
    #[error("injection policy schema_version={found}; expected {expected}")]
    SchemaVersionMismatch {
        /// The schema version we found.
        found: u32,
        /// The version we expected.
        expected: u32,
    },
    /// A weight/threshold was non-finite, negative, or out of `0.0..=1.0`.
    #[error("injection policy field `{field}` has invalid value {value}")]
    InvalidWeight {
        /// Name of the offending field.
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
        let p = InjectionPolicy::default();
        assert_eq!(p.schema_version, 1);
        assert!((p.reject_threshold - 0.85).abs() < 1e-12);
        assert!((p.pattern_weight - 0.5).abs() < 1e-12);
        assert!((p.model_weight - 0.5).abs() < 1e-12);
        assert!((p.model_threshold - 0.5).abs() < 1e-12);
        assert_eq!(p.fail_mode, FailMode::Open);
        assert!(p.model_gated_for("unknown"));
        assert!(!p.model_gated_for("foundation"));
    }

    #[test]
    fn round_trips_through_toml() {
        let body = toml::to_string(&InjectionPolicy::default()).unwrap();
        let back = InjectionPolicy::parse(&body).unwrap();
        assert_eq!(back, InjectionPolicy::default());
    }

    #[test]
    fn rejects_schema_mismatch() {
        let p = InjectionPolicy {
            schema_version: 99,
            ..InjectionPolicy::default()
        };
        let body = toml::to_string(&p).unwrap();
        let err = InjectionPolicy::parse(&body).unwrap_err();
        assert!(matches!(
            err,
            InjectionPolicyError::SchemaVersionMismatch { found: 99, expected: 1 }
        ));
    }

    #[test]
    fn rejects_unknown_key() {
        let mut body = toml::to_string(&InjectionPolicy::default()).unwrap();
        body.push_str("\nbogus_top_level_key = 42\n");
        let err = InjectionPolicy::parse(&body).unwrap_err();
        assert!(matches!(err, InjectionPolicyError::Parse(_)));
    }

    #[test]
    fn rejects_negative_weight() {
        let p = InjectionPolicy {
            pattern_weight: -1.0,
            ..InjectionPolicy::default()
        };
        let body = toml::to_string(&p).unwrap();
        let err = InjectionPolicy::parse(&body).unwrap_err();
        assert!(matches!(
            err,
            InjectionPolicyError::InvalidWeight { ref field, .. } if field == "pattern_weight"
        ));
    }

    #[test]
    fn rejects_threshold_above_one() {
        let p = InjectionPolicy {
            reject_threshold: 1.5,
            ..InjectionPolicy::default()
        };
        let body = toml::to_string(&p).unwrap();
        let err = InjectionPolicy::parse(&body).unwrap_err();
        assert!(matches!(
            err,
            InjectionPolicyError::InvalidWeight { ref field, .. } if field == "reject_threshold"
        ));
    }

    #[test]
    fn blend_without_model_returns_pattern() {
        let p = InjectionPolicy::default();
        assert!((p.blend(0.7, None) - 0.7).abs() < 1e-12);
        // Clamped.
        assert!((p.blend(1.5, None) - 1.0).abs() < 1e-12);
        assert!((p.blend(-0.2, None) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn blend_with_model_is_weighted_mean() {
        let p = InjectionPolicy::default(); // 0.5 / 0.5
        assert!((p.blend(0.8, Some(0.4)) - 0.6).abs() < 1e-12);

        let skewed = InjectionPolicy {
            pattern_weight: 0.75,
            model_weight: 0.25,
            ..InjectionPolicy::default()
        };
        // (0.75*0.8 + 0.25*0.4) / 1.0 = 0.7
        assert!((skewed.blend(0.8, Some(0.4)) - 0.7).abs() < 1e-12);
    }

    #[test]
    fn blend_with_zero_weights_falls_back_to_pattern() {
        let p = InjectionPolicy {
            pattern_weight: 0.0,
            model_weight: 0.0,
            ..InjectionPolicy::default()
        };
        assert!((p.blend(0.42, Some(0.99)) - 0.42).abs() < 1e-12);
    }

    #[test]
    fn gating_matches_default_set() {
        let p = InjectionPolicy::default();
        for a in ["third_party", "community", "unknown"] {
            assert!(p.model_gated_for(a), "{a} should gate");
        }
        for a in ["foundation", "partner"] {
            assert!(!p.model_gated_for(a), "{a} should not gate");
        }
    }
}

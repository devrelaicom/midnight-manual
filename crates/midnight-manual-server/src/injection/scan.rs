//! Ingest-time injection scan orchestration (issue #103).
//!
//! [`InjectionState`] holds the resolved [`InjectionPolicy`] and the optional
//! hosted-model client, and runs the two-leg scan: the pure pattern detector
//! (always) plus the model leg (only for gated source attributions, when the
//! model is configured). [`assemble_report`] is the pure blend+verdict step,
//! kept separate so it is unit-testable without any network I/O.

use std::sync::Arc;

use mnm_core::injection::{
    detect, FailMode, InjectionPolicy, ModelReport, PatternResult, ScanReport, Verdict,
};
use mnm_core::provenance::Attribution;

use crate::injection::model_client::HfClient;

/// Resolved injection-scanning state shared (read-only) across requests.
#[derive(Clone)]
pub struct InjectionState {
    /// Master switch — when `false`, [`InjectionState::scan`] runs the pattern
    /// leg only (it is cheap) and never reaches the model leg.
    pub enabled: bool,
    /// The blend/gate policy resolved at boot.
    pub policy: Arc<InjectionPolicy>,
    /// Hosted model-detector client, or `None` when the model leg is not
    /// configured (no endpoint/token) — pattern-only still runs.
    pub model: Option<Arc<HfClient>>,
}

impl InjectionState {
    /// Build the scanning state from server config.
    ///
    /// The model leg is constructed only when injection is enabled AND both the
    /// HF endpoint URL and token are present. A client build failure is logged
    /// and degrades to pattern-only (the leg becomes `None`).
    #[must_use]
    pub fn from_config(cfg: &crate::config::ServerConfig) -> Self {
        let policy = Arc::new(cfg.injection_policy.clone());
        let model = if cfg.injection_enabled
            && cfg.injection_hf_endpoint_url.is_some()
            && cfg.injection_hf_token.is_some()
        {
            // Both checked `is_some` above; safe to unwrap the refs.
            let endpoint = cfg.injection_hf_endpoint_url.as_deref().unwrap_or_default();
            let token = cfg.injection_hf_token.as_deref().unwrap_or_default();
            match HfClient::new(endpoint, token, cfg.injection_hf_model.clone()) {
                Ok(client) => Some(Arc::new(client)),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "injection model client build failed; running pattern-only"
                    );
                    None
                }
            }
        } else {
            if cfg.injection_enabled {
                tracing::warn!(
                    "injection enabled but HF endpoint/token missing; model leg disabled \
                     (pattern-only scanning still runs)"
                );
            }
            None
        };
        Self {
            enabled: cfg.injection_enabled,
            policy,
            model,
        }
    }

    /// Run the injection scan for one document's text.
    ///
    /// The pattern leg always runs. The model leg runs only when injection is
    /// enabled, a model client is configured, AND the policy gates this
    /// `attribution`. On a model error the [`FailMode`] decides: `Open` drops
    /// the model leg and proceeds (returning `model_unavailable = true`),
    /// `Closed` aborts the scan ([`ScanAbort`]).
    ///
    /// # Errors
    ///
    /// Returns [`ScanAbort`] only under [`FailMode::Closed`] when the model leg
    /// errored — the caller MUST refuse to ingest the document in that case.
    pub async fn scan(&self, content: &str, attribution: &str) -> Result<ScanResult, ScanAbort> {
        let pattern = detect(content);

        let run_model =
            self.enabled && self.model.is_some() && self.policy.model_gated_for(attribution);

        let (model_leg, model_unavailable) = if run_model {
            // `run_model` guaranteed `self.model.is_some()`.
            let model = self
                .model
                .as_ref()
                .expect("run_model implies model present");
            match model.score(content, self.policy.model_threshold).await {
                Ok(report) => (Some(report), false),
                Err(e) => match self.policy.fail_mode {
                    FailMode::Open => {
                        tracing::warn!(
                            error = %e,
                            "injection model leg failed; failing open (pattern-only)"
                        );
                        (None, true)
                    }
                    FailMode::Closed => return Err(ScanAbort),
                },
            }
        } else {
            (None, false)
        };

        let report = assemble_report(pattern, model_leg, &self.policy);
        Ok(ScanResult { report, model_unavailable })
    }
}

/// Outcome of a successful (non-aborted) scan.
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// The assembled report (legs, blend, verdict).
    pub report: ScanReport,
    /// `true` when the model leg was requested but unreachable and the policy
    /// failed open (the verdict reflects pattern-only scoring).
    pub model_unavailable: bool,
}

/// Marker that the scan aborted under [`FailMode::Closed`] because the model
/// leg was unreachable — the caller MUST refuse the document.
#[derive(Debug)]
pub struct ScanAbort;

/// Map an [`Attribution`] to its snake_case wire string (matching the serde
/// rename used by the policy's `gate_attributions`).
#[must_use]
pub const fn attribution_str(a: Attribution) -> &'static str {
    match a {
        Attribution::Foundation => "foundation",
        Attribution::Partner => "partner",
        Attribution::ThirdParty => "third_party",
        Attribution::Community => "community",
        Attribution::Unknown => "unknown",
    }
}

/// Assemble a [`ScanReport`] from the pattern leg and an optional model leg.
///
/// Pure: no network I/O. `model` is `Some` iff the model leg actually ran (even
/// when it reports `available = false` for a "requested but unavailable" case),
/// and `"model"` is listed in `detectors_run` accordingly. An unavailable model
/// contributes `None` to the blend (so the blend falls back to pattern-only).
#[must_use]
pub fn assemble_report(
    pattern: PatternResult,
    model: Option<ModelReport>,
    policy: &InjectionPolicy,
) -> ScanReport {
    let mut detectors_run = vec!["pattern".to_owned()];
    if model.is_some() {
        detectors_run.push("model".to_owned());
    }

    let model_score = model
        .as_ref()
        .and_then(|m| if m.available { Some(m.score) } else { None });
    let blended_score = policy.blend(pattern.score, model_score);

    let verdict = if blended_score >= policy.reject_threshold {
        Verdict::Reject
    } else {
        Verdict::Accept
    };

    ScanReport {
        detectors_run,
        pattern,
        model,
        blended_score,
        reject_threshold: policy.reject_threshold,
        verdict,
    }
}

#[cfg(test)]
mod tests {
    use super::{assemble_report, attribution_str};
    use mnm_core::injection::{InjectionPolicy, ModelReport, PatternResult, Verdict};
    use mnm_core::provenance::Attribution;

    const EPS: f64 = 1e-9;

    fn pattern_with(score: f64) -> PatternResult {
        PatternResult { matches: vec![], score }
    }

    #[test]
    fn attribution_str_maps_all_variants() {
        assert_eq!(attribution_str(Attribution::Foundation), "foundation");
        assert_eq!(attribution_str(Attribution::Partner), "partner");
        assert_eq!(attribution_str(Attribution::ThirdParty), "third_party");
        assert_eq!(attribution_str(Attribution::Community), "community");
        assert_eq!(attribution_str(Attribution::Unknown), "unknown");
    }

    #[test]
    fn assemble_pattern_only() {
        let policy = InjectionPolicy::default();
        let report = assemble_report(pattern_with(0.42), None, &policy);
        assert_eq!(report.detectors_run, vec!["pattern".to_owned()]);
        assert!((report.blended_score - 0.42).abs() < EPS);
        assert!(report.model.is_none());
    }

    #[test]
    fn assemble_pattern_plus_available_model_is_weighted_mean() {
        let policy = InjectionPolicy::default(); // 0.5 / 0.5
        let model = ModelReport {
            available: true,
            score: 0.4,
            flagged_windows: vec![],
        };
        let report = assemble_report(pattern_with(0.8), Some(model), &policy);
        assert!(report.detectors_run.iter().any(|d| d == "model"));
        // (0.5*0.8 + 0.5*0.4) / 1.0 = 0.6
        assert!((report.blended_score - 0.6).abs() < EPS, "got {}", report.blended_score);
    }

    #[test]
    fn verdict_accept_below_threshold() {
        let policy = InjectionPolicy::default(); // reject_threshold 0.85
        let report = assemble_report(pattern_with(0.84), None, &policy);
        assert_eq!(report.verdict, Verdict::Accept);
    }

    #[test]
    fn verdict_reject_at_or_above_threshold() {
        let policy = InjectionPolicy::default(); // reject_threshold 0.85
        let report = assemble_report(pattern_with(0.86), None, &policy);
        assert_eq!(report.verdict, Verdict::Reject);
    }

    #[test]
    fn unavailable_model_falls_back_to_pattern_but_lists_model() {
        let policy = InjectionPolicy::default();
        let model = ModelReport {
            available: false,
            score: 0.0,
            flagged_windows: vec![],
        };
        let report = assemble_report(pattern_with(0.7), Some(model), &policy);
        // model contributes None → blend == pattern score
        assert!((report.blended_score - 0.7).abs() < EPS);
        // but detectors_run still lists "model" (the leg ran)
        assert!(report.detectors_run.iter().any(|d| d == "model"));
    }
}

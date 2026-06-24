//! Shared detector-result wire types.
//!
//! These are pure data shapes returned by the server's ingest-time scan and its
//! `/v1/admin/injection/...` endpoints, and embedded in rejected-document
//! [`crate::ingest::UploadConflict`] detail. They live in `mnm-core` (not the
//! server) so the CLI can deserialize the admin/ingest responses against the
//! same contract the server serializes.

use serde::{Deserialize, Serialize};

use crate::injection::pattern::PatternResult;

/// One ≤512-token window the model detector flagged, with its malicious
/// probability and span (in original input bytes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlaggedWindow {
    /// `[start, end)` byte span in the original input.
    pub span: [usize; 2],
    /// Model-assigned malicious probability for this window, `0.0..=1.0`.
    pub score: f64,
}

/// Result of the hosted model detector for one document.
///
/// `available = false` means the model endpoint was unreachable and this leg was
/// skipped (fail-open) — `score` is then `0.0` and `flagged_windows` empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelReport {
    /// Whether the model detector actually ran (false ⇒ skipped/unreachable).
    pub available: bool,
    /// Max malicious probability across windows, `0.0..=1.0` (0 when unavailable).
    pub score: f64,
    /// Per-window detail for windows whose score met the model threshold.
    #[serde(default)]
    pub flagged_windows: Vec<FlaggedWindow>,
}

/// Final accept/reject verdict for a scanned document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Below the reject threshold — content is ingested.
    Accept,
    /// At or above the reject threshold — content is rejected.
    Reject,
}

/// Full breakdown of one document's scan.
///
/// Carries both detector legs, the blended score, the threshold in force, and the
/// verdict. Returned verbatim by the admin `score` endpoint and used to assemble
/// the ingest rejection detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanReport {
    /// Which detectors actually ran (`"pattern"`, `"model"`).
    pub detectors_run: Vec<String>,
    /// Local pattern-detector leg.
    pub pattern: PatternResult,
    /// Hosted model-detector leg (absent when the model was not requested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelReport>,
    /// Weighted blend of the two legs, `0.0..=1.0`.
    pub blended_score: f64,
    /// Reject threshold the blend was compared against.
    pub reject_threshold: f64,
    /// Final verdict.
    pub verdict: Verdict,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_report_default_is_unavailable() {
        let m = ModelReport::default();
        assert!(!m.available);
        assert!((m.score - 0.0).abs() < 1e-12);
        assert!(m.flagged_windows.is_empty());
    }

    #[test]
    fn scan_report_round_trips_through_json() {
        let r = ScanReport {
            detectors_run: vec!["pattern".into(), "model".into()],
            pattern: PatternResult::default(),
            model: Some(ModelReport {
                available: true,
                score: 0.91,
                flagged_windows: vec![FlaggedWindow { span: [0, 512], score: 0.91 }],
            }),
            blended_score: 0.9,
            reject_threshold: 0.85,
            verdict: Verdict::Reject,
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: ScanReport = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
        // Verdict renders lowercase on the wire.
        assert!(s.contains("\"verdict\":\"reject\""));
    }

    #[test]
    fn model_omitted_when_none() {
        let r = ScanReport {
            detectors_run: vec!["pattern".into()],
            pattern: PatternResult::default(),
            model: None,
            blended_score: 0.0,
            reject_threshold: 0.85,
            verdict: Verdict::Accept,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("\"model\""), "model must be omitted when None: {s}");
    }
}

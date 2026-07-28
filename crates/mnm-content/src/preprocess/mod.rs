//! Pre-chunk preprocessing: strips boilerplate noise (license headers,
//! decorative comment banners, MDX plumbing, badges) and detects generated
//! files, BEFORE chunking/embedding.
//!
//! Spec: docs/superpowers/specs/2026-07-27-preprocess-license-design.md

pub mod comment_syntax;
pub mod lexer;
pub mod rules_code;

/// Confidence-thresholded license text identification. Implemented in Phase B
/// by `spdx::detection`; `None` everywhere = the spec's degraded mode
/// (heuristic head stripping + SPDX-tag parsing only).
pub trait LicenseDetector: Sync {
    /// Return the SPDX expression for `text` when identified at >= 0.9
    /// confidence, else `None`. Input should already be capped at 8 KB.
    fn detect(&self, text: &str) -> Option<String>;
}

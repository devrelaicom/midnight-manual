//! Pre-chunk preprocessing: strips boilerplate noise (license headers,
//! decorative comment banners, MDX plumbing, badges) and detects generated
//! files, BEFORE chunking/embedding.
//!
//! Spec: docs/superpowers/specs/2026-07-27-preprocess-license-design.md

pub mod comment_syntax;
pub mod lexer;
pub mod normalize;
pub mod rules_code;
pub mod rules_markdown;

use std::path::Path;

use mnm_core::types::DocumentKind;

use crate::code::language::Language;

/// Confidence-thresholded license text identification. Implemented in Phase B
/// by `spdx::detection`; `None` everywhere = the spec's degraded mode
/// (heuristic head stripping + SPDX-tag parsing only).
pub trait LicenseDetector: Sync {
    /// Return the SPDX expression for `text` when identified at >= 0.9
    /// confidence, else `None`. Input should already be capped at 8 KB.
    fn detect(&self, text: &str) -> Option<String>;
}

/// Per-rule byte counters, aggregated into the ingest run report.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PreprocessStats {
    /// Bytes removed by license-header rules (1-2).
    pub license_bytes: usize,
    /// Bytes removed by the decorative-comment-run rule (3).
    pub decorative_bytes: usize,
    /// Bytes removed by the HTML/MDX comment rules (5-6).
    pub html_comment_bytes: usize,
    /// Bytes removed by the MDX ESM import/export rule (7).
    pub mdx_esm_bytes: usize,
    /// Bytes removed by the solo JSX component-tag rule (8).
    pub mdx_jsx_bytes: usize,
    /// Bytes removed by the badge-only-line rule.
    pub badge_bytes: usize,
    /// Bytes removed by the final normalization pass (9-11).
    pub whitespace_bytes: usize,
}

impl PreprocessStats {
    /// Merge another file's counters into this aggregate.
    pub const fn absorb(&mut self, other: &Self) {
        self.license_bytes += other.license_bytes;
        self.decorative_bytes += other.decorative_bytes;
        self.html_comment_bytes += other.html_comment_bytes;
        self.mdx_esm_bytes += other.mdx_esm_bytes;
        self.mdx_jsx_bytes += other.mdx_jsx_bytes;
        self.badge_bytes += other.badge_bytes;
        self.whitespace_bytes += other.whitespace_bytes;
    }

    /// Total bytes stripped across all rules.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.license_bytes
            + self.decorative_bytes
            + self.html_comment_bytes
            + self.mdx_esm_bytes
            + self.mdx_jsx_bytes
            + self.badge_bytes
            + self.whitespace_bytes
    }
}

/// The result of preprocessing one file body.
#[derive(Debug)]
pub struct PreprocessedDocument {
    /// Stripped + normalized text. Chunkers must only ever see this.
    pub body: String,
    /// SPDX expressions detected in-file (may be empty).
    pub licenses: Vec<String>,
    /// True: generated file -- the walker records a `GeneratedFile` skip.
    pub generated: bool,
    /// Per-rule byte counters.
    pub stats: PreprocessStats,
}

/// Run the strip rules + normalization for one file (spec Architecture).
///
/// `rel_path` supplies both the extension (the `.mdx` markdown/MDX
/// discriminator, and `Language::for_extension` for code files) and the
/// basename (the `.d.ts` generated-file exemption). Language detection is
/// extension-only -- no shebang sniffing -- a known, accepted gap (spec
/// Non-goals).
///
/// Pure and panic-free by intent; the walker still wraps it in
/// `catch_unwind`.
#[must_use]
pub fn preprocess(
    kind: DocumentKind,
    rel_path: &Path,
    body: &str,
    detector: Option<&dyn LicenseDetector>,
) -> PreprocessedDocument {
    let ext = rel_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let file_name = rel_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let mut stats = PreprocessStats::default();
    let mut licenses = Vec::new();

    let stripped = match kind {
        DocumentKind::Code => {
            let lang = Language::for_extension(ext);
            let outcome = rules_code::apply_code_rules(body, lang, file_name, detector);
            if outcome.generated {
                return PreprocessedDocument {
                    body: String::new(),
                    licenses: Vec::new(),
                    generated: true,
                    stats,
                };
            }
            stats.license_bytes = outcome.license_bytes;
            stats.decorative_bytes = outcome.decorative_bytes;
            licenses = outcome.licenses;
            rules_code::apply_edits(body, &outcome.edits)
        }
        DocumentKind::Markdown => {
            let is_mdx = ext.eq_ignore_ascii_case("mdx");
            let outcome = rules_markdown::apply_markdown_rules(body, is_mdx);
            stats.html_comment_bytes = outcome.html_comment_bytes;
            stats.mdx_esm_bytes = outcome.mdx_esm_bytes;
            stats.mdx_jsx_bytes = outcome.mdx_jsx_bytes;
            stats.badge_bytes = outcome.badge_bytes;
            rules_code::apply_edits(body, &outcome.edits)
        }
        DocumentKind::Plaintext => body.to_owned(),
    };

    let (normalized, ws) = normalize::normalize(&stripped);
    stats.whitespace_bytes = ws;
    PreprocessedDocument {
        body: normalized,
        licenses,
        generated: false,
        stats,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mnm_core::types::DocumentKind;

    use super::*;

    #[test]
    fn normalization_trims_and_collapses() {
        let out = preprocess(
            DocumentKind::Plaintext,
            Path::new("notes.txt"),
            "\n\nline one   \n\n\n\nline two\t\n\n\n",
            None,
        );
        assert_eq!(out.body, "line one\n\nline two\n");
        assert!(out.stats.whitespace_bytes > 0);
    }

    #[test]
    fn code_pipeline_end_to_end() {
        let body = "// Copyright 2024 Foo\n// Licensed under the Apache License, Version 2.0\n\n// ==========\nfn main() {}\n";
        let out = preprocess(DocumentKind::Code, Path::new("src/main.rs"), body, None);
        assert_eq!(out.body, "fn main() {}\n");
        assert!(out.stats.license_bytes > 0);
        assert!(out.stats.decorative_bytes > 0);
    }

    #[test]
    fn mdx_pipeline_end_to_end() {
        let body = "import Tabs from '@theme/Tabs';\n\n# Title\n\n<Tabs>\ncontent\n</Tabs>\n";
        let out = preprocess(DocumentKind::Markdown, Path::new("docs/page.mdx"), body, None);
        assert_eq!(out.body, "# Title\n\ncontent\n");
    }

    #[test]
    fn md_is_not_mdx() {
        let body = "import notation in prose stays\n";
        let out = preprocess(DocumentKind::Markdown, Path::new("docs/page.md"), body, None);
        assert_eq!(out.body, body);
    }

    #[test]
    fn generated_signal_propagates() {
        let body = "// @generated by tool\ncode();\n";
        let out = preprocess(DocumentKind::Code, Path::new("gen.js"), body, None);
        assert!(out.generated);
    }

    #[test]
    fn idempotent() {
        let bodies = [
            "// Copyright X\n// Licensed under MIT terms\nfn a() {}\n",
            "# T\n<!-- c -->\ntext\n",
            "plain\n\n\ntext",
        ];
        for (kind, path, body) in [
            (DocumentKind::Code, "a.rs", bodies[0]),
            (DocumentKind::Markdown, "a.md", bodies[1]),
            (DocumentKind::Plaintext, "a.txt", bodies[2]),
        ] {
            let once = preprocess(kind, Path::new(path), body, None);
            let twice = preprocess(kind, Path::new(path), &once.body, None);
            assert_eq!(once.body, twice.body, "{path}");
        }
    }
}

//! Approximate token counting for the ingest pipeline.
//!
//! v1 uses a whitespace-word-count approximation, which is good enough
//! for the per-document upload-size estimates surfaced by
//! `mnm ingest plan`. A BPE-accurate counter (matching the embedder's
//! vocabulary) is a follow-up.
//!
//! Spec: docs/superpowers/specs/2026-05-25-ingest-ux-design.md §3.4

/// Approximate token count for `text` using a whitespace word split.
#[must_use]
pub fn count(text: &str) -> u32 {
    u32::try_from(text.split_whitespace().count()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_zero() {
        assert_eq!(count(""), 0);
    }

    #[test]
    fn whitespace_only_is_zero() {
        assert_eq!(count("   \n\t  "), 0);
    }

    #[test]
    fn counts_space_separated_words() {
        assert_eq!(count("hello world how are you"), 5);
    }

    #[test]
    fn handles_newlines_and_punctuation_as_whitespace_breakers() {
        assert_eq!(count("hello\nworld,\nfoo"), 3);
    }
}

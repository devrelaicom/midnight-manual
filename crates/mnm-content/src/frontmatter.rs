//! YAML frontmatter extraction from Markdown bodies (FR-017).
//!
//! Two outputs from one parse pass:
//! - `frontmatter`: the verbatim parsed YAML as `serde_json::Value` for
//!   stable round-tripping into the `document.frontmatter` JSONB column.
//! - `provenance`: any recognized provenance fields (`attribution`, `verified`,
//!   `verified_by`, etc.) extracted into a typed `Provenance` struct.

#![allow(clippy::derive_partial_eq_without_eq)] // serde_json::Value blocks Eq

use mnm_core::provenance::Provenance;

/// Result of splitting a Markdown body into frontmatter + body.
///
/// Not `Eq` because `frontmatter` is `serde_json::Value`, which transitively
/// contains `f64`. `PartialEq` is enough for test helpers.
#[derive(Debug, Clone, PartialEq)]
pub struct FrontmatterSplit {
    /// The parsed YAML as JSON, or `None` if no frontmatter was present.
    pub frontmatter: Option<serde_json::Value>,
    /// Recognized provenance fields, defaulted where unset.
    pub provenance: Provenance,
    /// The body of the document with the frontmatter block stripped.
    pub body: String,
}

/// Strip a leading `---\n…\n---\n` YAML frontmatter block from `input`, parse it,
/// and extract the recognized provenance fields. Tolerant of CRLF line endings.
///
/// Returns `FrontmatterSplit` even when no frontmatter is present — frontmatter
/// is `None` and provenance is defaulted.
#[must_use]
pub fn split(input: &str) -> FrontmatterSplit {
    let normalized = input.replace("\r\n", "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return FrontmatterSplit {
            frontmatter: None,
            provenance: Provenance::default(),
            body: normalized,
        };
    };
    let Some(end_pos) = rest.find("\n---\n") else {
        // Opening fence with no closing fence — treat the whole thing as body.
        return FrontmatterSplit {
            frontmatter: None,
            provenance: Provenance::default(),
            body: normalized,
        };
    };
    let (yaml, body_with_closing) = rest.split_at(end_pos);
    let body = body_with_closing
        .strip_prefix("\n---\n")
        .map_or_else(|| body_with_closing.to_owned(), str::to_owned);

    let frontmatter = serde_yaml::from_str::<serde_yaml::Value>(yaml)
        .ok()
        .and_then(|v| serde_json::to_value(v).ok());

    let provenance = frontmatter
        .as_ref()
        .map(|v| {
            // Permissive: unknown keys are ignored; missing keys take defaults.
            serde_json::from_value::<Provenance>(v.clone()).unwrap_or_default()
        })
        .unwrap_or_default();

    FrontmatterSplit { frontmatter, provenance, body }
}

/// Build a frontmatter-free split for a non-Markdown document.
///
/// No frontmatter is extracted, provenance is defaulted, and the whole input
/// becomes the body (with CRLF normalized to LF, matching how [`split`] hands
/// back a frontmatter-less body).
///
/// Frontmatter extraction is a Markdown-only convention (FR-017). Running
/// [`split`] on YAML/code files silently swallows a leading `---`-delimited
/// block into the frontmatter column instead of the searchable body — routine
/// content loss on multi-document or `---`-prefixed YAML (issue #161). The
/// walker calls this for every non-Markdown [`DocumentKind`] so the body is
/// preserved verbatim.
///
/// [`DocumentKind`]: mnm_core::types::DocumentKind
#[must_use]
pub fn passthrough(input: &str) -> FrontmatterSplit {
    FrontmatterSplit {
        frontmatter: None,
        provenance: Provenance::default(),
        body: input.replace("\r\n", "\n"),
    }
}

#[cfg(test)]
mod tests {
    use mnm_core::provenance::Attribution;

    use super::*;

    #[test]
    fn no_frontmatter_passes_body_through() {
        let r = split("just a body\nwith\nlines");
        assert!(r.frontmatter.is_none());
        assert_eq!(r.provenance, Provenance::default());
        assert_eq!(r.body, "just a body\nwith\nlines");
    }

    #[test]
    fn extracts_recognized_provenance_fields() {
        let input = "---\nverified: true\nverified_by: midnight-foundation\nattribution: foundation\n---\nBody.\n";
        let r = split(input);
        assert_eq!(r.body, "Body.\n");
        assert!(r.frontmatter.is_some());
        assert!(r.provenance.verified);
        assert_eq!(r.provenance.verified_by.as_deref(), Some("midnight-foundation"));
        assert_eq!(r.provenance.attribution, Attribution::Foundation);
    }

    #[test]
    fn unknown_keys_are_preserved_in_frontmatter_jsonb() {
        let input = "---\ncustom_field: 42\n---\nBody.";
        let r = split(input);
        let fm = r.frontmatter.unwrap();
        assert_eq!(fm["custom_field"], 42);
    }

    #[test]
    fn malformed_yaml_does_not_panic() {
        let input = "---\nthis : is = not = valid\n: yaml: :\n---\nBody.";
        let r = split(input);
        // Malformed frontmatter -> None, body still present.
        assert!(r.frontmatter.is_none());
        assert!(r.body.contains("Body."));
    }

    #[test]
    fn crlf_endings_tolerated() {
        let input = "---\r\nverified: true\r\n---\r\nBody.\r\n";
        let r = split(input);
        assert!(r.provenance.verified);
        assert_eq!(r.body, "Body.\n");
    }

    #[test]
    fn open_fence_without_close_is_passed_through() {
        let input = "---\nstart but no end";
        let r = split(input);
        assert!(r.frontmatter.is_none());
        assert_eq!(r.body, input);
    }

    #[test]
    fn passthrough_keeps_leading_fenced_block_in_body() {
        // A `---`-prefixed YAML doc that `split` would swallow into the
        // frontmatter column survives intact through `passthrough` (issue #161).
        let input =
            "---\napiVersion: v1\nkind: Service\n---\napiVersion: apps/v1\nkind: Deployment\n";
        let r = passthrough(input);
        assert!(r.frontmatter.is_none());
        assert_eq!(r.provenance, Provenance::default());
        assert_eq!(r.body, input);
    }

    #[test]
    fn passthrough_normalizes_crlf_like_split() {
        let r = passthrough("---\r\nfoo: bar\r\n---\r\n");
        assert_eq!(r.body, "---\nfoo: bar\n---\n");
        assert!(r.frontmatter.is_none());
    }
}

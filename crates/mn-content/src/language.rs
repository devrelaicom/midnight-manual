//! Filename-extension → IANA-like language name lookup. Populates the
//! `language` column on `document` rows during ingest (§3.4 of the
//! ingest-UX design spec). Returns `None` for unknown extensions.

use std::path::Path;

/// Map a file path's extension to a stable language identifier.
///
/// Returns `None` when the extension is unknown or absent. Matching is
/// case-insensitive on the extension only.
#[must_use]
pub fn from_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "md" | "mdx" => "markdown",
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "compact" => "compact",
        "txt" => "plaintext",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_resolve() {
        assert_eq!(from_path(Path::new("foo.md")), Some("markdown"));
        assert_eq!(from_path(Path::new("foo.MD")), Some("markdown"));
        assert_eq!(from_path(Path::new("foo.compact")), Some("compact"));
        assert_eq!(from_path(Path::new("a/b/c.tsx")), Some("typescript"));
    }

    #[test]
    fn unknown_or_missing_extension_returns_none() {
        assert_eq!(from_path(Path::new("foo")), None);
        assert_eq!(from_path(Path::new("foo.unknown")), None);
        assert_eq!(from_path(Path::new("")), None);
    }
}

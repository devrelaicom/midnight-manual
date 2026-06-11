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
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "compact" => "compact",
        "txt" => "plaintext",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "py" | "pyi" => "python",
        "go" => "go",
        "sol" => "solidity",
        "sh" | "bash" => "bash",
        "scm" | "ss" | "sld" => "scheme",
        "java" => "java",
        "swift" => "swift",
        "rb" => "ruby",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "hs" => "haskell",
        "html" | "htm" => "html",
        "xml" | "csproj" | "nuspec" | "plist" => "xml",
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

    #[test]
    fn code_chunker_languages_are_discoverable() {
        // Every language the code chunker supports must resolve here, else
        // glob-included files of that language bypass discovery defaults (D7).
        let cases = [
            ("a.py", "python"),
            ("a.pyi", "python"),
            ("a.go", "go"),
            ("a.sol", "solidity"),
            ("a.sh", "bash"),
            ("a.bash", "bash"),
            ("a.scm", "scheme"),
            ("a.ss", "scheme"),
            ("a.sld", "scheme"),
            ("a.java", "java"),
            ("a.swift", "swift"),
            ("a.rb", "ruby"),
            ("a.kt", "kotlin"),
            ("a.kts", "kotlin"),
            ("a.cs", "csharp"),
            ("a.hs", "haskell"),
            ("a.html", "html"),
            ("a.htm", "html"),
            ("a.xml", "xml"),
            ("a.csproj", "xml"),
            ("a.nuspec", "xml"),
            ("a.plist", "xml"),
            ("a.mjs", "javascript"),
            ("a.cjs", "javascript"),
        ];
        for (path, lang) in cases {
            assert_eq!(from_path(Path::new(path)), Some(lang), "{path}");
        }
    }
}

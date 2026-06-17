//! Extension/shebang → Language, and the per-language dispatch key.

/// A source language the chunker recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// Rust source files (`.rs`).
    Rust,
    /// TypeScript / TSX source files (`.ts`, `.tsx`).
    TypeScript,
    /// JavaScript / JSX / ES module source files (`.js`, `.jsx`, `.mjs`, `.cjs`).
    JavaScript,
    /// Scheme / R7RS source files (`.scm`, `.ss`, `.sld`).
    Scheme,
    /// Bash / POSIX shell scripts (`.sh`, `.bash`) or detected via shebang.
    Bash,
    /// Go source files (`.go`).
    Go,
    /// Python source files (`.py`, `.pyi`) or detected via shebang.
    Python,
    /// Solidity smart-contract source files (`.sol`).
    Solidity,
    /// TOML configuration files (`.toml`).
    Toml,
    /// YAML configuration files (`.yaml`, `.yml`).
    Yaml,
    /// HTML / HTM markup files (`.html`, `.htm`).
    Html,
    /// XML-family files (`.xml`, `.csproj`, `.nuspec`, `.plist`).
    Xml,
    /// Swift source files (`.swift`).
    Swift,
    /// Ruby source files (`.rb`).
    Ruby,
    /// Kotlin source files (`.kt`, `.kts`).
    Kotlin,
    /// C# source files (`.cs`).
    CSharp,
    /// Haskell source files (`.hs`).
    Haskell,
    /// Java source files (`.java`).
    Java,
    /// Compact smart-contract source files (`.compact`).
    Compact,
    /// Unknown extension → line-window fallback.
    Other,
}

impl Language {
    /// Map a lowercased extension (no dot) to a language.
    #[must_use]
    pub fn for_extension(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Self::Rust,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "scm" | "ss" | "sld" => Self::Scheme,
            "sh" | "bash" => Self::Bash,
            "go" => Self::Go,
            "py" | "pyi" => Self::Python,
            "sol" => Self::Solidity,
            "toml" => Self::Toml,
            "yaml" | "yml" => Self::Yaml,
            "html" | "htm" => Self::Html,
            "xml" | "csproj" | "nuspec" | "plist" => Self::Xml,
            "swift" => Self::Swift,
            "rb" => Self::Ruby,
            "kt" | "kts" => Self::Kotlin,
            "cs" => Self::CSharp,
            "hs" => Self::Haskell,
            "java" => Self::Java,
            "compact" => Self::Compact,
            _ => Self::Other,
        }
    }

    /// Detect language from a shebang line (EC-53). Returns `None` if absent.
    #[must_use]
    pub fn for_shebang(body: &str) -> Option<Self> {
        let first = body.lines().next()?;
        let first = first.strip_prefix("#!")?;
        if first.contains("bash") || first.contains("/sh") || first.ends_with("sh") {
            Some(Self::Bash)
        } else if first.contains("python") {
            Some(Self::Python)
        } else if first.contains("node") {
            Some(Self::JavaScript)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extension_mapping() {
        assert_eq!(Language::for_extension("rs"), Language::Rust);
        assert_eq!(Language::for_extension("tsx"), Language::TypeScript);
        assert_eq!(Language::for_extension("mjs"), Language::JavaScript);
        assert_eq!(Language::for_extension("compact"), Language::Compact);
        assert_eq!(Language::for_extension("zzz"), Language::Other);
    }
    #[test]
    fn shebang_detects_bash() {
        assert_eq!(Language::for_shebang("#!/usr/bin/env bash\n..."), Some(Language::Bash));
        assert_eq!(Language::for_shebang("no shebang"), None);
    }
}

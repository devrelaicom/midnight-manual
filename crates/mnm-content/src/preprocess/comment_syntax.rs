//! Per-language comment syntax used by the preprocess lexer. Centralizes the
//! comment-token knowledge that previously existed only as scattered literals
//! at each chunker's `run_tree_sitter` call site.

use crate::code::language::Language;

/// The comment (and naive string) syntax of one language.
#[derive(Debug, Clone, Copy)]
pub struct CommentSyntax {
    /// Line-comment openers (`//`, `#`, `--`, `;`).
    pub line: &'static [&'static str],
    /// Block-comment (open, close) delimiter pairs.
    pub block: &'static [(&'static str, &'static str)],
    /// String delimiters for naive quote-toggle tracking. Known accepted
    /// limitation (spec §Comment lexer): template literals, regex literals,
    /// raw strings, and triple quotes can defeat the toggle.
    pub strings: &'static [char],
}

/// Comment syntax for `lang`, or `None` for `Language::Other` (no comment
/// rules run; normalization still applies).
#[must_use]
pub const fn comment_syntax(lang: Language) -> Option<CommentSyntax> {
    const C_LIKE: CommentSyntax = CommentSyntax {
        line: &["//"],
        block: &[("/*", "*/")],
        strings: &['"', '\''],
    };
    Some(match lang {
        Language::Rust | Language::Go => CommentSyntax { strings: &['"'], ..C_LIKE },
        Language::TypeScript | Language::JavaScript | Language::Solidity => CommentSyntax {
            strings: &['"', '\'', '`'],
            ..C_LIKE
        },
        Language::Swift
        | Language::Kotlin
        | Language::CSharp
        | Language::Java
        | Language::Compact => C_LIKE,
        Language::Bash | Language::Toml | Language::Yaml | Language::Python | Language::Ruby => {
            CommentSyntax {
                line: &["#"],
                block: &[],
                strings: &['"', '\''],
            }
        }
        Language::Scheme => CommentSyntax {
            line: &[";"],
            block: &[("#|", "|#")],
            strings: &['"'],
        },
        Language::Haskell => CommentSyntax {
            line: &["--"],
            block: &[("{-", "-}")],
            strings: &['"'],
        },
        Language::Html | Language::Xml => CommentSyntax {
            line: &[],
            block: &[("<!--", "-->")],
            strings: &[],
        },
        Language::Other => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::language::Language;

    #[test]
    fn rust_has_line_and_block_comments() {
        let cs = comment_syntax(Language::Rust).unwrap();
        assert!(cs.line.contains(&"//"));
        assert!(cs.block.contains(&("/*", "*/")));
        assert!(cs.strings.contains(&'"'));
    }

    #[test]
    fn python_and_bash_are_hash_line_only() {
        for lang in [
            Language::Python,
            Language::Bash,
            Language::Toml,
            Language::Yaml,
        ] {
            let cs = comment_syntax(lang).unwrap();
            assert_eq!(cs.line, ["#"], "{lang:?}");
            assert!(cs.block.is_empty(), "{lang:?}");
        }
    }

    #[test]
    fn xml_family_is_block_only() {
        for lang in [Language::Html, Language::Xml] {
            let cs = comment_syntax(lang).unwrap();
            assert!(cs.line.is_empty());
            assert_eq!(cs.block, [("<!--", "-->")]);
        }
    }

    #[test]
    fn other_has_no_syntax() {
        assert!(comment_syntax(Language::Other).is_none());
    }
}

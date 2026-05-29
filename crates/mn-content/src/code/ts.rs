//! TypeScript/TSX chunker.
//!
//! `tree-sitter-typescript` ships two grammars (`LANGUAGE_TYPESCRIPT` and
//! `LANGUAGE_TSX`); `tsx` selects the JSX-aware one.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for TypeScript.
#[must_use]
pub const fn ts_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "internal_module",
            label: "namespace",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "module",
            label: "namespace",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "class_declaration",
            label: "class",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "interface_declaration",
            label: "interface",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "function_declaration",
            label: "function",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "method_definition",
            label: "method",
            name_field: Some("name"),
        },
    ]
}

/// TS/TSX chunker. `tsx = true` selects the JSX-aware grammar.
pub struct TypeScriptChunker {
    /// Whether to use the TSX (JSX-aware) grammar.
    pub tsx: bool,
}

impl Chunker for TypeScriptChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        let lang = if self.tsx {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        };
        crate::code::run_tree_sitter(body, cfg, &lang, ts_kind_table())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn class_method_symbol_path() {
        let src = "class Widget {\n  render() { return 1; }\n}\nexport function f() {}\n";
        let chunks = TypeScriptChunker { tsx: false }
            .chunk(src, &ChunkerConfig::default())
            .unwrap();
        let m = chunks
            .iter()
            .find(|c| c.content.contains("render"))
            .unwrap();
        assert!(m
            .symbol_path
            .iter()
            .any(|s| s.kind == "class" && s.name == "Widget"));
    }

    #[test]
    fn tsx_component_parses() {
        let src = "function App() { return <div>{x}</div>; }\n";
        let chunks = TypeScriptChunker { tsx: true }
            .chunk(src, &ChunkerConfig::default())
            .unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| !c.fallback_used));
    }
}

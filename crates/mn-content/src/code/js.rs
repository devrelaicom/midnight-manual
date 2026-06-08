//! JavaScript/JSX chunker.
//!
//! `tree-sitter-javascript` handles JSX within the single grammar.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for JavaScript.
#[must_use]
pub const fn js_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "class_declaration",
            label: "class",
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

/// JavaScript code chunker.
pub struct JavaScriptChunker;

impl Chunker for JavaScriptChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_javascript::LANGUAGE.into(),
            js_kind_table(),
            "//",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn js_function_and_class_paths() {
        let src = "class A { m() {} }\nfunction g() {}\n";
        let chunks = JavaScriptChunker
            .chunk(src, &ChunkerConfig::default())
            .unwrap();
        assert!(chunks
            .iter()
            .any(|c| c.symbol_path.iter().any(|s| s.name == "A")));
    }
}

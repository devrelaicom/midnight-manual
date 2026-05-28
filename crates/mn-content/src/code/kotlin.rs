//! Kotlin chunker: tree-sitter-kotlin-ng + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for Kotlin.
#[must_use]
pub const fn kotlin_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "class_declaration",
            label: "class",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "object_declaration",
            label: "object",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "function_declaration",
            label: "fun",
            name_field: Some("name"),
        },
    ]
}

/// Kotlin code chunker backed by `tree-sitter-kotlin-ng`.
pub struct KotlinChunker;

impl Chunker for KotlinChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_kotlin_ng::LANGUAGE.into(),
            kotlin_kind_table(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn kotlin_class_and_fun_paths() {
        // Class declaration first so that byte 0 lands inside `class_declaration`
        // when the splitter emits a single range.
        let src = "class Foo {\n  fun bar() {}\n}\n";
        let chunks = KotlinChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(chunks
            .iter()
            .any(|c| c.symbol_path.iter().any(|s| s.kind == "class")));
    }
}

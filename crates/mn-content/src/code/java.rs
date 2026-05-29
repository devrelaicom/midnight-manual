//! Java chunker: tree-sitter-java + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for Java.
#[must_use]
pub const fn java_kind_table() -> KindTable {
    &[
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
            node_kind: "method_declaration",
            label: "method",
            name_field: Some("name"),
        },
    ]
}

/// Java code chunker backed by `tree-sitter-java`.
pub struct JavaChunker;

impl Chunker for JavaChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_java::LANGUAGE.into(),
            java_kind_table(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn java_class_and_method_paths() {
        // Class declaration first so that byte 0 lands inside `class_declaration`
        // when the splitter emits a single range.
        let src = "class Foo {\n  void bar() {}\n}\n";
        let chunks = JavaChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().any(|c| c
            .symbol_path
            .iter()
            .any(|s| s.kind == "class" && s.name == "Foo")));
    }
}

//! Python chunker: tree-sitter-python + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for Python.
#[must_use]
pub const fn python_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "class_definition",
            label: "class",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "function_definition",
            label: "def",
            name_field: Some("name"),
        },
    ]
}

/// Python code chunker backed by `tree-sitter-python`.
pub struct PythonChunker;

impl Chunker for PythonChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_python::LANGUAGE.into(),
            python_kind_table(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn python_class_and_method_paths() {
        let src = "class Foo:\n    def bar(self):\n        return 1\n";
        let chunks = PythonChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().any(|c| c
            .symbol_path
            .iter()
            .any(|s| s.kind == "class" && s.name == "Foo")));
    }
}

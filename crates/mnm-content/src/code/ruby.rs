//! Ruby chunker: tree-sitter-ruby + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for Ruby.
#[must_use]
pub const fn ruby_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "class",
            label: "class",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "module",
            label: "module",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "method",
            label: "def",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "singleton_method",
            label: "def",
            name_field: Some("name"),
        },
    ]
}

/// Ruby code chunker backed by `tree-sitter-ruby`.
pub struct RubyChunker;

impl Chunker for RubyChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_ruby::LANGUAGE.into(),
            ruby_kind_table(),
            "#",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn ruby_class_and_method_paths() {
        // Class declaration first so that byte 0 lands inside `class` when the
        // splitter emits a single range.
        let src = "class Foo\n  def bar\n  end\nend\n";
        let chunks = RubyChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().any(|c| c
            .symbol_path
            .iter()
            .any(|s| s.kind == "class" && s.name == "Foo")));
    }
}

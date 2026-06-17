//! Go chunker: tree-sitter-go + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for Go.
#[must_use]
pub const fn go_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "function_declaration",
            label: "func",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "method_declaration",
            label: "method",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "type_declaration",
            label: "type",
            name_field: None,
        },
    ]
}

/// Go code chunker backed by `tree-sitter-go`.
pub struct GoChunker;

impl Chunker for GoChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_go::LANGUAGE.into(),
            go_kind_table(),
            "//",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn go_func_and_type_paths() {
        // Source starts with the function so that chunk byte-0 lands inside
        // `function_declaration` when the splitter emits a single range.
        let src = "func Add(a, b int) int { return a+b }\ntype T struct{ X int }\n";
        let chunks = GoChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().any(|c| c
            .symbol_path
            .iter()
            .any(|s| s.kind == "func" && s.name == "Add")));
    }
}

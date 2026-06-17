//! Haskell chunker: tree-sitter-haskell + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for Haskell.
///
/// The Haskell grammar (0.23.1) surfaces top-level bindings as `function`
/// nodes, type declarations as `data_type`, and type-class declarations as
/// `class`.  Each carries a `name` field that resolves to either a `variable`
/// or `name` leaf, so `Some("name")` reliably extracts the identifier text.
#[must_use]
pub const fn haskell_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "function",
            label: "fn",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "data_type",
            label: "data",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "class",
            label: "class",
            name_field: Some("name"),
        },
    ]
}

/// Haskell code chunker backed by `tree-sitter-haskell`.
pub struct HaskellChunker;

impl Chunker for HaskellChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_haskell::LANGUAGE.into(),
            haskell_kind_table(),
            "--",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn haskell_function_path() {
        // Top-level function declaration first so byte 0 lands inside `function`
        // when the splitter emits a single range.
        let src = "square x = x * x\n";
        let chunks = HaskellChunker
            .chunk(src, &ChunkerConfig::default())
            .unwrap();
        assert!(chunks
            .iter()
            .any(|c| c.symbol_path.iter().any(|s| s.kind == "fn")));
    }
}

//! TOML chunker: tree-sitter-toml-ng + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for TOML.
///
/// TOML structural elements — `table` (`[header]`) and `table_array_element`
/// (`[[array-table]]`) — are mapped to the `"key"` label.  The name field is
/// `None` because table headers store their key as a `bare_key`/`quoted_key`
/// child node whose kind does not contain the word `"identifier"`, so the
/// generic walker cannot extract it automatically; the path entry is still
/// emitted with an empty name.
#[must_use]
pub const fn toml_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "table",
            label: "key",
            name_field: None,
        },
        KindEntry {
            node_kind: "table_array_element",
            label: "key",
            name_field: None,
        },
    ]
}

/// TOML configuration chunker backed by `tree-sitter-toml-ng`.
pub struct TomlChunker;

impl Chunker for TomlChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_toml_ng::LANGUAGE.into(),
            toml_kind_table(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn toml_table_symbol_path() {
        // The `[package]` table starts at byte 0, so the first chunk's
        // symbol_path should include an entry with kind == "key".
        let src = "[package]\nname = \"x\"\n";
        let chunks = TomlChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_path.iter().any(|s| s.kind == "key")),
            "expected a symbol-path entry with kind 'key', got: {chunks:?}"
        );
    }
}

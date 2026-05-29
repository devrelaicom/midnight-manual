//! Solidity chunker: tree-sitter-solidity + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for Solidity.
#[must_use]
pub const fn solidity_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "contract_declaration",
            label: "contract",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "function_definition",
            label: "function",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "modifier_definition",
            label: "modifier",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "struct_declaration",
            label: "struct",
            name_field: Some("name"),
        },
    ]
}

/// Solidity smart-contract chunker backed by `tree-sitter-solidity`.
pub struct SolidityChunker;

impl Chunker for SolidityChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_solidity::LANGUAGE.into(),
            solidity_kind_table(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn solidity_contract_path() {
        let src = "contract C {\n  function f() public {}\n}\n";
        let chunks = SolidityChunker
            .chunk(src, &ChunkerConfig::default())
            .unwrap();
        assert!(chunks.iter().any(|c| c
            .symbol_path
            .iter()
            .any(|s| s.kind == "contract" && s.name == "C")));
    }
}

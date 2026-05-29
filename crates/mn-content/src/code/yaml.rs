//! YAML chunker: tree-sitter-yaml + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for YAML.
///
/// `block_mapping_pair` represents a key-value pair in a YAML mapping (e.g.
/// `name: value`).  The `key` field of that node holds the key text, so
/// `name_field: Some("key")` extracts it directly and populates the symbol
/// path with `kind = "key"` and `name = <key-text>`.
#[must_use]
pub const fn yaml_kind_table() -> KindTable {
    &[KindEntry {
        node_kind: "block_mapping_pair",
        label: "key",
        name_field: Some("key"),
    }]
}

/// YAML configuration chunker backed by `tree-sitter-yaml`.
pub struct YamlChunker;

impl Chunker for YamlChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_yaml::LANGUAGE.into(),
            yaml_kind_table(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn yaml_mapping_pair_symbol_path() {
        // `name:` starts at byte 0 so the first chunk should report kind "key".
        let src = "name: value\nfoo: bar\n";
        let chunks = YamlChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_path.iter().any(|s| s.kind == "key")),
            "expected a symbol-path entry with kind 'key', got: {chunks:?}"
        );
    }
}

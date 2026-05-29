//! Bash/shell chunker.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for Bash.
#[must_use]
pub const fn bash_kind_table() -> KindTable {
    &[KindEntry {
        node_kind: "function_definition",
        label: "fn",
        name_field: Some("name"),
    }]
}

/// Bash/shell code chunker.
pub struct BashChunker;

impl Chunker for BashChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_bash::LANGUAGE.into(),
            bash_kind_table(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};
    #[test]
    fn bash_function_path() {
        let src = "greet() {\n  echo hi\n}\n";
        let chunks = BashChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(chunks
            .iter()
            .any(|c| c.symbol_path.iter().any(|s| s.name == "greet")));
    }
}

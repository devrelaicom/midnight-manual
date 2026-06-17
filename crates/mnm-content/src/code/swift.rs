//! Swift chunker: tree-sitter-swift + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for Swift.
///
/// Swift uses `class_declaration` for both `class` and `struct` (distinguished
/// by the `declaration_kind` field).  `function_declaration` covers top-level
/// and member functions; `protocol_declaration` covers protocol definitions.
/// The `name` field is always present and resolves to a `type_identifier` (for
/// types) or `simple_identifier` (for functions), so `Some("name")` extracts
/// the text reliably.
#[must_use]
pub const fn swift_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "class_declaration",
            label: "class",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "function_declaration",
            label: "func",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "protocol_declaration",
            label: "protocol",
            name_field: Some("name"),
        },
    ]
}

/// Swift code chunker backed by `tree-sitter-swift`.
pub struct SwiftChunker;

impl Chunker for SwiftChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_swift::LANGUAGE.into(),
            swift_kind_table(),
            "//",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn swift_class_and_func_paths() {
        // Class declaration first so that byte 0 lands inside `class_declaration`
        // when the splitter emits a single range.
        let src = "class Widget {\n  func render() {}\n}\n";
        let chunks = SwiftChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(chunks
            .iter()
            .any(|c| c.symbol_path.iter().any(|s| s.kind == "class")));
    }
}

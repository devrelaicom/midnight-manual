//! Rust chunker: tree-sitter-rust + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for Rust.
#[must_use]
pub const fn rust_kind_table() -> KindTable {
    &[
        KindEntry {
            node_kind: "mod_item",
            label: "mod",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "impl_item",
            label: "impl",
            name_field: Some("type"),
        },
        KindEntry {
            node_kind: "trait_item",
            label: "trait",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "struct_item",
            label: "struct",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "enum_item",
            label: "enum",
            name_field: Some("name"),
        },
        KindEntry {
            node_kind: "function_item",
            label: "fn",
            name_field: Some("name"),
        },
    ]
}

/// Rust code chunker backed by `tree-sitter-rust`.
pub struct RustChunker;

impl Chunker for RustChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_rust::LANGUAGE.into(),
            rust_kind_table(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn chunks_carry_symbol_path() {
        let src = "impl Foo {\n    fn bar(&self) { let x = 1; }\n}\n\nfn free() {}\n";
        let chunks = RustChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(!chunks.is_empty());
        let bar = chunks
            .iter()
            .find(|c| c.content.contains("fn bar"))
            .unwrap();
        assert!(bar
            .symbol_path
            .iter()
            .map(|s| s.kind.as_str())
            .any(|x| x == "impl"));
        assert!(bar.symbol_path.iter().any(|s| s.name == "Foo"));
        assert!(!bar.fallback_used);
    }

    #[test]
    fn leading_preamble_still_yields_symbol_path() {
        // A small file that fits in a single chunk and opens with file-level
        // preamble (doc comment + `use`) before the first item: the chunk's
        // start byte is outside any named item, so the symbol path must be
        // recovered from the first item contained in the chunk (regression for
        // the code-ingest E2E, where real files begin with comments/imports).
        let src = "//! Crate docs.\nuse std::fmt;\n\npub struct Widget {\n    x: i32,\n}\n\npub fn helper() -> i32 {\n    1\n}\n";
        let chunks = RustChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(!chunks.is_empty());
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_path.iter().any(|s| s.name == "Widget")),
            "single-chunk file with leading preamble must still record a symbol_path: {:?}",
            chunks.iter().map(|c| &c.symbol_path).collect::<Vec<_>>()
        );
        assert!(chunks.iter().all(|c| !c.fallback_used));
    }

    #[test]
    fn malformed_falls_back_to_line_window() {
        let src = "fn broken( { { { unterminated\n".repeat(40);
        let chunks = RustChunker.chunk(&src, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().any(|c| c.fallback_used));
    }
}

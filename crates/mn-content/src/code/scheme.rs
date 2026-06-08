//! Scheme chunker over a vendored tree-sitter grammar.
//!
//! The Compact compiler is written in Scheme, so we chunk Scheme source when
//! reading the compiler's own code (distinct from Compact contracts).

use tree_sitter_language::LanguageFn;

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::KindTable;

// The vendored grammar's `parser.c` (compiled by build.rs) exposes this C symbol.
#[allow(unsafe_code)]
extern "C" {
    fn tree_sitter_scheme() -> *const ();
}

/// The tree-sitter `LanguageFn` for the vendored Scheme grammar.
///
/// `LanguageFn::from_raw` is the idiomatic tree-sitter 0.25 binding (mirroring the
/// real grammar crates such as `tree-sitter-rust`). The plan's
/// `extern "C" { fn tree_sitter_scheme() -> tree_sitter::Language; }` is incorrect:
/// `tree_sitter::Language` is not an FFI-returnable C type.
#[allow(unsafe_code)]
const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_scheme) };

/// Node-kind → symbol mapping for Scheme.
///
/// This grammar (`6cdh/tree-sitter-scheme`) is purely syntactic: it has no
/// `definition` node. A top-level `(define (square x) ...)` form parses as a
/// generic `list` whose children are bare `symbol` nodes (the head `symbol` being
/// `"define"`), and `list` exposes no name field. There is also no `identifier`
/// node kind (identifiers are `symbol`), so the generic name walker cannot extract
/// a meaningful name. There is therefore no clean define-form node to map, so the
/// table is empty: chunks still split semantically, `symbol_path` just stays empty.
#[must_use]
pub const fn scheme_kind_table() -> KindTable {
    &[]
}

/// Scheme code chunker backed by the vendored grammar.
pub struct SchemeChunker;

impl Chunker for SchemeChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(body, cfg, &LANGUAGE.into(), scheme_kind_table(), ";")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn scheme_defines_chunk() {
        let src = "(define (square x) (* x x))\n(define y 10)\n";
        let chunks = SchemeChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(!chunks.is_empty());
    }
}

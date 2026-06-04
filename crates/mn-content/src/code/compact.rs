//! Compact chunker: compactp (rowan CST) + token budgeting + symbol paths.
//!
//! compactp is rowan-based, so this is a self-contained walker behind the
//! shared [`Chunker`] trait — parallel to the Markdown chunker, not the
//! tree-sitter language chunkers. Falls back to line-window on a catastrophic
//! parse.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};

/// Compact code chunker backed by the `compactp` parser.
pub struct CompactChunker;

use compactp_syntax::SyntaxNode;

impl Chunker for CompactChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }
        let parsed = compactp_parser::parse(body);
        let _root = SyntaxNode::new_root(parsed.green);

        // Minimal: one chunk for the whole file (split + symbol_path land in
        // later tasks). Trim leading/trailing whitespace off the range so the
        // single chunk is byte-accurate to its content.
        let start = body.len() - body.trim_start().len();
        let end = body.trim_end().len();
        let content = body[start..end].to_string();
        let chunk = Chunk {
            token_count: crate::tokens::count(&content),
            symbol_path: Vec::new(),
            content,
            heading_path: Vec::new(),
            start_byte: start,
            end_byte: end,
            chunk_index: 0,
            fallback_used: false,
        };
        Ok(vec![chunk])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    const COUNTER: &str = "import CompactStandardLibrary;\n\nexport ledger round: Counter;\n\nexport circuit increment(): [] {\n  round.increment(1);\n}\n";

    #[test]
    fn parses_and_emits_a_chunk() {
        let chunks = CompactChunker.chunk(COUNTER, &ChunkerConfig::default()).unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| !c.fallback_used));
        // chunks reconstruct the bytes they claim
        for c in &chunks {
            assert_eq!(c.content, COUNTER[c.start_byte..c.end_byte]);
        }
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(CompactChunker.chunk("   \n\t", &ChunkerConfig::default()).unwrap().is_empty());
    }
}

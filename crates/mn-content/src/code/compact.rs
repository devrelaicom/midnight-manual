//! Compact chunker: compactp (rowan CST) + token budgeting + symbol paths.
//!
//! compactp is rowan-based, so this is a self-contained walker behind the
//! shared [`Chunker`] trait — parallel to the Markdown chunker, not the
//! tree-sitter language chunkers. Falls back to line-window on a catastrophic
//! parse.

use std::ops::Range;

use compactp_ast::{AstNode, SourceFile};
use compactp_syntax::SyntaxNode;

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::line_window::LineWindowChunker;

/// Compact code chunker backed by the `compactp` parser.
pub struct CompactChunker;

impl Chunker for CompactChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }
        let parsed = compactp_parser::parse(body);
        let root = SyntaxNode::new_root(parsed.green);

        if SourceFile::cast(root.clone()).is_none() {
            return LineWindowChunker.chunk(body, cfg);
        }

        let mut ranges = Vec::new();
        split_node(&root, body, cfg.max_tokens, &mut ranges);
        if ranges.is_empty() {
            return LineWindowChunker.chunk(body, cfg);
        }

        let mut chunks = Vec::with_capacity(ranges.len());
        for r in ranges {
            let content = body[r.clone()].to_string();
            if content.trim().is_empty() {
                continue;
            }
            let idx = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
            chunks.push(Chunk {
                token_count: crate::tokens::count(&content),
                symbol_path: Vec::new(),
                content,
                heading_path: Vec::new(),
                start_byte: r.start,
                end_byte: r.end,
                chunk_index: idx,
                fallback_used: false,
            });
        }
        if chunks.is_empty() {
            return LineWindowChunker.chunk(body, cfg);
        }
        Ok(chunks)
    }
}

/// Split `node` into byte ranges, each within `budget` tokens where the tree
/// allows. Adjacent children are packed (absorbing inter-child trivia); any
/// child *node* that alone exceeds `budget` is recursed into. A leaf that
/// cannot be divided is emitted as a single (possibly over-budget) range, so
/// the produced ranges always tile `node`'s span with no gaps.
fn split_node(node: &SyntaxNode, body: &str, budget: u32, out: &mut Vec<Range<usize>>) {
    let nr = node.text_range();
    let (nstart, nend) = (usize::from(nr.start()), usize::from(nr.end()));
    if nstart >= nend {
        return;
    }
    if crate::tokens::count(&body[nstart..nend]) <= budget {
        out.push(nstart..nend);
        return;
    }

    let mut run: Option<Range<usize>> = None;
    let mut run_tokens = 0u32;
    for child in node.children_with_tokens() {
        let cr = child.text_range();
        let (cs, ce) = (usize::from(cr.start()), usize::from(cr.end()));
        if cs >= ce {
            continue;
        }
        let ct = crate::tokens::count(&body[cs..ce]);

        // An oversize child *node* is split recursively.
        if let Some(child_node) = child.as_node() {
            if ct > budget {
                if let Some(prev) = run.take() {
                    out.push(prev);
                }
                split_node(child_node, body, budget, out);
                continue;
            }
        }

        // Otherwise pack the child (a small node, or any token) into the run.
        match run.as_mut() {
            None => {
                run = Some(cs..ce);
                run_tokens = ct;
            }
            Some(r) => {
                if run_tokens.saturating_add(ct) > budget {
                    out.push(r.clone());
                    run = Some(cs..ce);
                    run_tokens = ct;
                } else {
                    r.end = ce;
                    run_tokens = run_tokens.saturating_add(ct);
                }
            }
        }
    }
    if let Some(r) = run.take() {
        out.push(r);
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

    #[test]
    fn small_siblings_pack_into_one_chunk() {
        // Whole file fits the default 400-token budget → a single chunk.
        let chunks = CompactChunker.chunk(COUNTER, &ChunkerConfig::default()).unwrap();
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn tiny_budget_splits_into_multiple_chunks() {
        let cfg = ChunkerConfig { max_tokens: 8, ..ChunkerConfig::default() };
        let chunks = CompactChunker.chunk(COUNTER, &cfg).unwrap();
        assert!(chunks.len() >= 2, "tiny budget should split: got {}", chunks.len());
        // sorted + non-overlapping
        for w in chunks.windows(2) {
            assert!(w[0].end_byte <= w[1].start_byte);
        }
        // byte-accurate
        for c in &chunks {
            assert_eq!(c.content, COUNTER[c.start_byte..c.end_byte]);
        }
        assert!(chunks.iter().all(|c| !c.fallback_used));
    }
}

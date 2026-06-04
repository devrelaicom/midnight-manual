//! Compact chunker: compactp (rowan CST) + token budgeting + symbol paths.
//!
//! compactp is rowan-based, so this is a self-contained walker behind the
//! shared [`Chunker`] trait — parallel to the Markdown chunker, not the
//! tree-sitter language chunkers. Falls back to line-window on a catastrophic
//! parse.

use std::ops::Range;

use compactp_ast::{AstNode, Item, SourceFile};
use compactp_syntax::{SyntaxNode, SyntaxToken};
use mn_core::types::SymbolSegment;

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
                symbol_path: symbol_path_for(&root, r.start, r.end),
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

/// Map a CST node to a symbol segment if it is a named Compact item.
/// Preamble items (pragma/include/import/export) contribute no segment.
fn item_segment(node: &SyntaxNode) -> Option<SymbolSegment> {
    let item = Item::cast(node.clone())?;
    let (kind, name) = match item {
        Item::ModuleDef(n) => ("module", token_text(n.name())),
        Item::LedgerDecl(n) => ("ledger", token_text(n.name())),
        Item::ConstructorDef(_) => ("constructor", String::new()),
        Item::CircuitDef(n) => ("circuit", token_text(n.name())),
        Item::CircuitDecl(n) => ("circuit", token_text(n.name())),
        Item::WitnessDecl(n) => ("witness", token_text(n.name())),
        Item::ContractDecl(n) => ("contract", token_text(n.name())),
        Item::StructDef(n) => ("struct", token_text(n.name())),
        Item::EnumDef(n) => ("enum", token_text(n.name())),
        Item::TypeDecl(n) => ("type", token_text(n.name())),
        Item::Pragma(_) | Item::Include(_) | Item::Import(_) | Item::ExportList(_) => {
            return None;
        }
    };
    Some(SymbolSegment { kind: kind.to_string(), name })
}

fn token_text(t: Option<SyntaxToken>) -> String {
    t.map(|t| t.text().to_string()).unwrap_or_default()
}

/// Build the symbol path enclosing `offset`: walk root → deepest child
/// containing `offset`, collecting a segment for each named item on the way.
fn symbol_path_at(root: &SyntaxNode, offset: usize) -> Vec<SymbolSegment> {
    let mut path = Vec::new();
    let mut node = root.clone();
    loop {
        if let Some(seg) = item_segment(&node) {
            path.push(seg);
        }
        let next = node.children().find(|c| {
            let r = c.text_range();
            usize::from(r.start()) <= offset && offset < usize::from(r.end())
        });
        match next {
            Some(c) => node = c,
            None => break,
        }
    }
    path
}

/// Segments for every top-level named item beginning in `[start, end)`, in
/// source order. Used to recover a path for a chunk that opens with preamble
/// and may span several sibling items (e.g. a whole-file single chunk).
fn top_level_symbols(root: &SyntaxNode, start: usize, end: usize) -> Vec<SymbolSegment> {
    root.children()
        .filter(|node| {
            let s = usize::from(node.text_range().start());
            s >= start && s < end
        })
        .filter_map(|node| item_segment(&node))
        .collect()
}

/// Symbol path for a chunk spanning `[start, end)`: the enclosing item at
/// `start`, or — when `start` falls in preamble — the named top-level items the
/// chunk covers.
///
/// Note the two branches carry different shapes: the in-body branch returns a
/// hierarchical enclosing path (root → leaf, e.g. `[module M, circuit brad]`),
/// while the preamble fallback returns a *flat covering set* of the top-level
/// siblings the chunk spans (e.g. `[ledger round, circuit increment]`), which
/// are not parent/child. Consume `symbol_path` as an unordered set of
/// `(kind, name)` symbols a chunk touches, not as a strict nested path.
fn symbol_path_for(root: &SyntaxNode, start: usize, end: usize) -> Vec<SymbolSegment> {
    let path = symbol_path_at(root, start);
    if !path.is_empty() {
        return path;
    }
    top_level_symbols(root, start, end)
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

    fn seg(chunks: &[Chunk], kind: &str, name: &str) -> bool {
        chunks.iter().any(|c| {
            c.symbol_path.iter().any(|s| s.kind == kind && s.name == name)
        })
    }

    #[test]
    fn top_level_circuit_and_ledger_symbol_paths() {
        let chunks = CompactChunker.chunk(COUNTER, &ChunkerConfig::default()).unwrap();
        assert!(seg(&chunks, "circuit", "increment"), "missing [circuit increment]: {:?}",
            chunks.iter().map(|c| &c.symbol_path).collect::<Vec<_>>());
        assert!(seg(&chunks, "ledger", "round"), "missing [ledger round]");
    }

    #[test]
    fn preamble_only_start_recovers_symbol() {
        // The whole file is one chunk; its start byte sits in `import` preamble,
        // so the path must be recovered from the first named item inside it.
        let chunks = CompactChunker.chunk(COUNTER, &ChunkerConfig::default()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(!chunks[0].symbol_path.is_empty(), "single-chunk file must record a symbol_path");
    }

    const MODULE_NEST: &str = "module M {\n  export circuit brad(a: Field): Field {\n    return a;\n  }\n}\n";

    #[test]
    fn module_nested_circuit_has_module_prefix() {
        let cfg = ChunkerConfig { max_tokens: 8, ..ChunkerConfig::default() };
        let chunks = CompactChunker.chunk(MODULE_NEST, &cfg).unwrap();
        // some chunk inside M carries both [module M] and [circuit brad]
        let nested = chunks.iter().any(|c| {
            c.symbol_path.iter().any(|s| s.kind == "module" && s.name == "M")
                && c.symbol_path.iter().any(|s| s.kind == "circuit" && s.name == "brad")
        });
        assert!(nested, "expected module-nested path: {:?}",
            chunks.iter().map(|c| &c.symbol_path).collect::<Vec<_>>());
    }
}

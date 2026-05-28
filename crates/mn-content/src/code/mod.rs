//! Code chunkers: tree-sitter + text-splitter per language, plus the shared
//! line-window fallback. Dispatch via [`chunker_for`] or [`chunker_for_ext`].

pub mod language;
pub mod line_window;
pub mod splitter;
pub mod symbols;

#[cfg(feature = "core-grammars")]
pub mod rust;

#[cfg(feature = "core-grammars")]
pub mod ts;

#[cfg(feature = "core-grammars")]
pub mod js;

#[cfg(feature = "scheme")]
pub mod scheme;

use crate::chunk::{Chunk, Chunker, ChunkerConfig};
use crate::code::symbols::{symbol_path_at, KindTable};
use language::Language;
use line_window::LineWindowChunker;

/// Shared tree-sitter driver used by all language chunkers.
///
/// Parses `body` with `language`, detects catastrophic parse errors, splits the
/// source into token-budgeted ranges via [`splitter::split_ranges`], and attaches
/// symbol paths via `table`. Falls back to [`LineWindowChunker`] when parsing is
/// unusable (set-language failure, parse failure, `has_error()` root with more
/// than 50% ERROR/MISSING bytes, splitter error, or empty result).
pub(crate) fn run_tree_sitter(
    body: &str,
    cfg: &ChunkerConfig,
    language: &tree_sitter::Language,
    table: KindTable,
) -> Result<Vec<Chunk>, crate::chunk::ChunkError> {
    use crate::chunk::Chunker as _;

    if body.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(language).is_err() {
        return LineWindowChunker.chunk(body, cfg);
    }
    let Some(tree) = parser.parse(body, None) else {
        return LineWindowChunker.chunk(body, cfg);
    };

    // Catastrophic-error heuristic: walk ALL descendants and sum bytes of ERROR
    // and MISSING nodes. If they exceed 50% of the source, fall back.
    // Direct-child-only counting misses deep recovery nodes; a full descendant
    // walk catches cases where tree-sitter recovers at the statement level but
    // most leaves are still ERROR/MISSING.
    let root = tree.root_node();
    if root.has_error() {
        let err_bytes: usize = count_error_bytes(root);
        if err_bytes * 2 > body.len() {
            return LineWindowChunker.chunk(body, cfg);
        }
    }

    let Ok(ranges) = crate::code::splitter::split_ranges(body, language, cfg) else {
        return LineWindowChunker.chunk(body, cfg);
    };
    let mut chunks = Vec::with_capacity(ranges.len());
    for (i, r) in ranges.into_iter().enumerate() {
        let content = body[r.clone()].to_string();
        if content.trim().is_empty() {
            continue;
        }
        chunks.push(Chunk {
            token_count: crate::tokens::count(&content),
            symbol_path: symbol_path_at(&tree, body, r.start, table),
            content,
            heading_path: Vec::new(),
            start_byte: r.start,
            end_byte: r.end,
            chunk_index: u32::try_from(i).unwrap_or(u32::MAX),
            fallback_used: false,
        });
    }
    if chunks.is_empty() {
        return LineWindowChunker.chunk(body, cfg);
    }
    Ok(chunks)
}

/// Recursively sum bytes of all ERROR and MISSING descendant nodes.
///
/// Uses a cursor-based traversal rather than recursion to avoid stack overflow
/// on deeply nested trees. Every node in the subtree rooted at `node` is
/// visited; ERROR and MISSING nodes contribute their byte span.
fn count_error_bytes(node: tree_sitter::Node<'_>) -> usize {
    let mut total = 0usize;
    let mut cursor = node.walk();
    loop {
        let n = cursor.node();
        if n.is_error() || n.is_missing() {
            total += n.end_byte() - n.start_byte();
        }
        // Descend into children first; if no child, go to next sibling; if
        // no sibling, walk back up until we find an unvisited sibling.
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return total;
            }
        }
    }
}

/// Return the best chunker for `lang`, using `ext` to disambiguate where needed
/// (e.g. `.tsx` selects the JSX TypeScript grammar).
///
/// Languages whose grammar feature is not compiled fall back to line-window
/// (graceful degradation). Per-language arms are added as each grammar lands
/// (Task 13+).
#[must_use]
pub fn chunker_for_ext(lang: Language, ext: &str) -> Box<dyn Chunker> {
    match lang {
        #[cfg(feature = "core-grammars")]
        Language::Rust => Box::new(rust::RustChunker),
        #[cfg(feature = "core-grammars")]
        Language::TypeScript => Box::new(ts::TypeScriptChunker {
            tsx: ext.eq_ignore_ascii_case("tsx"),
        }),
        #[cfg(feature = "core-grammars")]
        Language::JavaScript => Box::new(js::JavaScriptChunker),
        #[cfg(feature = "scheme")]
        Language::Scheme => Box::new(scheme::SchemeChunker),
        _ => Box::new(LineWindowChunker),
    }
}

/// Return the best chunker for `lang` (extension-agnostic).
///
/// Delegates to [`chunker_for_ext`] with an empty extension string.
#[must_use]
pub fn chunker_for(lang: Language) -> Box<dyn Chunker> {
    chunker_for_ext(lang, "")
}

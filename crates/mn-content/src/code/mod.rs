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

#[cfg(feature = "core-grammars")]
pub mod bash;

#[cfg(feature = "scheme")]
pub mod scheme;

#[cfg(feature = "compact")]
pub mod compact;

#[cfg(feature = "extended-grammars")]
pub mod go;

#[cfg(feature = "extended-grammars")]
pub mod python;

#[cfg(feature = "extended-grammars")]
pub mod solidity;

#[cfg(feature = "markup-grammars")]
pub mod toml;

#[cfg(feature = "markup-grammars")]
pub mod yaml;

#[cfg(feature = "markup-grammars")]
pub mod html;

#[cfg(feature = "markup-grammars")]
pub mod xml;

#[cfg(feature = "all-grammars")]
pub mod swift;

#[cfg(feature = "all-grammars")]
pub mod ruby;

#[cfg(feature = "all-grammars")]
pub mod kotlin;

#[cfg(feature = "all-grammars")]
pub mod csharp;

#[cfg(feature = "all-grammars")]
pub mod haskell;

#[cfg(feature = "all-grammars")]
pub mod java;

use crate::chunk::{Chunk, Chunker, ChunkerConfig};
use crate::code::symbols::{symbol_path_at, KindTable};
use language::Language;
use line_window::LineWindowChunker;
use mn_core::types::SymbolSegment;

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
    line_comment: &'static str,
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
    for r in ranges {
        let content = body[r.clone()].to_string();
        if content.trim().is_empty() {
            continue;
        }
        // Prefer the symbol path enclosing the chunk's first byte. If that is
        // empty — the chunk opens with file-level preamble (doc comments,
        // `use`/`import`, attributes) before any named item — retry at the
        // first in-table node contained in the chunk, so small single-chunk
        // files still record the symbol they contain.
        let mut symbol_path = entries_from_path(&symbol_path_at(&tree, body, r.start, table));
        if symbol_path.is_empty() {
            if let Some(off) = symbols::first_symbol_start(&tree, r.start, r.end, table) {
                symbol_path = entries_from_path(&symbol_path_at(&tree, body, off, table));
            }
        }
        chunks.push(Chunk {
            token_count: crate::tokens::count(&content),
            symbol_path,
            content,
            heading_path: Vec::new(),
            start_byte: r.start,
            end_byte: r.end,
            chunk_index: 0,
            fallback_used: false,
        });
    }
    if chunks.is_empty() {
        return LineWindowChunker.chunk(body, cfg);
    }

    // Pass A: greedily pack adjacent chunks up to 90% of the token budget.
    let mut chunks = coalesce_code(body, &chunks, cfg);

    // Pass B: prepend an enclosing-symbol breadcrumb to interior chunks of a
    // split symbol (those that do not open the symbol they sit inside). Skipped
    // for languages without a line comment (`line_comment == ""`).
    if !line_comment.is_empty() {
        for c in &mut chunks {
            let headers = symbols::enclosing_symbol_headers(&tree, body, c.start_byte, table);
            let interior: Vec<&str> = headers
                .iter()
                .filter(|(node_start, _)| *node_start < c.start_byte)
                .map(|(_, line)| line.as_str())
                .filter(|l| !l.is_empty())
                .collect();
            if interior.is_empty() {
                continue;
            }
            // The breadcrumb intentionally makes `content` longer than
            // `body[start..end]` (byte range is left pointing at the real slice;
            // downstream dedup only trims byte-aligned content). It may also nudge
            // an already-budget-sized interior chunk a little past `max_tokens` —
            // `max_tokens` is a soft budget for code, and the embedder tolerates
            // it. Do not "fix" either by re-syncing bytes or re-splitting here.
            let crumb = format!("{} {} … (continued)\n", line_comment, interior.join(" > "));
            c.content = format!("{crumb}{}", c.content);
            c.token_count = crate::tokens::count(&c.content);
        }
    }

    // Assign sequential chunk indices after all transforms.
    for (i, c) in chunks.iter_mut().enumerate() {
        c.chunk_index = u32::try_from(i).unwrap_or(u32::MAX);
    }
    Ok(chunks)
}

/// Convert a root→leaf ancestor path into the flat entry list stored on the
/// chunk: one entry per symbol (ancestors included, so scope-level facet
/// containment keeps matching), each carrying its own ancestor names.
fn entries_from_path(path: &[SymbolSegment]) -> Vec<SymbolSegment> {
    let mut ancestors: Vec<String> = Vec::new();
    let mut out = Vec::with_capacity(path.len());
    for seg in path {
        out.push(SymbolSegment {
            kind: seg.kind.clone(),
            name: seg.name.clone(),
            path: ancestors.clone(),
        });
        ancestors.push(seg.name.clone());
    }
    out
}

/// Greedy coalescing for code chunks (D2): pack adjacent chunks while the
/// merged run stays within `coalesce_target` (90% of `max_tokens`). Unlike the
/// pre-dual-embeddings version this merges across unrelated top-level symbols —
/// `symbol_path` carries the union of every merged chunk's entries (§5.3).
pub(crate) fn coalesce_code(body: &str, chunks: &[Chunk], cfg: &ChunkerConfig) -> Vec<Chunk> {
    let target = crate::chunk::coalesce_target(cfg);
    let mut out: Vec<Chunk> = Vec::new();
    let mut i = 0usize;
    while i < chunks.len() {
        let mut end = i;
        while end + 1 < chunks.len() {
            let next = &chunks[end + 1];
            if crate::tokens::count(&body[chunks[i].start_byte..next.end_byte]) > target {
                break;
            }
            end += 1;
        }
        if end == i {
            out.push(chunks[i].clone());
        } else {
            let start_byte = chunks[i].start_byte;
            let end_byte = chunks[end].end_byte;
            let content = body[start_byte..end_byte].to_string();
            let mut symbol_path: Vec<SymbolSegment> = Vec::new();
            for c in &chunks[i..=end] {
                for e in &c.symbol_path {
                    if !symbol_path.contains(e) {
                        symbol_path.push(e.clone());
                    }
                }
            }
            out.push(Chunk {
                token_count: crate::tokens::count(&content),
                content,
                symbol_path,
                heading_path: Vec::new(),
                start_byte,
                end_byte,
                chunk_index: 0,
                fallback_used: false,
            });
        }
        i = end + 1;
    }
    out
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
        #[cfg(feature = "core-grammars")]
        Language::Bash => Box::new(bash::BashChunker),
        #[cfg(feature = "scheme")]
        Language::Scheme => Box::new(scheme::SchemeChunker),
        #[cfg(feature = "extended-grammars")]
        Language::Go => Box::new(go::GoChunker),
        #[cfg(feature = "extended-grammars")]
        Language::Python => Box::new(python::PythonChunker),
        #[cfg(feature = "extended-grammars")]
        Language::Solidity => Box::new(solidity::SolidityChunker),
        #[cfg(feature = "markup-grammars")]
        Language::Toml => Box::new(toml::TomlChunker),
        #[cfg(feature = "markup-grammars")]
        Language::Yaml => Box::new(yaml::YamlChunker),
        #[cfg(feature = "markup-grammars")]
        Language::Html => Box::new(html::HtmlChunker),
        #[cfg(feature = "markup-grammars")]
        Language::Xml => Box::new(xml::XmlChunker),
        #[cfg(feature = "all-grammars")]
        Language::Swift => Box::new(swift::SwiftChunker),
        #[cfg(feature = "all-grammars")]
        Language::Ruby => Box::new(ruby::RubyChunker),
        #[cfg(feature = "all-grammars")]
        Language::Kotlin => Box::new(kotlin::KotlinChunker),
        #[cfg(feature = "all-grammars")]
        Language::CSharp => Box::new(csharp::CSharpChunker),
        #[cfg(feature = "all-grammars")]
        Language::Haskell => Box::new(haskell::HaskellChunker),
        #[cfg(feature = "all-grammars")]
        Language::Java => Box::new(java::JavaChunker),
        #[cfg(feature = "compact")]
        Language::Compact => Box::new(compact::CompactChunker),
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

#[cfg(test)]
mod coalesce_tests {
    use super::*;
    use mn_core::types::SymbolSegment;

    fn seg(kind: &str, name: &str) -> SymbolSegment {
        SymbolSegment {
            kind: kind.into(),
            name: name.into(),
            path: Vec::new(),
        }
    }
    fn chunk(start: usize, end: usize, path: Vec<SymbolSegment>) -> Chunk {
        Chunk {
            content: String::new(),
            heading_path: Vec::new(),
            symbol_path: path,
            start_byte: start,
            end_byte: end,
            token_count: 0,
            chunk_index: 0,
            fallback_used: false,
        }
    }

    #[test]
    fn entries_from_path_emits_ancestors_and_leaf() {
        let raw = vec![seg("mod", "m"), seg("impl", "Foo"), seg("fn", "bar")];
        let entries = entries_from_path(&raw);
        assert_eq!(entries.len(), 3);
        assert_eq!((entries[0].kind.as_str(), entries[0].name.as_str()), ("mod", "m"));
        assert!(entries[0].path.is_empty());
        assert_eq!(entries[1].path, vec!["m".to_string()]);
        assert_eq!(entries[2].path, vec!["m".to_string(), "Foo".to_string()]);
    }

    #[test]
    fn packs_adjacent_units_up_to_target() {
        // Two tiny adjacent symbols pack into one chunk (greedy-to-90%),
        // regardless of shared scope — distinct top-level symbols now merge.
        let body = "fn a() {} fn b() {}".to_string();
        let cfg = ChunkerConfig::default();
        let chunks = vec![
            chunk(0, 9, vec![seg("fn", "a")]),
            chunk(9, body.len(), vec![seg("fn", "b")]),
        ];
        let out = coalesce_code(&body, &chunks, &cfg);
        assert_eq!(out.len(), 1, "tiny adjacent top-level symbols must pack");
        assert_eq!(out[0].start_byte, 0);
        assert_eq!(out[0].end_byte, body.len());
    }

    #[test]
    fn merged_chunk_unions_symbol_entries() {
        let body = "fn a() {} fn b() {}".to_string();
        let cfg = ChunkerConfig::default();
        let chunks = vec![
            chunk(0, 9, vec![seg("fn", "a")]),
            chunk(9, body.len(), vec![seg("fn", "b")]),
        ];
        let out = coalesce_code(&body, &chunks, &cfg);
        assert_eq!(out[0].symbol_path, vec![seg("fn", "a"), seg("fn", "b")]);
    }

    #[test]
    fn stops_when_next_unit_would_exceed_target() {
        // Two halves of ~250 tokens each against a 256-token budget (target ≈230):
        // the first half alone already exceeds the target, so no merge happens.
        let body = "word ".repeat(500);
        let cfg = ChunkerConfig {
            max_tokens: 256,
            ..ChunkerConfig::default()
        };
        let mid = body.len() / 2;
        let first = crate::tokens::count(&body[0..mid]);
        assert!(first > crate::chunk::coalesce_target(&cfg), "precondition (got {first})");
        let chunks = vec![
            chunk(0, mid, vec![seg("fn", "a")]),
            chunk(mid, body.len(), vec![seg("fn", "b")]),
        ];
        let out = coalesce_code(&body, &chunks, &cfg);
        assert_eq!(out.len(), 2);
    }
}

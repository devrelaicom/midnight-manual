//! Compact chunker: compactp (rowan CST) + token budgeting + symbol paths.
//!
//! compactp is rowan-based, so this is a self-contained walker behind the
//! shared [`Chunker`] trait — parallel to the Markdown chunker, not the
//! tree-sitter language chunkers. Falls back to line-window on a catastrophic
//! parse.

use std::ops::Range;

use compactp_ast::{AstNode, Item, SourceFile};
use compactp_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use mn_core::types::{PackageRef, SymbolSegment};

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

        if error_bytes(&root) * 2 > body.len() {
            return LineWindowChunker.chunk(body, cfg);
        }

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
            chunks.push(Chunk {
                token_count: crate::tokens::count(&content),
                symbol_path: symbol_path_for(&root, r.start, r.end),
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

        // Pass A: fold tiny same-scope fragments (shared with the tree-sitter path).
        let mut chunks = crate::code::coalesce_code(body, &chunks, cfg);

        // Pass B: prepend an enclosing-symbol breadcrumb to interior chunks of a
        // split symbol (those that do not open the symbol they sit inside).
        for c in &mut chunks {
            let headers = enclosing_symbol_headers(&root, body, c.start_byte);
            let interior: Vec<&str> = headers
                .iter()
                .filter(|(node_start, _)| *node_start < c.start_byte)
                .map(|(_, line)| line.as_str())
                .filter(|l| !l.is_empty())
                .collect();
            if interior.is_empty() {
                continue;
            }
            // Intentional content/byte divergence (same as the tree-sitter path):
            // the breadcrumb makes `content` longer than `body[start..end]` and may
            // nudge an interior chunk slightly past `max_tokens` (soft budget for code).
            let crumb = format!("// {} … (continued)\n", interior.join(" > "));
            c.content = format!("{crumb}{}", c.content);
            c.token_count = crate::tokens::count(&c.content);
        }

        // Renumber after all transforms.
        for (i, c) in chunks.iter_mut().enumerate() {
            c.chunk_index = u32::try_from(i).unwrap_or(u32::MAX);
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

/// Total bytes covered by `ERROR` regions — interior ERROR *nodes* (parser-level
/// recovery wrapping otherwise-valid tokens) and leaf ERROR *tokens* (lexer-level
/// unknown input). Summing the *outermost* ERROR element of each subtree (never
/// descending into an ERROR node) counts each garbage region once, so an ERROR
/// node and the tokens nested inside it are not double-counted.
fn error_bytes(root: &SyntaxNode) -> usize {
    let mut total = 0;
    for el in root.children_with_tokens() {
        if let Some(n) = el.as_node() {
            if n.kind() == SyntaxKind::ERROR {
                let r = n.text_range();
                total += usize::from(r.end()) - usize::from(r.start());
            } else {
                total += error_bytes(n);
            }
        } else if let Some(t) = el.as_token() {
            if t.kind() == SyntaxKind::ERROR {
                let r = t.text_range();
                total += usize::from(r.end()) - usize::from(r.start());
            }
        }
    }
    total
}

/// Detect the Compact package for a file: a single top-level `module <Name>`.
///
/// Zero modules → `None` (the common case for application contracts). Multiple
/// top-level modules → `None` with a debug log (per-chunk multi-module tagging
/// is deferred; see the design doc, Decision 4 / P1).
#[must_use]
pub fn detect_module_package(body: &str) -> Option<PackageRef> {
    if body.trim().is_empty() {
        return None;
    }
    let parsed = compactp_parser::parse(body);
    let root = SyntaxNode::new_root(parsed.green);
    let file = SourceFile::cast(root)?;
    let mut names = file.items().filter_map(|item| match item {
        Item::ModuleDef(m) => {
            let n = token_text(m.name());
            if n.is_empty() {
                None
            } else {
                Some(n)
            }
        }
        _ => None,
    });
    let first = names.next()?;
    if names.next().is_some() {
        tracing::debug!(
            "compact file declares multiple top-level modules; package left untagged (P1)"
        );
        return None;
    }
    Some(PackageRef {
        kind: "compact".to_string(),
        name: first,
        version: None,
        manifest_path: None,
    })
}

/// Extract the `pragma language_version <expr>;` constraint (spec §1.1).
///
/// Only the `language_version` pragma is read — the legacy `pragma compact X`
/// form states a compiler version. The expression is normalized (whitespace
/// stripped, `&&` → `,`) and must parse as a `semver::VersionReq`, else `None`
/// (warn-and-skip, never fatal).
#[must_use]
pub fn detect_language_version(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return None;
    }
    let parsed = compactp_parser::parse(body);
    let root = SyntaxNode::new_root(parsed.green);
    let file = SourceFile::cast(root)?;
    for pragma in file.pragmas() {
        let Some(name) = pragma.name() else { continue };
        if name.text() != "language_version" {
            continue;
        }
        let full = pragma.syntax().text().to_string();
        let expr = full
            .trim_start()
            .strip_prefix("pragma")?
            .trim_start()
            .strip_prefix("language_version")?
            .trim()
            .trim_end_matches(';')
            .trim()
            .replace("&&", ",");
        let normalized: String = expr.split_whitespace().collect::<Vec<_>>().join("");
        if normalized.is_empty() || semver::VersionReq::parse(&normalized).is_err() {
            tracing::warn!(expr = %expr, "unparseable language_version pragma; skipping extraction");
            return None;
        }
        return Some(normalized);
    }
    None
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
    Some(SymbolSegment {
        kind: kind.to_string(),
        name,
        path: Vec::new(),
    })
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

/// Enclosing named-item headers for the item at `offset`, outermost first.
/// Each entry is `(header_start_byte, first_line)` with the line trimmed and a
/// trailing `{` removed — the Compact-CST analogue of the tree-sitter
/// `enclosing_symbol_headers`. Lets the breadcrumb pass tell which items a chunk
/// is *inside* (`header_start < chunk start`) from the one it *opens* (`==`).
///
/// Unlike tree-sitter, rowan attaches leading whitespace/newline trivia to the
/// FRONT of an item node, so `text_range().start()` points at that trivia, not
/// the item's first real token. We advance past the leading whitespace before
/// reading the header line, so the recorded byte and the captured line both
/// reflect where the item's signature actually begins.
fn enclosing_symbol_headers(root: &SyntaxNode, body: &str, offset: usize) -> Vec<(usize, String)> {
    let mut headers = Vec::new();
    let mut node = root.clone();
    loop {
        if item_segment(&node).is_some() {
            let raw_start = usize::from(node.text_range().start());
            let lead = body[raw_start..]
                .find(|ch: char| !ch.is_whitespace())
                .unwrap_or(0);
            let start = raw_start + lead;
            let line_end = body[start..]
                .find('\n')
                .map_or(body.len(), |off| start + off);
            let first_line = body
                .get(start..line_end)
                .unwrap_or_default()
                .trim()
                .trim_end_matches('{')
                .trim_end()
                .to_string();
            headers.push((start, first_line));
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
    headers
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

    const TWO_MODULES: &str =
        "module A {\n  export ledger a: Field;\n}\n\nmodule B {\n  export ledger b: Field;\n}\n";

    #[test]
    fn one_module_detected_as_package() {
        let src = "module M {\n  export ledger b: Field;\n}\n";
        let pkg = detect_module_package(src).expect("one module → package");
        assert_eq!(pkg.kind, "compact");
        assert_eq!(pkg.name, "M");
        assert_eq!(pkg.manifest_path, None);
    }

    #[test]
    fn no_module_is_none() {
        assert!(detect_module_package(COUNTER).is_none());
    }

    #[test]
    fn multiple_modules_is_none() {
        assert!(detect_module_package(TWO_MODULES).is_none());
    }

    #[test]
    fn language_version_pragma_extracted() {
        let body = "pragma language_version >= 0.23;\nledger x: Uint<8>;\n";
        assert_eq!(detect_language_version(body).as_deref(), Some(">=0.23"));
        // legacy compiler pragma is NOT extracted (spec §1.1)
        assert_eq!(detect_language_version("pragma compact 0.15.0;\n"), None);
        // conjunction normalizes && → comma
        let body = "pragma language_version >= 0.13 && <= 0.17;\n";
        assert_eq!(detect_language_version(body).as_deref(), Some(">=0.13,<=0.17"));
        // garbage expr (won't parse as VersionReq) → None
        assert_eq!(detect_language_version("pragma language_version banana;\n"), None);
    }

    #[test]
    fn parses_and_emits_a_chunk() {
        let chunks = CompactChunker
            .chunk(COUNTER, &ChunkerConfig::default())
            .unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| !c.fallback_used));
        // chunks reconstruct the bytes they claim (a breadcrumb may prefix them)
        for c in &chunks {
            assert!(
                c.content.ends_with(&COUNTER[c.start_byte..c.end_byte]),
                "chunk content must end with its source slice"
            );
        }
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(CompactChunker
            .chunk("   \n\t", &ChunkerConfig::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn garbage_falls_back_to_line_window() {
        // Non-Compact junk: each non-ASCII glyph lexes to an ERROR token, so
        // error bytes dominate → catastrophic fallback.
        let src = "🔥🔥🔥 ❌❌❌ ¡¡¡¡ §§§§ ".repeat(60);
        let chunks = CompactChunker
            .chunk(&src, &ChunkerConfig::default())
            .unwrap();
        assert!(chunks.iter().any(|c| c.fallback_used), "garbage must fall back");
    }

    #[test]
    fn token_soup_falls_back_to_line_window() {
        // Valid tokens in nonsense order: the parser wraps them in ERROR *nodes*
        // (not ERROR tokens), so this exercises the parser-recovery path that the
        // emoji fixture (lexer ERROR tokens) does not.
        let src = "foo bar baz 123 qux wibble wobble 456 zzz plugh ".repeat(40);
        let chunks = CompactChunker
            .chunk(&src, &ChunkerConfig::default())
            .unwrap();
        assert!(chunks.iter().any(|c| c.fallback_used), "token-soup garbage must fall back");
    }

    #[test]
    fn valid_compact_does_not_fall_back() {
        let chunks = CompactChunker
            .chunk(COUNTER, &ChunkerConfig::default())
            .unwrap();
        assert!(chunks.iter().all(|c| !c.fallback_used));
    }

    #[test]
    fn small_siblings_pack_into_one_chunk() {
        // Whole file fits the default 400-token budget → a single chunk.
        let chunks = CompactChunker
            .chunk(COUNTER, &ChunkerConfig::default())
            .unwrap();
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn tiny_budget_splits_into_multiple_chunks() {
        let cfg = ChunkerConfig {
            max_tokens: 8,
            ..ChunkerConfig::default()
        };
        let chunks = CompactChunker.chunk(COUNTER, &cfg).unwrap();
        assert!(chunks.len() >= 2, "tiny budget should split: got {}", chunks.len());
        // sorted + non-overlapping
        for w in chunks.windows(2) {
            assert!(w[0].end_byte <= w[1].start_byte);
        }
        // byte-accurate (a breadcrumb may prefix the content)
        for c in &chunks {
            assert!(
                c.content.ends_with(&COUNTER[c.start_byte..c.end_byte]),
                "chunk content must end with its source slice"
            );
        }
        assert!(chunks.iter().all(|c| !c.fallback_used));
    }

    fn seg(chunks: &[Chunk], kind: &str, name: &str) -> bool {
        chunks.iter().any(|c| {
            c.symbol_path
                .iter()
                .any(|s| s.kind == kind && s.name == name)
        })
    }

    #[test]
    fn top_level_circuit_and_ledger_symbol_paths() {
        let chunks = CompactChunker
            .chunk(COUNTER, &ChunkerConfig::default())
            .unwrap();
        assert!(
            seg(&chunks, "circuit", "increment"),
            "missing [circuit increment]: {:?}",
            chunks.iter().map(|c| &c.symbol_path).collect::<Vec<_>>()
        );
        assert!(seg(&chunks, "ledger", "round"), "missing [ledger round]");
    }

    #[test]
    fn preamble_only_start_recovers_symbol() {
        // The whole file is one chunk; its start byte sits in `import` preamble,
        // so the path must be recovered from the first named item inside it.
        let chunks = CompactChunker
            .chunk(COUNTER, &ChunkerConfig::default())
            .unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(!chunks[0].symbol_path.is_empty(), "single-chunk file must record a symbol_path");
    }

    const MODULE_NEST: &str =
        "module M {\n  export circuit brad(a: Field): Field {\n    return a;\n  }\n}\n";

    #[test]
    fn module_nested_circuit_has_module_prefix() {
        let cfg = ChunkerConfig {
            max_tokens: 8,
            ..ChunkerConfig::default()
        };
        let chunks = CompactChunker.chunk(MODULE_NEST, &cfg).unwrap();
        // some chunk inside M carries both [module M] and [circuit brad]
        let nested = chunks.iter().any(|c| {
            c.symbol_path
                .iter()
                .any(|s| s.kind == "module" && s.name == "M")
                && c.symbol_path
                    .iter()
                    .any(|s| s.kind == "circuit" && s.name == "brad")
        });
        assert!(
            nested,
            "expected module-nested path: {:?}",
            chunks.iter().map(|c| &c.symbol_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn split_compact_symbol_interior_chunk_gets_breadcrumb() {
        // Tiny budget splits the nested circuit body across chunks; an interior
        // chunk must carry the enclosing module/circuit signature as a `//`
        // breadcrumb (Compact gets the same wrapper-context treatment as the
        // tree-sitter languages).
        let cfg = ChunkerConfig {
            max_tokens: 8,
            ..ChunkerConfig::default()
        };
        let chunks = CompactChunker.chunk(MODULE_NEST, &cfg).unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.content.starts_with("//") && c.content.contains("circuit brad")),
            "an interior chunk should carry the wrapper breadcrumb: {:#?}",
            chunks
                .iter()
                .map(|c| c.content.lines().next().unwrap_or(""))
                .collect::<Vec<_>>()
        );
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn ranges_tile_and_cover_non_whitespace(n in 1usize..10, budget in 6u32..40) {
            // Build a valid multi-circuit file from a proven-good template.
            use std::fmt::Write as _;
            let mut src = String::new();
            for i in 0..n {
                let _ = write!(src, "export circuit c{i}(a: Field): Field {{\n  return a;\n}}\n\n");
            }
            let cfg = ChunkerConfig { max_tokens: budget, ..ChunkerConfig::default() };
            let chunks = CompactChunker.chunk(&src, &cfg).unwrap();
            prop_assume!(chunks.iter().all(|c| !c.fallback_used));

            // sorted + non-overlapping
            for w in chunks.windows(2) {
                prop_assert!(w[0].end_byte <= w[1].start_byte);
            }
            // byte-accurate (a breadcrumb may prefix the content)
            for c in &chunks {
                prop_assert!(c.content.ends_with(&src[c.start_byte..c.end_byte]));
            }
            // every non-whitespace byte is covered by some chunk
            for (i, b) in src.bytes().enumerate() {
                if !b.is_ascii_whitespace() {
                    let covered = chunks.iter().any(|c| c.start_byte <= i && i < c.end_byte);
                    prop_assert!(covered, "byte {} not covered", i);
                }
            }
        }
    }
}

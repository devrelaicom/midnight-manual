# Compact Chunker (compactp integration) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `.compact` files real symbol-aware, token-budgeted chunking plus single-module package detection, by integrating the `compactp` parser as a default-on, opt-out-able library feature in `mn-content`.

**Architecture:** `compactp` is a `rowan`-based parser (not tree-sitter), so the Compact chunker is a self-contained walker in `crates/mn-content/src/code/compact.rs` behind the `Chunker` trait — parallel to `markdown.rs`, not to the tree-sitter `rust.rs`. It parses with `compactp_parser::parse`, splits the lossless CST with a uniform recursive largest-fit packer (descending into modules/circuits, line-window only as a whole-file fallback), attaches a `symbol_path` per chunk, and detects a single top-level `module <Name>` as a `PackageKind::Compact` package via the existing per-document package model (no schema change). The feature is default-on in `mn-content` and `mn-cli`, excluded with `cargo build -p mn-cli --no-default-features`.

**Tech Stack:** Rust (edition 2024, MSRV 1.91), `compactp_parser` + `compactp_ast` `=0.1.0-beta.1` (crates.io), `rowan`, existing `mn-content` chunker framework, `proptest`.

**Design doc:** `docs/superpowers/specs/2026-06-04-compact-chunker-design.md`

---

## Background the implementer needs

- **The `Chunker` contract** (`crates/mn-content/src/chunk.rs`): `fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError>`. Empty/whitespace input → `Ok(vec![])`; otherwise ≥1 chunk. `Chunk { content, heading_path, symbol_path, start_byte, end_byte, token_count, chunk_index, fallback_used }`. `ChunkerConfig { max_tokens (default 400), fallback_lines, fallback_overlap_lines, max_file_bytes }`.
- **Fallback chunker**: `crate::code::line_window::LineWindowChunker` (unit struct) implements `Chunker` and sets `fallback_used = true`. Call `LineWindowChunker.chunk(body, cfg)`.
- **Token counting**: `crate::tokens::count(text: &str) -> u32` (BPE; the same tokenizer the whole pipeline uses).
- **`mn_core::types::SymbolSegment { kind: String, name: String }`** and **`PackageRef { kind: String, name: String, manifest_path: Option<String> }`** (`PackageKind::Compact` already exists; the DB `package.kind` CHECK already allows `'compact'`).
- **compactp library API**:
  - `compactp_parser::parse(src: &str) -> ParseResult { green: GreenNode, errors: Vec<Diagnostic> }` — always returns a full-coverage CST (recovers losslessly).
  - `compactp_syntax::SyntaxNode::new_root(green) -> SyntaxNode`; `SyntaxKind::ERROR`; standard rowan APIs `text_range()`, `kind()`, `children()`, `children_with_tokens()`, `descendants()`, `descendants_with_tokens()`. Convert offsets with `usize::from(range.start())`.
  - `compactp_ast::{AstNode, Item, SourceFile}`; `SourceFile::cast(node) -> Option<SourceFile>`; `SourceFile::items() -> impl Iterator<Item = Item>` (top-level only); each named item exposes `.name() -> Option<SyntaxToken>` (`SyntaxToken::text() -> &str`). `Item` variants: `Pragma, Include, Import, ExportList, LedgerDecl, ConstructorDef, CircuitDef, CircuitDecl, WitnessDecl, ContractDecl, StructDef, EnumDef, ModuleDef, TypeDecl`. **`ConstructorDef` has no `name()`** (constructors are anonymous).
- **CI gate**: `cargo clippy --workspace --all-targets -- -D warnings`. Keep all new code clippy-clean (no `unwrap`/`expect` outside `#[cfg(test)]`; the workspace lints deny them in non-test code per the constitution).
- **Compact syntax warning**: do NOT write Compact from memory. All fixtures in this plan are copied verbatim from compactp's own test corpus (`counter.compact`, `tiny.compact`, `module_wpp.compact`) and are known to parse. Tests assert `!fallback_used` so any bad fixture fails loudly.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `Cargo.toml` (workspace) | declare `compactp_parser` / `compactp_ast` | modify (`[workspace.dependencies]`, ~line 76) |
| `crates/mn-content/Cargo.toml` | optional deps + `compact` feature (default-on) | modify |
| `crates/mn-content/src/code/mod.rs` | feature-gated `mod compact;` + dispatch arm | modify |
| `crates/mn-content/src/code/compact.rs` | the Compact chunker + module package detector | **create** |
| `crates/mn-content/src/lib.rs` | always-present `detect_compact_package` wrapper + doc fix | modify |
| `crates/mn-cli/Cargo.toml` | `compact` passthrough feature + opt-out wiring | modify |
| `crates/mn-cli/src/commands/ingest/run.rs` | route `.compact` to content-based package detection | modify (~line 391, ~1129) |
| `crates/mn-content/tests/fixtures/compact/*.compact` | real fixtures | **create** |
| `crates/mn-content/tests/compact_corpus.rs` | acceptance test (SC-028 surrogate) | **create** |
| `README.md` | grammar-tier paragraph + opt-out | modify |
| `specs/001-rag-platform/spec.md` | FR-047 note | modify |

---

## Task 1: Dependencies, feature flag, dispatch, minimal chunker

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies]`)
- Modify: `crates/mn-content/Cargo.toml`
- Modify: `crates/mn-content/src/code/mod.rs`
- Create: `crates/mn-content/src/code/compact.rs`

- [ ] **Step 1: Add the workspace dependencies**

In `Cargo.toml`, inside `[workspace.dependencies]` (the block starting ~line 76), add (after the tree-sitter grammar block is fine):

```toml
# Compact parser frontend (rowan-based, pure Rust — no native build step).
# Optional + feature-gated in mn-content. Exact pin: pre-release (beta) API.
compactp_parser = "=0.1.0-beta.1"
compactp_ast    = "=0.1.0-beta.1"
```

- [ ] **Step 2: Wire the feature + optional deps in mn-content**

In `crates/mn-content/Cargo.toml`, change the default and add the feature. Replace:

```toml
default = ["core-grammars"]
```

with:

```toml
default = ["core-grammars", "compact"]
```

Add to the `[features]` block (after `all-grammars`):

```toml
# Compact chunking via the compactp library (experimental; default-on).
# Build without it: `cargo build -p mn-cli --no-default-features`.
compact = ["dep:compactp_parser", "dep:compactp_ast"]
```

Add to `[dependencies]` (next to the tree-sitter optional deps):

```toml
compactp_parser       = { workspace = true, optional = true }
compactp_ast          = { workspace = true, optional = true }
```

- [ ] **Step 3: Write the failing test (create `compact.rs` with the test only)**

Create `crates/mn-content/src/code/compact.rs`:

```rust
//! Compact chunker: compactp (rowan CST) + token budgeting + symbol paths.
//!
//! compactp is rowan-based, so this is a self-contained walker behind the
//! shared [`Chunker`] trait — parallel to the Markdown chunker, not the
//! tree-sitter language chunkers. Falls back to line-window on a catastrophic
//! parse.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};

/// Compact code chunker backed by the `compactp` parser.
pub struct CompactChunker;

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
```

- [ ] **Step 4: Register the module + dispatch arm in `code/mod.rs`**

In `crates/mn-content/src/code/mod.rs`, add the module declaration near the other feature-gated `pub mod` lines:

```rust
#[cfg(feature = "compact")]
pub mod compact;
```

In `chunker_for_ext`, add this arm immediately before the final `_ => Box::new(LineWindowChunker)` arm:

```rust
        #[cfg(feature = "compact")]
        Language::Compact => Box::new(compact::CompactChunker),
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test -p mn-content compact:: 2>&1 | tail -20`
Expected: FAIL — `CompactChunker` has no `Chunker` impl (`the trait bound CompactChunker: Chunker is not satisfied`) / method `chunk` not found.

- [ ] **Step 6: Implement the minimal chunker**

In `crates/mn-content/src/code/compact.rs`, add (above the `#[cfg(test)]` module):

```rust
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
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p mn-content compact:: 2>&1 | tail -20`
Expected: PASS (both tests).

- [ ] **Step 8: Confirm the no-feature build still compiles**

Run: `cargo build -p mn-content --no-default-features --features core-grammars 2>&1 | tail -5`
Expected: builds; `compact.rs` is absent, `Language::Compact` falls through to line-window.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/mn-content/Cargo.toml crates/mn-content/src/code/mod.rs crates/mn-content/src/code/compact.rs
git commit -m "feat(content): scaffold Compact chunker behind the compact feature"
```

---

## Task 2: Recursive token-budgeted splitter

Replaces the whole-file single chunk with a uniform recursive largest-fit packer: pack adjacent CST children up to `max_tokens`, recurse into any child *node* that alone exceeds budget, absorb inter-child trivia so ranges tile the source.

**Files:**
- Modify: `crates/mn-content/src/code/compact.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `compact.rs`:

```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content compact:: 2>&1 | tail -20`
Expected: FAIL — `tiny_budget_splits_into_multiple_chunks` gets 1 chunk (minimal impl ignores budget).

- [ ] **Step 3: Add the splitter and rewrite `chunk` to use it**

In `compact.rs`, add imports at the top:

```rust
use std::ops::Range;

use compactp_ast::SourceFile;
```

Add the splitter function (below the `impl Chunker`):

```rust
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
                    run_tokens = 0;
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
```

Replace the body of `impl Chunker for CompactChunker`'s `chunk` (from the `// Minimal:` comment to the end) with:

```rust
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
```

Update the top imports to add the fallback chunker, and remove the now-unused `parsed`/`_root` minimal code. Ensure these `use`s are present at the top of the file:

```rust
use crate::code::line_window::LineWindowChunker;
```

And keep `use compactp_syntax::SyntaxNode;`. The line `let parsed = compactp_parser::parse(body);` stays (now feeds `SyntaxNode::new_root(parsed.green)`).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content compact:: 2>&1 | tail -20`
Expected: PASS (all four tests).

- [ ] **Step 5: Clippy clean**

Run: `cargo clippy -p mn-content --all-targets -- -D warnings 2>&1 | tail -15`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-content/src/code/compact.rs
git commit -m "feat(content): uniform recursive largest-fit splitting for Compact"
```

---

## Task 3: Symbol-path attribution

Attach a `symbol_path` to each chunk: the enclosing named item at the chunk's start byte, falling back to the first named item contained in the chunk when the start lands in preamble (pragma/import/export).

**Files:**
- Modify: `crates/mn-content/src/code/compact.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    fn seg<'a>(chunks: &'a [Chunk], kind: &str, name: &str) -> bool {
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content compact:: 2>&1 | tail -25`
Expected: FAIL — symbol paths are empty (`symbol_path: Vec::new()`).

- [ ] **Step 3: Implement symbol-path extraction**

Add imports at the top of `compact.rs`:

```rust
use compactp_ast::{AstNode, Item};
use compactp_syntax::SyntaxToken;
use mn_core::types::SymbolSegment;
```

Add these functions (below `split_node`):

```rust
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

/// Byte offset of the first named item beginning in `[start, end)`, in source
/// order. Used to recover a path for a chunk that opens with preamble.
fn first_symbol_start(root: &SyntaxNode, start: usize, end: usize) -> Option<usize> {
    root.descendants().find_map(|node| {
        let s = usize::from(node.text_range().start());
        if s >= start && s < end && item_segment(&node).is_some() {
            Some(s)
        } else {
            None
        }
    })
}

/// Symbol path for a chunk spanning `[start, end)`: the enclosing item at
/// `start`, or the first named item inside the chunk if `start` is in preamble.
fn symbol_path_for(root: &SyntaxNode, start: usize, end: usize) -> Vec<SymbolSegment> {
    let path = symbol_path_at(root, start);
    if !path.is_empty() {
        return path;
    }
    match first_symbol_start(root, start, end) {
        Some(off) => symbol_path_at(root, off),
        None => Vec::new(),
    }
}
```

In `chunk`, replace `symbol_path: Vec::new(),` in the `Chunk { … }` construction with:

```rust
                symbol_path: symbol_path_for(&root, r.start, r.end),
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content compact:: 2>&1 | tail -25`
Expected: PASS (all tests).

- [ ] **Step 5: Clippy clean + commit**

Run: `cargo clippy -p mn-content --all-targets -- -D warnings 2>&1 | tail -10`
Expected: no warnings.

```bash
git add crates/mn-content/src/code/compact.rs
git commit -m "feat(content): symbol_path extraction for Compact chunks"
```

---

## Task 4: Lossless-coverage property test

Guarantee the splitter tiles the source: for clean-parsing input, chunk ranges are sorted, non-overlapping, byte-accurate, and cover every non-whitespace byte (EC-51 "nothing is dropped").

**Files:**
- Modify: `crates/mn-content/src/code/compact.rs`

- [ ] **Step 1: Write the property test**

Add to the `tests` module (proptest is already a dev-dependency of `mn-content`):

```rust
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn ranges_tile_and_cover_non_whitespace(n in 1usize..10, budget in 6u32..40) {
            // Build a valid multi-circuit file from a proven-good template.
            let mut src = String::new();
            for i in 0..n {
                src.push_str(&format!("export circuit c{i}(a: Field): Field {{\n  return a;\n}}\n\n"));
            }
            let cfg = ChunkerConfig { max_tokens: budget, ..ChunkerConfig::default() };
            let chunks = CompactChunker.chunk(&src, &cfg).unwrap();
            prop_assume!(chunks.iter().all(|c| !c.fallback_used));

            // sorted + non-overlapping
            for w in chunks.windows(2) {
                prop_assert!(w[0].end_byte <= w[1].start_byte);
            }
            // byte-accurate
            for c in &chunks {
                prop_assert_eq!(&c.content, &src[c.start_byte..c.end_byte]);
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
```

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test -p mn-content compact:: 2>&1 | tail -20`
Expected: PASS. If it fails on a shrunk case, the failure prints the input + offending byte — fix `split_node` (most likely a trivia gap) and re-run.

- [ ] **Step 3: Commit**

```bash
git add crates/mn-content/src/code/compact.rs
git commit -m "test(content): proptest Compact chunk ranges tile the source"
```

---

## Task 5: Catastrophic-parse fallback

Mirror the tree-sitter path: if >50% of bytes sit inside `ERROR` tokens, fall back to line-window (`fallback_used = true`). Recoverable parses (minor diagnostics) must NOT fall back.

**Files:**
- Modify: `crates/mn-content/src/code/compact.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn garbage_falls_back_to_line_window() {
        // Non-Compact junk: each non-ASCII glyph lexes to an ERROR token, so
        // error bytes dominate → catastrophic fallback.
        let src = "🔥🔥🔥 ❌❌❌ ¡¡¡¡ §§§§ ".repeat(60);
        let chunks = CompactChunker.chunk(&src, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().any(|c| c.fallback_used), "garbage must fall back");
    }

    #[test]
    fn valid_compact_does_not_fall_back() {
        let chunks = CompactChunker.chunk(COUNTER, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().all(|c| !c.fallback_used));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content compact::tests::garbage 2>&1 | tail -15`
Expected: FAIL — no fallback yet (the splitter chunks the junk without checking errors).

- [ ] **Step 3: Implement the error-byte heuristic**

Add to `compact.rs`:

```rust
use compactp_syntax::SyntaxKind;

/// Total bytes covered by `ERROR` tokens in the tree (no double counting:
/// only leaf tokens are summed).
fn error_bytes(root: &SyntaxNode) -> usize {
    root.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::ERROR)
        .map(|t| {
            let r = t.text_range();
            usize::from(r.end()) - usize::from(r.start())
        })
        .sum()
}
```

In `chunk`, immediately after `let root = SyntaxNode::new_root(parsed.green);` and before the `SourceFile::cast` check, insert:

```rust
        if error_bytes(&root) * 2 > body.len() {
            return LineWindowChunker.chunk(body, cfg);
        }
```

- [ ] **Step 4: Run to verify both tests pass**

Run: `cargo test -p mn-content compact:: 2>&1 | tail -20`
Expected: PASS.

**If `garbage_falls_back_to_line_window` still fails** (compactp represents errors as `ERROR` *nodes*, not tokens): change `error_bytes` to count nodes instead —

```rust
fn error_bytes(root: &SyntaxNode) -> usize {
    root.descendants()
        .filter(|n| n.kind() == SyntaxKind::ERROR)
        .map(|n| {
            let r = n.text_range();
            usize::from(r.end()) - usize::from(r.start())
        })
        .sum()
}
```

Re-run; the requirement is simply that >50%-garbage input falls back while `COUNTER` does not.

- [ ] **Step 5: Clippy clean + commit**

Run: `cargo clippy -p mn-content --all-targets -- -D warnings 2>&1 | tail -10`

```bash
git add crates/mn-content/src/code/compact.rs
git commit -m "feat(content): catastrophic-parse fallback for Compact chunker"
```

---

## Task 6: Single-module package detection

A file with exactly one top-level `module <Name>` → `PackageRef { kind: "compact", name, manifest_path: None }`. Zero modules → `None` (the common case). Multiple top-level modules → `None` + a debug log (P1 limitation).

**Files:**
- Modify: `crates/mn-content/src/code/compact.rs`
- Modify: `crates/mn-content/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `compact.rs`:

```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content compact:: 2>&1 | tail -15`
Expected: FAIL — `detect_module_package` not found.

- [ ] **Step 3: Implement `detect_module_package`**

Add to `compact.rs` (public function, near the top after the imports). Add `use mn_core::types::PackageRef;` to the imports.

```rust
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
            if n.is_empty() { None } else { Some(n) }
        }
        _ => None,
    });
    let first = names.next()?;
    if names.next().is_some() {
        tracing::debug!("compact file declares multiple top-level modules; package left untagged (P1)");
        return None;
    }
    Some(PackageRef {
        kind: "compact".to_string(),
        name: first,
        manifest_path: None,
    })
}
```

- [ ] **Step 4: Add the always-present wrapper in `lib.rs`**

In `crates/mn-content/src/lib.rs`, add after the module declarations:

```rust
/// Detect Compact module-based package membership from file contents.
///
/// Returns `None` when the `compact` feature is disabled, or when the file
/// declares zero or multiple top-level modules (see [`code::compact`]).
#[must_use]
pub fn detect_compact_package(body: &str) -> Option<mn_core::types::PackageRef> {
    #[cfg(feature = "compact")]
    {
        crate::code::compact::detect_module_package(body)
    }
    #[cfg(not(feature = "compact"))]
    {
        let _ = body;
        None
    }
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p mn-content compact:: 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 6: Verify the no-feature wrapper compiles + returns None**

Run: `cargo build -p mn-content --no-default-features --features core-grammars 2>&1 | tail -5`
Expected: builds (the `#[cfg(not(feature = "compact"))]` arm is active).

- [ ] **Step 7: Clippy clean + commit**

Run: `cargo clippy -p mn-content --all-targets -- -D warnings 2>&1 | tail -10`

```bash
git add crates/mn-content/src/code/compact.rs crates/mn-content/src/lib.rs
git commit -m "feat(content): single-module Compact package detection (P1)"
```

---

## Task 7: Wire package detection into the ingest caller

Route `.compact` files to content-based detection; everything else keeps the filesystem manifest walk.

**Files:**
- Modify: `crates/mn-cli/src/commands/ingest/run.rs`

- [ ] **Step 1: Write the failing test**

Add a test module at the end of `crates/mn-cli/src/commands/ingest/run.rs` (or extend an existing `#[cfg(test)] mod tests`). Use a fresh module to avoid clashes:

```rust
#[cfg(test)]
mod compact_package_routing_tests {
    use super::detect_package_ref;
    use std::path::Path;

    #[test]
    fn compact_file_routes_to_module_detection() {
        let root = tempfile::tempdir().unwrap();
        let body = "module M {\n  export ledger b: Field;\n}\n";
        let pkg = detect_package_ref(root.path(), Path::new("src/Token.compact"), body)
            .expect("module M should be detected");
        assert_eq!(pkg.kind, "compact");
        assert_eq!(pkg.name, "M");
    }

    #[test]
    fn non_compact_file_ignores_content() {
        let root = tempfile::tempdir().unwrap();
        // No Cargo.toml/package.json anywhere → None, regardless of content.
        let pkg = detect_package_ref(root.path(), Path::new("src/lib.rs"), "module M {}");
        assert!(pkg.is_none());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-cli compact_package_routing 2>&1 | tail -20`
Expected: FAIL — `detect_package_ref` takes 2 args, not 3 (signature mismatch).

- [ ] **Step 3: Update `detect_package_ref` to accept and route on content**

In `run.rs`, replace the `detect_package_ref` function (~line 1129) with:

```rust
/// Detect package membership for a single file.
///
/// `.compact` files are detected from their contents (a single top-level
/// `module <Name>`); all other files walk up to the source root for the nearest
/// `Cargo.toml` / `package.json`.
fn detect_package_ref(
    source_root: &std::path::Path,
    rel_path: &std::path::Path,
    content: &str,
) -> Option<mn_core::types::PackageRef> {
    if rel_path.extension().and_then(|e| e.to_str()) == Some("compact") {
        return mn_content::detect_compact_package(content);
    }
    let abs = source_root.join(rel_path);
    mn_content::package::detect(&abs, source_root).map(|p| mn_core::types::PackageRef {
        kind: p.kind,
        name: p.name,
        manifest_path: Some(p.manifest_path.display().to_string()),
    })
}
```

- [ ] **Step 4: Update the call site to pass content**

At ~line 391, change:

```rust
            package: detect_package_ref(&source_root, &doc.rel_path),
```

to:

```rust
            package: detect_package_ref(&source_root, &doc.rel_path, &doc.content),
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p mn-cli compact_package_routing 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 6: Build the whole CLI + clippy**

Run: `cargo build -p mn-cli 2>&1 | tail -5 && cargo clippy -p mn-cli --all-targets -- -D warnings 2>&1 | tail -10`
Expected: builds, no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/mn-cli/src/commands/ingest/run.rs
git commit -m "feat(cli): route .compact files to module-based package detection"
```

---

## Task 8: mn-cli feature passthrough + opt-out

Make the chunker default-on in the shipped binary and excludable with one flag, without dropping the existing tree-sitter grammars.

**Files:**
- Modify: `crates/mn-cli/Cargo.toml`

- [ ] **Step 1: Add the passthrough feature and pin the mn-content grammar set**

In `crates/mn-cli/Cargo.toml`, change the `[features]` block from:

```toml
[features]
integration = []
```

to:

```toml
[features]
integration = []
default = ["compact"]
# Compact chunking (default-on). Exclude with `--no-default-features`.
compact = ["mn-content/compact"]
```

And change the `mn-content` dependency line from:

```toml
mn-content    = { path = "../mn-content" }
```

to:

```toml
mn-content    = { path = "../mn-content", default-features = false, features = ["core-grammars"] }
```

(This keeps the existing core-grammars chunkers always on, and gates Compact behind mn-cli's own default-on `compact` feature so `--no-default-features` drops only Compact.)

- [ ] **Step 2: Verify the default build includes Compact**

Run: `cargo build -p mn-cli 2>&1 | tail -5`
Expected: builds; `compactp_*` appear in the build.

Confirm the chunker is wired (the dispatch arm is active):

Run: `cargo test -p mn-cli compact_package_routing 2>&1 | tail -10`
Expected: PASS (proves `mn-content/compact` is enabled transitively).

- [ ] **Step 3: Verify the opt-out build excludes Compact and still compiles**

Run: `cargo build -p mn-cli --no-default-features 2>&1 | tail -8`
Expected: builds with no `compactp_*` crates; `detect_compact_package` returns `None`; `.compact` files line-window.

Confirm compactp is genuinely absent from the opt-out build:

Run: `cargo tree -p mn-cli --no-default-features -i compactp_parser 2>&1 | tail -5`
Expected: error/empty — `compactp_parser` is not in the dependency graph.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-cli/Cargo.toml
git commit -m "feat(cli): default-on Compact chunker with --no-default-features opt-out"
```

---

## Task 9: Acceptance fixtures + corpus test (SC-028 surrogate)

A deterministic, offline acceptance test over real fixtures: module files get a `compact` package and module-nested symbol paths; module-less files get `package = null`; nothing falls back.

**Files:**
- Create: `crates/mn-content/tests/fixtures/compact/counter.compact`
- Create: `crates/mn-content/tests/fixtures/compact/module_wpp.compact`
- Create: `crates/mn-content/tests/fixtures/compact/two_modules.compact`
- Create: `crates/mn-content/tests/compact_corpus.rs`

- [ ] **Step 1: Create the fixture files (verbatim real Compact)**

`crates/mn-content/tests/fixtures/compact/counter.compact`:

```compact
import CompactStandardLibrary;

export ledger round: Counter;

export circuit increment(): [] {
  round.increment(1);
}
```

`crates/mn-content/tests/fixtures/compact/module_wpp.compact`:

```compact
import CompactStandardLibrary;

module M {
  export ledger b: Field;

  export circuit brad(a: Field, c: Boolean): Field {
    if(disclose(c)) {
      b = disclose(a);
    }

    return b;
  }

  export circuit olaf(a: Field, b: Field) : Field {
    return brad(a + b, true);
  }
}

import M prefix $;
export {$brad}

ledger x: Field;
export ledger set: Set<Field>;

circuit andy(a: Field, b: Boolean) : [] {
  $brad(a, b);
}

export circuit gary(a: Field, b: Boolean) : [] {
  $brad($brad(disclose(a), disclose(b)), disclose(b));
  $olaf(disclose(a), disclose(a));
}
```

`crates/mn-content/tests/fixtures/compact/two_modules.compact`:

```compact
module A {
  export ledger a: Field;
}

module B {
  export ledger b: Field;
}
```

- [ ] **Step 2: Write the corpus test**

Create `crates/mn-content/tests/compact_corpus.rs`:

```rust
//! Acceptance test for the Compact chunker (SC-028 surrogate, offline).
//!
//! Asserts module-based package detection and symbol-aware chunking against
//! real fixtures from compactp's own corpus. The full OZ `compact-contracts`
//! clone is a separate, network-dependent CI/manual step (see the design doc).
#![cfg(feature = "compact")]

use std::path::PathBuf;

use mn_content::chunk::{Chunker, ChunkerConfig};
use mn_content::code::compact::CompactChunker;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/compact")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn counter_has_symbol_chunks_and_no_package() {
    let src = fixture("counter.compact");
    let chunks = CompactChunker.chunk(&src, &ChunkerConfig::default()).unwrap();
    assert!(chunks.iter().all(|c| !c.fallback_used), "counter must parse cleanly");
    assert!(
        chunks.iter().any(|c| c.symbol_path.iter().any(|s| s.kind == "circuit" && s.name == "increment")),
        "expected [circuit increment]"
    );
    // module-less file → no package
    assert!(mn_content::detect_compact_package(&src).is_none());
}

#[test]
fn module_file_tags_package_and_nested_symbols() {
    let src = fixture("module_wpp.compact");
    // small budget forces per-item chunks so the module-nested path appears
    let cfg = ChunkerConfig { max_tokens: 24, ..ChunkerConfig::default() };
    let chunks = CompactChunker.chunk(&src, &cfg).unwrap();
    assert!(chunks.iter().all(|c| !c.fallback_used), "module_wpp must parse cleanly");

    // SC-028: exactly one top-level module M → compact/M package
    let pkg = mn_content::detect_compact_package(&src).expect("module M → package");
    assert_eq!(pkg.kind, "compact");
    assert_eq!(pkg.name, "M");

    // a chunk inside M carries the module prefix
    assert!(
        chunks.iter().any(|c| {
            c.symbol_path.iter().any(|s| s.kind == "module" && s.name == "M")
                && c.symbol_path.iter().any(|s| s.kind == "circuit")
        }),
        "expected a module-nested circuit chunk: {:?}",
        chunks.iter().map(|c| &c.symbol_path).collect::<Vec<_>>()
    );
    // a top-level circuit outside M has no module prefix
    assert!(
        chunks.iter().any(|c| {
            c.symbol_path.iter().any(|s| s.kind == "circuit" && s.name == "gary")
                && !c.symbol_path.iter().any(|s| s.kind == "module")
        }),
        "expected top-level [circuit gary] with no module prefix"
    );
}

#[test]
fn two_modules_leave_package_untagged() {
    let src = fixture("two_modules.compact");
    let chunks = CompactChunker.chunk(&src, &ChunkerConfig::default()).unwrap();
    assert!(chunks.iter().all(|c| !c.fallback_used), "two_modules must parse cleanly");
    assert!(mn_content::detect_compact_package(&src).is_none(), "multi-module → no package (P1)");
}
```

Note: `code::compact` is `pub` and the integration test enables `--features compact` via the crate default, so `mn_content::code::compact::CompactChunker` resolves. If the workspace ever drops `compact` from `mn-content`'s default, run these with `--features compact`.

- [ ] **Step 3: Run the corpus test**

Run: `cargo test -p mn-content --test compact_corpus 2>&1 | tail -20`
Expected: PASS (all three tests). If any fixture trips `fallback_used`, the fixture text is wrong — re-copy it from compactp's corpus.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-content/tests/fixtures/compact crates/mn-content/tests/compact_corpus.rs
git commit -m "test(content): Compact corpus acceptance (SC-028 surrogate)"
```

---

## Task 10: Documentation

Make the README's "first-class citizen" claim true, document the opt-out, and update the stale code comment + spec note.

**Files:**
- Modify: `README.md`
- Modify: `crates/mn-content/src/lib.rs`
- Modify: `specs/001-rag-platform/spec.md`

- [ ] **Step 1: Update the grammar-tier paragraph in `README.md`**

Find the paragraph (~line 400) beginning "Grammars are **Cargo-feature-gated** into tiers". Append a sentence:

```markdown
Compact chunking is its own default-on feature (`compact`, backed by the [`compactp`](https://crates.io/crates/compactp_parser) parser); build the CLI without the experimental Compact chunker via `cargo build -p mn-cli --no-default-features` (the tree-sitter grammars stay on).
```

- [ ] **Step 2: Fix the stale module doc comment in `lib.rs`**

In `crates/mn-content/src/lib.rs`, replace:

```rust
//! Phase-3 lands the Markdown side: heading-based chunker with fallback windowing,
//! frontmatter parser, manifest loader, content-hash. The tree-sitter code
//! chunkers and Compact module scanner land in Phase 6.
```

with:

```rust
//! Markdown (heading-based), code (tree-sitter per language; Compact via the
//! `compactp` parser behind the `compact` feature), and a line-window fallback,
//! plus frontmatter, manifest loading, content-hash, and package detection.
```

- [ ] **Step 3: Add a note to FR-047 in the spec**

In `specs/001-rag-platform/spec.md`, find the FR-047 row (~line 1273) and append to its description (inside the table cell, before the trailing `|`):

```markdown
 — IMPLEMENTED via the compactp parser (rowan CST), superseding the hand-rolled scanner; single top-level module → package (P1); per-chunk multi-module tagging deferred.
```

- [ ] **Step 4: Final full-workspace verification**

Run: `cargo fmt --all -- --check 2>&1 | tail -5`
Expected: clean (run `cargo fmt --all` if not).

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15`
Expected: no warnings.

Run: `cargo test -p mn-content -p mn-cli 2>&1 | tail -20`
Expected: all green (sandbox note: two `mn-cli` `auth_integration` loopback tests are known to fail in this environment and are not a regression; DB integration tests run in CI only).

- [ ] **Step 5: Commit**

```bash
git add README.md crates/mn-content/src/lib.rs specs/001-rag-platform/spec.md
git commit -m "docs: document Compact chunker feature + opt-out; update FR-047"
```

---

## Manual / CI follow-up (not a code task)

- **Full SC-028 over OpenZeppelin `compact-contracts`:** clone the repo, `mnm manifest generate`, `mnm ingest run`, and assert every `.compact` declaring a module has ≥1 chunk tagged `compact`/`<module>`. This needs network + a running server, so it belongs in CI or a manual acceptance pass, not the unit/integration suite. **This is the P1→P2 tripwire:** if any OZ `.compact` file declares *multiple* top-level modules, those modules won't all be tagged under P1 — escalate to the deferred P2 (per-chunk `chunk.package_id`) design.

---

## Self-Review

**Spec coverage** (design doc → task):
- Decision 1 (library dep): Task 1 (workspace + mn-content deps). ✓
- Decision 2 (bespoke rowan walker): Task 1–3 (`compact.rs`, not `run_tree_sitter`). ✓
- Decision 3 (uniform recursive largest-fit + lossless coverage): Task 2 (`split_node`) + Task 4 (proptest). ✓
- Decision 4 (P1 package detection): Task 6 + Task 7. ✓
- symbol_path item→kind table: Task 3 (`item_segment`, all 14 `Item` variants). ✓
- Error handling/fallback heuristic: Task 5. ✓
- Feature gating + default-on + opt-out + version pin + MSRV: Task 1 (mn-content) + Task 8 (mn-cli). ✓
- Testing (unit, proptest invariant, package, corpus/SC-028): Tasks 2–6, 9. ✓
- Docs (README + FR-047 + lib.rs comment): Task 10. ✓
- Non-goal (P2 per-chunk): explicitly deferred (Task 6 log + Manual follow-up tripwire). ✓

**Placeholder scan:** no TBD/TODO; every code step shows complete code; the one empirical branch (Task 5 error_bytes node-vs-token) ships both implementations with a clear decision criterion. ✓

**Type consistency:** `CompactChunker`, `split_node(node, body, budget, out)`, `item_segment`/`symbol_path_at`/`first_symbol_start`/`symbol_path_for`, `detect_module_package` (compact.rs) vs `detect_compact_package` (lib.rs wrapper) are used consistently across tasks; `detect_package_ref` 3-arg signature matches its call site; `PackageRef`/`SymbolSegment` field names match `mn-core`. ✓

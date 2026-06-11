# Contextualized + Dual Code Embeddings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the corpus to voyage-context-3 contextualized embeddings for all document kinds, add a second voyage-code-3 embedding for code files, remove all chunk overlap, raise the chunk budget to 1024 tokens with greedy 90% coalescing, and expose a `code_mode` search parameter end-to-end (server, CLI, MCP).

**Architecture:** One chunk row gains a nullable `code_embedding vector(1024)` column (migration 0011). Chunking loses every overlap mechanism (markdown rolling-window expansion, markdown window fallback overlap, line-window overlap) and coalesces greedily toward 90% of `max_tokens`. Ingest groups each document's chunks into ≤28,800-token context groups and embeds them via the new `/v1/contextualizedembeddings` Voyage endpoint; code-kind documents are additionally embedded flat with voyage-code-3. Search fuses an optional third ranked list (code-vector ANN over a partial HNSW index) into the existing RRF pool.

**Tech Stack:** Rust workspace (MSRV 1.91), sqlx + pgvector, axum, reqwest (HTTP/1.1-only for Voyage), clap v4, proptest, wiremock (new dev-dep for mn-embedding).

**Spec:** `docs/superpowers/specs/2026-06-10-contextualized-dual-embeddings-design.md`

**Sandbox caveats (read before executing):**
- DB integration tests cannot run locally (no Docker/DATABASE_URL) — they are verified in CI. Run `cargo test --workspace` (unit) locally; do not be alarmed that `--features integration` tests don't run.
- The sandbox sets `VOYAGE_API_KEY`; run mn-mcp/mn-cli tests with `VOYAGE_API_KEY= cargo test ...` where noted.
- 2 mn-cli `auth_integration` loopback tests always fail in this sandbox; not a regression.
- Before each commit: `cargo fmt` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` (CI gates on the full surface; building one crate is not enough).
- All sqlx in touched files uses runtime `sqlx::query`/`query_as`/`QueryBuilder` (no `query!` macros), so the offline `.sqlx` cache does not need regenerating.

**Mid-plan runtime caveat:** between Task 15 (config default flip) and Task 17, `mnm search` BYOK against the live Voyage API would send voyage-context-3 to the flat endpoint and fail at runtime. Tests stay green throughout; just don't manually smoke-test BYOK search in that window. This is a feature branch; the window closes within the same PR.

**Out of scope (per spec §12 + decisions during planning):** Voyage auto-chunking, reranker replacement, `mnm models migrate`. The `tests/recall/` harness (spec §13 last bullet) is a separate follow-up plan — it needs its own fixture-design pass; this plan covers the equivalent assertions as integration tests in Task 18.

---

## File Structure

New files:
- `crates/mn-content/src/context_group.rs` — balanced context-group splitting (§6)
- `crates/mn-embedding/src/contextualized.rs` — `ContextualizedVoyageEmbedder`
- `crates/mn-store/migrations/0011_dual_embeddings.sql`
- `crates/mn-server/src/code_model.rs` — boot-resolved code model (mirrors `corpus_model.rs`)

Heavily modified:
- `crates/mn-content/src/{language.rs, chunk.rs, markdown.rs, code/mod.rs, code/line_window.rs, manifest/resolve.rs, manifest/mod.rs}`
- `crates/mn-core/src/{types.rs, config.rs}`
- `crates/mn-embedding/src/{voyage.rs, client.rs, lib.rs}`
- `crates/mn-store/src/entities/{chunk.rs, source_version.rs}`
- `crates/mn-server/src/{app.rs, main.rs, config.rs, routes/embeddings.rs, routes/search.rs, routes/admin_ingest.rs, routes/models.rs}`
- `crates/mn-cli/src/commands/{ingest/run.rs, search.rs, models.rs}`
- `crates/mn-mcp/src/tools.rs`

---

### Task 1: Extension allowlist + kind classification fix (D7, §5.4)

**Files:**
- Modify: `crates/mn-content/src/language.rs`
- Modify: `crates/mn-content/src/manifest/resolve.rs:234-240`

- [ ] **Step 1: Write failing tests**

In `crates/mn-content/src/language.rs` tests module, add:

```rust
#[test]
fn code_chunker_languages_are_discoverable() {
    // Every language the code chunker supports must resolve here, else
    // glob-included files of that language bypass discovery defaults (D7).
    let cases = [
        ("a.py", "python"), ("a.pyi", "python"), ("a.go", "go"),
        ("a.sol", "solidity"), ("a.sh", "bash"), ("a.bash", "bash"),
        ("a.scm", "scheme"), ("a.ss", "scheme"), ("a.sld", "scheme"),
        ("a.java", "java"), ("a.swift", "swift"), ("a.rb", "ruby"),
        ("a.kt", "kotlin"), ("a.kts", "kotlin"), ("a.cs", "csharp"),
        ("a.hs", "haskell"), ("a.html", "html"), ("a.htm", "html"),
        ("a.xml", "xml"), ("a.csproj", "xml"), ("a.nuspec", "xml"),
        ("a.plist", "xml"), ("a.mjs", "javascript"), ("a.cjs", "javascript"),
    ];
    for (path, lang) in cases {
        assert_eq!(from_path(Path::new(path)), Some(lang), "{path}");
    }
}
```

In `crates/mn-content/src/manifest/resolve.rs` tests module, add:

```rust
#[test]
fn txt_is_plaintext_json_is_code() {
    assert_eq!(kind_for(Path::new("notes.txt")), DocumentKind::Plaintext);
    assert_eq!(kind_for(Path::new("cfg.json")), DocumentKind::Code);
    assert_eq!(kind_for(Path::new("doc.md")), DocumentKind::Markdown);
    assert_eq!(kind_for(Path::new("lib.rs")), DocumentKind::Code);
    assert_eq!(kind_for(Path::new("mystery.zzz")), DocumentKind::Plaintext);
}
```

(`kind_for` is private to the module; the test module is in the same file so it can call it directly. Check whether `DocumentKind` is already imported in the test module; it is used at line 275 so it is in scope.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mn-content code_chunker_languages_are_discoverable txt_is_plaintext`
Expected: FAIL (`from_path` returns `None` for the new extensions; `kind_for` returns `Code` for txt).

- [ ] **Step 3: Implement**

In `language.rs`, extend the match (keep existing arms):

```rust
Some(match ext.as_str() {
    "md" | "mdx" => "markdown",
    "rs" => "rust",
    "ts" | "tsx" => "typescript",
    "js" | "jsx" | "mjs" | "cjs" => "javascript",
    "compact" => "compact",
    "txt" => "plaintext",
    "json" => "json",
    "yaml" | "yml" => "yaml",
    "toml" => "toml",
    "py" | "pyi" => "python",
    "go" => "go",
    "sol" => "solidity",
    "sh" | "bash" => "bash",
    "scm" | "ss" | "sld" => "scheme",
    "java" => "java",
    "swift" => "swift",
    "rb" => "ruby",
    "kt" | "kts" => "kotlin",
    "cs" => "csharp",
    "hs" => "haskell",
    "html" | "htm" => "html",
    "xml" | "csproj" | "nuspec" | "plist" => "xml",
    _ => return None,
})
```

In `resolve.rs`, fix `kind_for`:

```rust
fn kind_for(path: &Path) -> DocumentKind {
    match crate::language::from_path(path) {
        Some("markdown") => DocumentKind::Markdown,
        // `.txt` resolves to "plaintext" but must NOT take the Code path —
        // it has no grammar and belongs with unknown files in line-window land.
        Some("plaintext") | None => DocumentKind::Plaintext,
        Some(_) => DocumentKind::Code,
    }
}
```

- [ ] **Step 4: Run tests** — `cargo test -p mn-content` — Expected: PASS (also re-check existing `language` and `resolve` tests).

- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(mn-content): extend extension allowlist to all chunker languages; txt classifies Plaintext"`

---

### Task 2: `SymbolSegment.path` + multi-symbol chunk entries (§5.3)

**Files:**
- Modify: `crates/mn-core/src/types.rs:279-286` (SymbolSegment)
- Modify: `crates/mn-content/src/code/mod.rs` (`run_tree_sitter`, new `entries_from_path`)
- Modify: `crates/mn-content/src/code/symbols.rs` (struct literals gain `path`)

The stored shape changes from "one root→leaf ancestor path per chunk" to "flat union of symbol entries, one per symbol the chunk touches". Each entry keeps its own ancestor names in a new `path` field. Ancestors are ALSO emitted as entries so the existing `@> [{"kind","name"}]` facet queries keep matching enclosing scopes. `path` is `skip_serializing_if = "Vec::is_empty"`, so top-level entries serialize byte-identically to today's `{kind,name}` objects.

- [ ] **Step 1: Extend `SymbolSegment`**

```rust
pub struct SymbolSegment {
    /// Syntactic kind: "impl", "fn", "class", "interface", "key", "element", …
    pub kind: String,
    /// Identifier or label for this segment.
    pub name: String,
    /// Ancestor symbol names, outermost first. Empty for top-level symbols.
    /// (Since the dual-embeddings cutover, `chunk.symbol_path` is a flat
    /// union of entries — nesting lives here, not in array order.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
}
```

Then fix every struct-literal constructor: `grep -rn "SymbolSegment {" crates/` and add `path: Vec::new(),` to each (expect hits in `mn-content/src/code/symbols.rs`, `mn-content/src/code/mod.rs` tests, and possibly mn-server/mn-cli test fixtures). Run `cargo build --workspace` to find any missed.

- [ ] **Step 2: Write failing test for `entries_from_path`**

In `crates/mn-content/src/code/mod.rs` (new test in `coalesce_tests` module or a sibling module):

```rust
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
```

(The existing `seg(kind, name)` helper gains `path: Vec::new()` in Step 1.)

- [ ] **Step 3: Run** — `cargo test -p mn-content entries_from_path` — Expected: FAIL (function missing).

- [ ] **Step 4: Implement in `code/mod.rs`**

```rust
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
```

In `run_tree_sitter`, wrap both `symbol_path_at` call sites:

```rust
let mut symbol_path = entries_from_path(&symbol_path_at(&tree, body, r.start, table));
if symbol_path.is_empty() {
    if let Some(off) = symbols::first_symbol_start(&tree, r.start, r.end, table) {
        symbol_path = entries_from_path(&symbol_path_at(&tree, body, off, table));
    }
}
```

Check `symbols::enclosing_symbol_headers` (breadcrumb pass) — it reads the tree directly, not `chunk.symbol_path`, so it is unaffected. Existing rust/ts/etc. chunker tests that assert `symbol_path` contents will need their expectations updated from "ancestor path" to "entry list" — same `(kind, name)` pairs, plus `path` values; fix them as they fail.

- [ ] **Step 5: Run** — `cargo test -p mn-content && cargo test --workspace` — fix any remaining literal/test fallout. Expected: PASS.

- [ ] **Step 6: Commit** — `git commit -am "feat: symbol_path becomes flat symbol-entry union with per-entry ancestor path"`

---

### Task 3: Greedy code coalescing to 90% (§5.1)

**Files:**
- Modify: `crates/mn-content/src/chunk.rs` (add `coalesce_target`)
- Modify: `crates/mn-content/src/code/mod.rs` (`coalesce_code`)

- [ ] **Step 1: Add the shared target helper to `chunk.rs`**

```rust
/// Greedy coalescing target: 90% of `max_tokens` (D2). Both the markdown and
/// code coalescers pack sibling units up to this; only a single unit larger
/// than `max_tokens` is ever split.
#[must_use]
pub const fn coalesce_target(cfg: &ChunkerConfig) -> u32 {
    cfg.max_tokens.saturating_mul(9) / 10
}
```

- [ ] **Step 2: Rewrite the coalesce tests to pin the NEW behavior**

Replace the three tests in `coalesce_tests` (`folds_wrapper_only_fragment_into_child`, `does_not_merge_distinct_top_level_symbols`, `does_not_merge_past_floor`) with:

```rust
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
    let cfg = ChunkerConfig { max_tokens: 256, ..ChunkerConfig::default() };
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
```

- [ ] **Step 3: Run** — `cargo test -p mn-content coalesce` — Expected: FAIL (old scope-gated behavior).

- [ ] **Step 4: Rewrite `coalesce_code`**

```rust
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
```

`common_prefix_len` loses its only caller — delete it. `cfg.code_min_tokens` is now unused by this function (the field itself is deleted in Task 6).

- [ ] **Step 5: Run** — `cargo test -p mn-content` — per-language chunker tests asserting chunk counts will shift (more merging); update their expectations to the new packing. Expected: PASS after updates.

- [ ] **Step 6: Commit** — `git commit -am "feat(mn-content): greedy code coalescing to 90% of budget, cross-scope, symbol-entry union"`

---

### Task 4: Token-budgeted, non-overlapping line windows (§5.2)

**Files:**
- Modify: `crates/mn-content/src/code/line_window.rs` (full rewrite of `chunk`)

- [ ] **Step 1: Rewrite the tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{coalesce_target, Chunker, ChunkerConfig};

    #[test]
    fn windows_are_token_budgeted_and_disjoint() {
        let body: String = (0..400).fold(String::new(), |mut s, i| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line {i} with a few more words for tokens");
            s
        });
        let cfg = ChunkerConfig { max_tokens: 128, ..ChunkerConfig::default() };
        let chunks = LineWindowChunker.chunk(&body, &cfg).unwrap();
        assert!(chunks.len() >= 2, "long input must produce multiple windows");
        let target = coalesce_target(&cfg);
        for c in &chunks {
            assert!(c.fallback_used);
            // Within budget OR a single line that alone exceeds it.
            let single_line = c.content.trim_end().lines().count() == 1;
            assert!(c.token_count <= target || single_line);
        }
        // Disjoint and contiguous in document order.
        for w in chunks.windows(2) {
            assert!(w[1].start_byte >= w[0].end_byte, "windows must not overlap");
        }
    }

    #[test]
    fn empty_input_no_chunks() {
        assert!(LineWindowChunker
            .chunk("  \n ", &ChunkerConfig::default())
            .unwrap()
            .is_empty());
    }
}
```

- [ ] **Step 2: Run** — `cargo test -p mn-content line_window` — Expected: FAIL (overlap assertion).

- [ ] **Step 3: Rewrite the implementation**

```rust
//! Line-window fallback — used for Plaintext, unknown languages, and
//! parser-error recovery. Token-budgeted, NON-overlapping windows (D3):
//! each window grows line by line until adding the next line would push it
//! past 90% of `max_tokens`, then the next window starts on the next line.

use crate::chunk::{coalesce_target, Chunk, ChunkError, Chunker, ChunkerConfig};

/// Splits source into token-budgeted, non-overlapping line windows.
pub struct LineWindowChunker;

impl Chunker for LineWindowChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }
        let target = coalesce_target(cfg);
        // Precompute byte offset of the start of each line.
        let mut line_starts = vec![0usize];
        for (i, b) in body.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let total_lines = line_starts.len();
        let line_end = |i: usize| -> usize {
            if i + 1 < total_lines { line_starts[i + 1] } else { body.len() }
        };

        let mut chunks = Vec::new();
        let mut start_line = 0usize;
        let mut idx = 0u32;
        while start_line < total_lines {
            // Grow: at least one line, then keep adding while within budget.
            let mut end_line = start_line + 1;
            while end_line < total_lines {
                let slice = &body[line_starts[start_line]..line_end(end_line)];
                if crate::tokens::count(slice) > target {
                    break;
                }
                end_line += 1;
            }
            let start_byte = line_starts[start_line];
            let end_byte = line_end(end_line - 1);
            let content = body[start_byte..end_byte].to_string();
            if !content.trim().is_empty() {
                chunks.push(Chunk {
                    token_count: crate::tokens::count(&content),
                    content,
                    heading_path: Vec::new(),
                    symbol_path: Vec::new(),
                    start_byte,
                    end_byte,
                    chunk_index: idx,
                    fallback_used: true,
                });
                idx += 1;
            }
            start_line = end_line;
        }
        Ok(chunks)
    }
}
```

`cfg.fallback_lines` / `cfg.fallback_overlap_lines` lose their last consumer (fields deleted in Task 6).

- [ ] **Step 4: Run** — `cargo test -p mn-content` — Expected: PASS (fix any chunker tests that asserted line-window overlap).

- [ ] **Step 5: Commit** — `git commit -am "feat(mn-content): line-window fallback becomes token-budgeted and non-overlapping"`

---

### Task 5: Markdown — greedy coalescing, no overlap, remove rolling-window expansion (§5.1, §5.2)

**Files:**
- Modify: `crates/mn-content/src/markdown.rs`

Three changes: (a) `coalesce_segments` packs greedily to `coalesce_target` (no `min_tokens` floor), (b) `token_window_split` loses its overlap step-back, (c) `expand_window`/`grow_side`/`segment_sentences`/`push_trimmed`/`pct_tokens` are **deleted** — the rolling-window context expansion from the 2026-06-08 chunk-context design overlapped chunk ranges by design; contextualized embeddings replace it (spec §5.2).

- [ ] **Step 1: Write the new invariant test first**

Add to markdown.rs tests:

```rust
/// THE no-overlap invariant (D3): every emitted chunk's byte range is
/// disjoint from every other chunk's, in document order.
#[test]
fn markdown_chunks_never_overlap() {
    let line = "the quick brown fox jumps over the lazy dog near the river bank.\n";
    let mut md = String::from("# Top\n\nintro paragraph.\n\n");
    for h in ["A", "B", "C", "D", "E", "F"] {
        md.push_str(&format!("## Section {h}\n\n{}", line.repeat(8)));
    }
    let cfg = ChunkerConfig { max_tokens: 64, ..ChunkerConfig::default() };
    let chunks = MarkdownChunker.chunk(&md, &cfg).unwrap();
    assert!(chunks.len() >= 2);
    for w in chunks.windows(2) {
        assert!(
            w[1].start_byte >= w[0].end_byte,
            "overlap: [{}, {}) then [{}, {})",
            w[0].start_byte, w[0].end_byte, w[1].start_byte, w[1].end_byte
        );
    }
}
```

- [ ] **Step 2: Run** — `cargo test -p mn-content markdown_chunks_never_overlap` — Expected: FAIL (expand_window pads ranges into neighbours).

- [ ] **Step 3: Implement**

In `coalesce_segments`, replace the inner loop body (keep the depth rule — it stops a run from escaping its subtree, which keeps `heading_path` honest):

```rust
let target = crate::chunk::coalesce_target(cfg);
// ... unchanged setup ...
while j < segments.len() {
    let next = &segments[j];
    // Structural: never absorb a segment SHALLOWER than the run anchor.
    if next.depth < start_depth {
        break;
    }
    // Greedy fill (D2): stop when absorbing the next section would push the
    // run past the 90% target. A single >target segment passes through alone
    // (windowed later).
    if crate::tokens::count(&body[start..next.end]) > target {
        break;
    }
    end = next.end;
    j += 1;
}
```

Remove `min` / `max` locals. In the `Chunker::chunk` second pass, replace the `expand_window` branch:

```rust
let target = crate::chunk::coalesce_target(cfg);
for seg in segments {
    let text = &body[seg.start..seg.end];
    if text.trim().is_empty() {
        continue;
    }
    if crate::tokens::count(text) <= cfg.max_tokens {
        chunks.push(Chunk {
            content: text.to_owned(),
            heading_path: seg.heading_path.clone(),
            symbol_path: Vec::new(),
            start_byte: seg.start,
            end_byte: seg.end,
            token_count: crate::tokens::count(text),
            chunk_index: 0,
            fallback_used: false,
        });
    } else {
        for window in token_window_split(text, seg.start, &seg.heading_path, cfg) {
            chunks.push(window);
        }
    }
}
```

In `token_window_split`: budget against `target` instead of `cfg.max_tokens` (so split windows also aim at 90%), and remove the overlap step-back:

```rust
// growth check becomes:
if crate::tokens::count(slice) > crate::chunk::coalesce_target(cfg) {
    break;
}
// and the step at the bottom becomes (delete `overlap_lines` entirely):
window_start_line = last_line + 1;
```

Delete `expand_window`, `grow_side`, `segment_sentences`, `push_trimmed`, `pct_tokens`, and the doc-comment references to overlap. Update the module doc comment to describe heading-split + greedy coalesce + non-overlapping window split.

- [ ] **Step 4: Fix the test suite**

Delete tests that pin removed behavior: `windows_overlap`, `windowed_chunk_spans_neighbours_but_stays_bounded`, `expand_grows_both_sides_when_centered`, `expand_is_noop_when_core_already_at_target`, `expand_at_document_start_only_grows_forward`, `segments_prose_into_sentences`, `fenced_code_block_is_one_atomic_unit`, `table_rows_group_into_one_unit`, `empty_region_yields_no_units`, `unterminated_fence_consumes_to_eof`, `multibyte_terminators_do_not_split_or_panic`, and the `window_cfg` helper.

Tests using `min_tokens` in their config (`no_coalesce_cfg`, `small_cfg`, `merged_chunk_uses_first_segments_path`, `window_cfg`): greedy-to-target removes the disable knob, so rewrite `no_coalesce_cfg`-dependent tests against small `max_tokens` with section bodies sized to NOT merge. Concretely, replace `no_coalesce_cfg()` with:

```rust
/// Coalescing-suppressing config for per-heading assertions: a tiny budget
/// whose 90% target is smaller than any two adjacent test sections combined,
/// so each heading stays its own chunk (and sections stay under max_tokens
/// so no window split kicks in either).
fn per_section_cfg() -> ChunkerConfig {
    ChunkerConfig { max_tokens: 28, ..ChunkerConfig::default() }
}
```

and in `nested_headings_record_path` / `chunk_indices_are_sequential` give each section a one-sentence body of ~15 tokens (e.g. `"this section body has roughly fifteen tokens of filler text here"`) so one section fits (≤28) but two do not (>25 target). Keep the assertions unchanged. For `merged_chunk_uses_first_segments_path`, drop the `min_tokens: 12` override and use `ChunkerConfig::default()` — under greedy packing `# Top` intro WILL absorb `## A` + `### a1` (depth ≥ anchor, all tiny), so update the test to find the single merged chunk and assert its `heading_path` is the FIRST segment's path (`[]`, the preamble anchored at `# Top`):

```rust
#[test]
fn merged_chunk_uses_first_segments_path() {
    let md = "# Top\n\nintro.\n\n## A\n\nshort\n\n### a1\n\nalso short\n";
    let chunks = MarkdownChunker.chunk(md, &ChunkerConfig::default()).unwrap();
    assert_eq!(chunks.len(), 1, "tiny sections all pack into the anchor run");
    assert!(chunks[0].content.contains("### a1"));
    assert!(chunks[0].heading_path.is_empty(), "run keeps the FIRST segment's path");
}
```

`small_cfg` keeps working (drop its `min_tokens` line in Task 6; until then it compiles).

- [ ] **Step 5: Run** — `cargo test -p mn-content` — Expected: PASS.

- [ ] **Step 6: Commit** — `git commit -am "feat(mn-content): markdown greedy coalescing to 90%, no window overlap, remove rolling-window expansion"`

---

### Task 6: ChunkerConfig cleanup + CLI flag consolidation (§5.1)

**Files:**
- Modify: `crates/mn-content/src/chunk.rs`
- Modify: `crates/mn-content/src/ingest/plan.rs` (config plumbing only)
- Modify: `crates/mn-cli/src/commands/ingest/run.rs:140-156` (flags) and the `ChunkerConfig` construction at `:388-395`

- [ ] **Step 1: Slim the config**

```rust
/// Configuration shared by all chunkers. Token-budgeted.
#[derive(Debug, Clone, Copy)]
pub struct ChunkerConfig {
    /// Max chunk size in BPE tokens before splitting. Greedy coalescing in
    /// every chunker packs units up to [`coalesce_target`] (90% of this).
    pub max_tokens: u32,
    /// Files larger than this are skipped by callers (EC-52).
    pub max_file_bytes: u64,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            max_tokens: 1024,
            max_file_bytes: 10 * 1024 * 1024,
        }
    }
}
```

Delete `min_tokens`, `window_switch_pct`, `window_target_pct`, `window_cap_pct`, `code_min_tokens`, `fallback_lines`, `fallback_overlap_lines`. Update `default_config_is_token_budgeted` test accordingly (`max_tokens == 1024`). Fix every `ChunkerConfig { ... }` literal across the workspace (`cargo build --workspace` finds them; they are all test configs plus the two production constructors in `ingest/plan.rs` and `mn-cli run.rs`).

- [ ] **Step 2: Replace the CLI flags**

In `run.rs`, delete `code_chunk_tokens`, `md_min_tokens`, `code_chunk_lines`, `code_chunk_overlap` and add:

```rust
/// Chunk budget in tokens, all document kinds (markdown, code, plaintext).
/// Greedy coalescing packs sibling units up to 90% of this.
#[arg(long = "chunk-tokens", default_value_t = 1024)]
pub chunk_tokens: u32,
```

Update the construction site:

```rust
let chunker_config = mn_content::chunk::ChunkerConfig {
    max_tokens: args.chunk_tokens,
    max_file_bytes: args.max_file_size,
};
```

Hard cutover — no deprecation aliases (pre-1.0).

- [ ] **Step 3: Run** — `cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets --all-features -- -D warnings` — Expected: PASS. Any mn-cli test invoking the deleted flags gets updated to `--chunk-tokens`.

- [ ] **Step 4: Add the cross-chunker no-overlap property test**

New test in `crates/mn-content/tests/no_overlap.rs`:

```rust
//! D3: NO chunker emits overlapping chunks. Property-tested across the
//! markdown, code (rust), and line-window chunkers.

use mn_content::chunk::{Chunker, ChunkerConfig};
use proptest::prelude::*;

fn assert_disjoint(chunks: &[mn_content::chunk::Chunk]) {
    for w in chunks.windows(2) {
        prop_assert!(w[1].start_byte >= w[0].end_byte);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn line_window_disjoint(lines in proptest::collection::vec("[a-z ]{0,80}", 1..200)) {
        let body = lines.join("\n");
        let cfg = ChunkerConfig { max_tokens: 64, ..ChunkerConfig::default() };
        let chunks = mn_content::code::line_window::LineWindowChunker.chunk(&body, &cfg).unwrap();
        assert_disjoint(&chunks)?;
    }

    #[test]
    fn markdown_disjoint(paras in proptest::collection::vec("[a-z ]{1,120}", 1..40)) {
        let body: String = paras.iter().enumerate()
            .map(|(i, p)| format!("## H{i}\n\n{p}.\n\n"))
            .collect();
        let cfg = ChunkerConfig { max_tokens: 64, ..ChunkerConfig::default() };
        let chunks = mn_content::markdown::MarkdownChunker.chunk(&body, &cfg).unwrap();
        assert_disjoint(&chunks)?;
    }
}
```

(If `assert_disjoint`'s `prop_assert!`-in-helper shape fights the macros, inline the loop into each test. Check the visibility of `mn_content::code::line_window` / `markdown` from an integration test — both modules are `pub` per `code/mod.rs` and `lib.rs`; adjust paths to match `lib.rs` re-exports.)

- [ ] **Step 5: Run** — `cargo test -p mn-content --test no_overlap` — Expected: PASS.

- [ ] **Step 6: Commit** — `git commit -am "feat: 1024-token budget, single --chunk-tokens flag, drop dead overlap/floor config + no-overlap property tests"`

---

### Task 7: Balanced context grouping (§6, D8)

**Files:**
- Create: `crates/mn-content/src/context_group.rs`
- Modify: `crates/mn-content/src/lib.rs` (add `pub mod context_group;`)

- [ ] **Step 1: Write the property tests first** (same file, `#[cfg(test)]`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn single_small_doc_is_one_group() {
        assert_eq!(balanced_groups(&[100, 200, 300], 28_800), vec![0..3]);
    }

    #[test]
    fn empty_doc_yields_no_groups() {
        assert!(balanced_groups(&[], 28_800).is_empty());
    }

    #[test]
    fn spec_example_220pct_doc_splits_into_three_balanced_groups() {
        // ~220% of the limit (§6 example) → 3 groups ≈ 73/73/74%, never 90/90/40.
        let limit = 28_800u32;
        let chunks = vec![920u32; 69]; // 63_480 tokens ≈ 220% of 28_800
        let groups = balanced_groups(&chunks, limit);
        assert_eq!(groups.len(), 3);
        let totals: Vec<u64> = groups.iter()
            .map(|r| chunks[r.clone()].iter().map(|&t| u64::from(t)).sum())
            .collect();
        let max = *totals.iter().max().unwrap();
        let min = *totals.iter().min().unwrap();
        assert!(max - min <= 920, "groups must be balanced within one chunk: {totals:?}");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn grouping_properties(chunks in proptest::collection::vec(1u32..=1024, 0..300)) {
            let limit = 28_800u32;
            let groups = balanced_groups(&chunks, limit);
            let total: u64 = chunks.iter().map(|&t| u64::from(t)).sum();
            if chunks.is_empty() {
                prop_assert!(groups.is_empty());
                return Ok(());
            }
            // Concatenation reproduces the original order exactly.
            let mut cursor = 0usize;
            for g in &groups {
                prop_assert_eq!(g.start, cursor);
                prop_assert!(g.end > g.start);
                cursor = g.end;
            }
            prop_assert_eq!(cursor, chunks.len());
            // Every group within the hard limit.
            for g in &groups {
                let sum: u64 = chunks[g.clone()].iter().map(|&t| u64::from(t)).sum();
                prop_assert!(sum <= u64::from(limit));
            }
            // Minimal group count: with chunks ≤1024 ≪ limit, the greedy
            // share-targeting partition always achieves ceil(total/limit).
            prop_assert_eq!(groups.len() as u64, total.div_ceil(u64::from(limit)).max(1));
        }
    }
}
```

- [ ] **Step 2: Run** — `cargo test -p mn-content context_group` — Expected: FAIL (module missing).

- [ ] **Step 3: Implement**

```rust
//! Balanced context-group splitting (spec §6, D8).
//!
//! voyage-context-3 accepts ≤32 000 tokens per inner list (one document's
//! chunks embedded together). Documents over 90% of that limit are split into
//! the minimum number of contiguous, roughly-equal-token groups. Grouping only
//! changes what context Voyage sees — chunk rows are unaffected.
//!
//! Token counts here are OUR BPE counts (`crate::tokens`), not Voyage's
//! tokenizer; the 10% headroom (28 800 vs 32 000) absorbs the divergence.

use std::ops::Range;

/// Voyage's per-document (inner list) token limit for contextualized embeddings.
pub const VOYAGE_CONTEXT_DOC_TOKEN_LIMIT: u32 = 32_000;

/// The grouping budget: 90% of the Voyage per-document limit.
#[must_use]
pub const fn context_group_limit() -> u32 {
    VOYAGE_CONTEXT_DOC_TOKEN_LIMIT / 10 * 9
}

/// Partition one document's contiguous chunk sequence into context groups.
///
/// Returns index ranges into `token_counts` (chunk order preserved; ranges
/// are contiguous and cover the whole slice). A document at or under `limit`
/// is one group. Larger documents split into `n = ceil(total/limit)` groups
/// with roughly equal token totals (greedy fill toward the remaining
/// per-group share, never exceeding `limit`).
///
/// # Panics
/// Debug-asserts that no single chunk exceeds `limit` — impossible by
/// construction with `max_tokens = 1024` (spec §6 edge case).
#[must_use]
pub fn balanced_groups(token_counts: &[u32], limit: u32) -> Vec<Range<usize>> {
    if token_counts.is_empty() {
        return Vec::new();
    }
    debug_assert!(
        token_counts.iter().all(|&t| t <= limit),
        "single chunk exceeds the context-group limit"
    );
    let total: u64 = token_counts.iter().map(|&t| u64::from(t)).sum();
    if total <= u64::from(limit) {
        return vec![0..token_counts.len()];
    }
    let mut n = usize::try_from(total.div_ceil(u64::from(limit))).unwrap_or(usize::MAX);
    loop {
        if let Some(groups) = try_partition(token_counts, limit, n) {
            return groups;
        }
        // Pathological granularity at the capacity boundary: a valid
        // n-partition under the hard limit may not exist; relax by one.
        n += 1;
    }
}

/// Try to partition into exactly `n` groups, each ≤ `limit`, each filled
/// greedily toward its share of the remaining total. `None` when the hard
/// limit forces leftover chunks past the last group.
fn try_partition(token_counts: &[u32], limit: u32, n: usize) -> Option<Vec<Range<usize>>> {
    let mut groups = Vec::with_capacity(n);
    let mut remaining_total: u64 = token_counts.iter().map(|&t| u64::from(t)).sum();
    let mut i = 0usize;
    for g in 0..n {
        if i >= token_counts.len() {
            return None;
        }
        let remaining_groups = u64::try_from(n - g).unwrap_or(1);
        let target = remaining_total.div_ceil(remaining_groups);
        let is_last = g + 1 == n;
        let start = i;
        let mut acc: u64 = 0;
        while i < token_counts.len() {
            let next = u64::from(token_counts[i]);
            if acc > 0 && acc + next > u64::from(limit) {
                break;
            }
            if acc > 0 && !is_last && acc >= target {
                break;
            }
            acc += next;
            i += 1;
        }
        remaining_total -= acc;
        groups.push(start..i);
    }
    (i == token_counts.len()).then_some(groups)
}
```

- [ ] **Step 4: Run** — `cargo test -p mn-content context_group` — Expected: PASS (256 proptest cases).

- [ ] **Step 5: Commit** — `git commit -am "feat(mn-content): balanced context-group splitting for voyage-context-3 (D8)"`

---

### Task 8: `ContextualizedVoyageEmbedder` (§4, §7)

**Files:**
- Create: `crates/mn-embedding/src/contextualized.rs`
- Modify: `crates/mn-embedding/src/voyage.rs` (make `voyage_http_client`, `DEFAULT_BASE_URL`, `Usage` `pub(crate)`)
- Modify: `crates/mn-embedding/src/lib.rs` (add `pub mod contextualized;`)
- Modify: `crates/mn-embedding/Cargo.toml` (add `wiremock` to `[dev-dependencies]`; check `[workspace.dependencies]` in the root `Cargo.toml` first and add it there if absent: `wiremock = "0.6"`)

- [ ] **Step 1: Write the wiremock test first** (in `contextualized.rs` `#[cfg(test)]`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ctx_response() -> serde_json::Value {
        // Two documents; items deliberately OUT of order at both levels to
        // pin the index-based reordering.
        serde_json::json!({
            "object": "list",
            "data": [
                { "object": "list", "index": 1, "data": [
                    { "object": "embedding", "index": 0, "embedding": [3.0, 3.0] }
                ]},
                { "object": "list", "index": 0, "data": [
                    { "object": "embedding", "index": 1, "embedding": [2.0, 2.0] },
                    { "object": "embedding", "index": 0, "embedding": [1.0, 1.0] }
                ]}
            ],
            "model": "voyage-context-3",
            "usage": { "total_tokens": 42 }
        })
    }

    #[tokio::test]
    async fn embeds_groups_and_restores_order() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/contextualizedembeddings"))
            .and(body_partial_json(serde_json::json!({
                "model": "voyage-context-3",
                "input_type": "document",
                "inputs": [["a", "b"], ["c"]],
                "output_dimension": 2,
                "output_dtype": "float",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(ctx_response()))
            .mount(&server)
            .await;

        let e = ContextualizedVoyageEmbedder::new("k", "voyage-context-3", 2, "float")
            .with_base_url(&server.uri());
        let out = e
            .embed_groups(
                vec![vec!["a".into(), "b".into()], vec!["c".into()]],
                InputType::Document,
            )
            .await
            .unwrap();
        assert_eq!(out.total_tokens, 42);
        assert_eq!(out.groups, vec![
            vec![vec![1.0, 1.0], vec![2.0, 2.0]],
            vec![vec![3.0, 3.0]],
        ]);
    }

    #[tokio::test]
    async fn query_embeds_as_single_chunk_document() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/contextualizedembeddings"))
            .and(body_partial_json(serde_json::json!({
                "input_type": "query",
                "inputs": [["how do I compile?"]],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [{ "object": "list", "index": 0, "data": [
                    { "object": "embedding", "index": 0, "embedding": [9.0, 9.0] }
                ]}],
                "model": "voyage-context-3",
                "usage": { "total_tokens": 5 }
            })))
            .mount(&server)
            .await;

        let e = ContextualizedVoyageEmbedder::new("k", "voyage-context-3", 2, "float")
            .with_base_url(&server.uri());
        let out = e.embed_queries(vec!["how do I compile?".into()]).await.unwrap();
        assert_eq!(out.vectors, vec![vec![9.0, 9.0]]);
        assert_eq!(out.total_tokens, 5);
    }

    #[tokio::test]
    async fn group_count_mismatch_is_a_decode_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/contextualizedembeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list", "data": [], "model": "voyage-context-3",
                "usage": { "total_tokens": 0 }
            })))
            .mount(&server)
            .await;
        let e = ContextualizedVoyageEmbedder::new("k", "voyage-context-3", 2, "float")
            .with_base_url(&server.uri());
        let err = e
            .embed_groups(vec![vec!["a".into()]], InputType::Document)
            .await
            .unwrap_err();
        assert!(matches!(err, VoyageError::Decode(_)));
    }
}
```

- [ ] **Step 2: Run** — `VOYAGE_API_KEY= cargo test -p mn-embedding contextualized` — Expected: FAIL (module missing; add wiremock dev-dep when the compiler asks).

- [ ] **Step 3: Implement**

In `voyage.rs`, change the three visibilities:
```rust
pub(crate) const DEFAULT_BASE_URL: &str = "https://api.voyageai.com";
pub(crate) fn voyage_http_client(timeout_secs: u64) -> reqwest::Client { ... }
#[derive(Deserialize)]
pub(crate) struct Usage { pub(crate) total_tokens: u64 }
```

`contextualized.rs`:

```rust
//! VoyageAI contextualized embeddings client (`POST /v1/contextualizedembeddings`).
//!
//! Each inner input list is one document's chunks, embedded together so every
//! chunk vector carries document-level context (spec §4). A query is a
//! single-chunk document: `inputs = [[query]]`, `input_type = "query"`.

use serde::{Deserialize, Serialize};

use crate::voyage::{
    voyage_http_client, InputType, Usage, VoyageError, DEFAULT_BASE_URL,
    DEFAULT_EMBED_TIMEOUT_SECS,
};

/// Output of a contextualized group-embedding call.
#[derive(Debug, Clone)]
pub struct GroupEmbedOutput {
    /// One vector list per input group, vectors in chunk order.
    pub groups: Vec<Vec<Vec<f32>>>,
    /// Total tokens Voyage reported consuming.
    pub total_tokens: u64,
    /// The model identifier echoed back by the API.
    pub model: String,
}

#[derive(Serialize)]
struct CtxRequest<'a> {
    model: &'a str,
    inputs: &'a [Vec<String>],
    input_type: &'a str,
    output_dimension: u32,
    output_dtype: &'a str,
}

#[derive(Deserialize)]
struct CtxItem {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
struct CtxGroup {
    data: Vec<CtxItem>,
    index: usize,
}

#[derive(Deserialize)]
struct CtxResponse {
    data: Vec<CtxGroup>,
    model: String,
    usage: Usage,
}

/// HTTP client for the VoyageAI contextualized-embeddings API.
#[derive(Clone)]
pub struct ContextualizedVoyageEmbedder {
    client: reqwest::Client,
    api_key: String,
    model: String,
    dim: u32,
    dtype: String,
    base_url: String,
}

impl ContextualizedVoyageEmbedder {
    /// Create a new contextualized embedder (e.g. model `"voyage-context-3"`).
    #[must_use]
    pub fn new(api_key: &str, model: &str, dim: u32, dtype: &str) -> Self {
        Self {
            client: voyage_http_client(DEFAULT_EMBED_TIMEOUT_SECS),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            dim,
            dtype: dtype.to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// Override the per-request timeout (seconds); rebuilds the inner client.
    #[must_use]
    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.client = voyage_http_client(secs);
        self
    }

    /// Override the base URL (for tests / local proxies).
    #[must_use]
    pub fn with_base_url(mut self, base: &str) -> Self {
        base.trim_end_matches('/').clone_into(&mut self.base_url);
        self
    }

    /// Embed `groups` (one inner list per document; caller enforces the
    /// per-group 28 800-token budget and the per-request limits: ≤1 000
    /// inputs, ≤120 K tokens, ≤16 K chunks).
    ///
    /// Returns groups in input order with vectors in chunk order, regardless
    /// of how the API orders `data` items.
    ///
    /// # Errors
    /// [`VoyageError`] on transport failure, non-2xx status, or a response
    /// whose group/chunk counts don't match the request.
    pub async fn embed_groups(
        &self,
        groups: Vec<Vec<String>>,
        input_type: InputType,
    ) -> Result<GroupEmbedOutput, VoyageError> {
        let expected: Vec<usize> = groups.iter().map(Vec::len).collect();
        let body = CtxRequest {
            model: &self.model,
            inputs: &groups,
            input_type: input_type.as_str(),
            output_dimension: self.dim,
            output_dtype: &self.dtype,
        };
        let resp = self
            .client
            .post(format!("{}/v1/contextualizedembeddings", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| VoyageError::Http(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(VoyageError::Status { status: status.as_u16(), body });
        }
        let mut parsed: CtxResponse = resp
            .json()
            .await
            .map_err(|e| VoyageError::Decode(e.to_string()))?;

        if parsed.data.len() != expected.len() {
            return Err(VoyageError::Decode(format!(
                "expected {} embedding groups, got {}",
                expected.len(),
                parsed.data.len()
            )));
        }
        parsed.data.sort_by_key(|g| g.index);
        let mut out_groups = Vec::with_capacity(parsed.data.len());
        for (gi, mut g) in parsed.data.into_iter().enumerate() {
            if g.data.len() != expected[gi] {
                return Err(VoyageError::Decode(format!(
                    "group {gi}: expected {} embeddings, got {}",
                    expected[gi],
                    g.data.len()
                )));
            }
            g.data.sort_by_key(|d| d.index);
            out_groups.push(g.data.into_iter().map(|d| d.embedding).collect());
        }
        Ok(GroupEmbedOutput {
            groups: out_groups,
            total_tokens: parsed.usage.total_tokens,
            model: parsed.model,
        })
    }

    /// Embed query texts, each as its own single-chunk document
    /// (`input_type = "query"`), returning one vector per text in order.
    ///
    /// # Errors
    /// See [`Self::embed_groups`].
    pub async fn embed_queries(
        &self,
        texts: Vec<String>,
    ) -> Result<crate::voyage::EmbedOutput, VoyageError> {
        let groups: Vec<Vec<String>> = texts.into_iter().map(|t| vec![t]).collect();
        let out = self.embed_groups(groups, InputType::Query).await?;
        let mut vectors = Vec::with_capacity(out.groups.len());
        for mut g in out.groups {
            let Some(v) = g.pop() else {
                return Err(VoyageError::Decode("empty query embedding group".into()));
            };
            vectors.push(v);
        }
        Ok(crate::voyage::EmbedOutput {
            vectors,
            total_tokens: out.total_tokens,
            model: out.model,
        })
    }
}
```

`InputType::as_str` is `const fn` but private — make it `pub(crate)` in `voyage.rs`. `EmbedOutput`'s fields are already `pub`.

- [ ] **Step 4: Run** — `VOYAGE_API_KEY= cargo test -p mn-embedding` — Expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat(mn-embedding): ContextualizedVoyageEmbedder for /v1/contextualizedembeddings"`

---

### Task 9: client.rs — general vs code embed paths (§7, §11)

**Files:**
- Modify: `crates/mn-embedding/src/client.rs`

Add general/code-aware entry points; keep the legacy `embed()` working unchanged (existing callers migrate in Tasks 15–17, then Task 17 deletes it).

- [ ] **Step 1: Write failing wiremock tests** (in `client.rs` tests; the file may already have a test module — extend it)

```rust
#[tokio::test]
async fn embed_code_sends_type_code_to_server() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/embeddings"))
        .and(wiremock::matchers::body_partial_json(serde_json::json!({
            "type": "code", "input": ["fn main() {}"], "input_type": "query",
        })))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "voyage-code-3@1",
            "embeddings": [[1.0, 2.0]],
            "usage": { "total_tokens": 3 },
            "rate": { "hour": {"limit":1,"remaining":1,"reset_at":""},
                      "day":  {"limit":1,"remaining":1,"reset_at":""} },
        })))
        .mount(&server)
        .await;
    let out = embed_code(
        vec!["fn main() {}".into()],
        InputType::Query,
        EmbedSource::Server { base_url: &server.uri(), bearer: None, no_global_limit: false },
    )
    .await
    .unwrap();
    assert_eq!(out.vectors, vec![vec![1.0, 2.0]]);
}

#[tokio::test]
async fn embed_general_groups_sends_nested_input_with_type_general() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/embeddings"))
        .and(wiremock::matchers::body_partial_json(serde_json::json!({
            "type": "general", "input": [["a", "b"], ["c"]], "input_type": "document",
        })))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "voyage-context-3@1",
            "embeddings": [[1.0], [2.0], [3.0]],
            "usage": { "total_tokens": 9 },
            "rate": { "hour": {"limit":1,"remaining":1,"reset_at":""},
                      "day":  {"limit":1,"remaining":1,"reset_at":""} },
        })))
        .mount(&server)
        .await;
    let out = embed_general_groups(
        vec![vec!["a".into(), "b".into()], vec!["c".into()]],
        GeneralEmbedSource::Server { base_url: &server.uri(), bearer: None, no_global_limit: false },
    )
    .await
    .unwrap();
    // The server returns row-per-chunk in input order; the client re-nests by group sizes.
    assert_eq!(out.groups, vec![vec![vec![1.0], vec![2.0]], vec![vec![3.0]]]);
}
```

- [ ] **Step 2: Run** — `VOYAGE_API_KEY= cargo test -p mn-embedding client` — Expected: FAIL.

- [ ] **Step 3: Implement** (added to `client.rs`; existing `EmbedSource`/`embed`/`embed_once` untouched)

```rust
use crate::contextualized::ContextualizedVoyageEmbedder;

/// Where to perform a GENERAL (voyage-context-3) embedding.
#[derive(Clone, Copy)]
pub enum GeneralEmbedSource<'a> {
    /// BYOK: call the contextualized endpoint directly.
    Byok(&'a ContextualizedVoyageEmbedder),
    /// Proxy through our server's `/v1/embeddings` with `type=general`.
    Server {
        /// Base URL of the `midnight-manual-server` deployment.
        base_url: &'a str,
        /// Optional bearer token for tier-based limits.
        bearer: Option<&'a str>,
        /// Admin-only opt-out from the server's site-wide token cap.
        no_global_limit: bool,
    },
}

/// Nested vectors + token usage for a group-embedding request.
#[derive(Debug)]
pub struct EmbeddedGroups {
    /// One vector list per input group, vectors in chunk order.
    pub groups: Vec<Vec<Vec<f32>>>,
    /// Total tokens Voyage reported consuming.
    pub total_tokens: u64,
}

#[derive(serde::Serialize)]
struct ServerEmbedBody<'a, I: serde::Serialize> {
    input: &'a I,
    input_type: &'a str,
    #[serde(rename = "type")]
    embed_type: &'a str,
    no_global_limit: bool,
}

/// POST one body to the server's `/v1/embeddings` and decode the flat reply.
async fn server_embed_once<I: serde::Serialize>(
    base_url: &str,
    bearer: Option<&str>,
    body: &ServerEmbedBody<'_, I>,
) -> Result<Embedded, VoyageError> {
    let client = crate::voyage::voyage_http_client(DEFAULT_EMBED_TIMEOUT_SECS);
    let mut req = client
        .post(format!("{}/v1/embeddings", base_url.trim_end_matches('/')))
        .json(body);
    if let Some(b) = bearer {
        req = req.bearer_auth(b);
    }
    let resp = req.send().await.map_err(|e| VoyageError::Http(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(VoyageError::Status {
            status: status.as_u16(),
            body: resp.text().await.unwrap_or_default(),
        });
    }
    let parsed: ServerResp = resp.json().await.map_err(|e| VoyageError::Decode(e.to_string()))?;
    Ok(Embedded { vectors: parsed.embeddings, total_tokens: parsed.usage.total_tokens })
}

/// Embed query/document texts with the GENERAL model (voyage-context-3),
/// each text as its own single-chunk document. Retries like [`embed`].
///
/// # Errors
/// Returns the last [`VoyageError`] if every attempt fails.
pub async fn embed_general(
    texts: Vec<String>,
    input_type: InputType,
    src: GeneralEmbedSource<'_>,
) -> Result<Embedded, VoyageError> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = match src {
            GeneralEmbedSource::Byok(e) => match input_type {
                InputType::Query => e.embed_queries(texts.clone()).await.map(|o| Embedded {
                    vectors: o.vectors,
                    total_tokens: o.total_tokens,
                }),
                InputType::Document => {
                    let groups: Vec<Vec<String>> =
                        texts.clone().into_iter().map(|t| vec![t]).collect();
                    e.embed_groups(groups, InputType::Document).await.map(|o| Embedded {
                        vectors: o.groups.into_iter().flatten().collect(),
                        total_tokens: o.total_tokens,
                    })
                }
            },
            GeneralEmbedSource::Server { base_url, bearer, no_global_limit } => {
                server_embed_once(
                    base_url,
                    bearer,
                    &ServerEmbedBody {
                        input: &texts,
                        input_type: input_type_str(input_type),
                        embed_type: "general",
                        no_global_limit,
                    },
                )
                .await
            }
        };
        match result {
            Ok(out) => return Ok(out),
            Err(e) if attempt < MAX_EMBED_ATTEMPTS && is_retryable(&e) => {
                backoff_sleep(attempt, &e).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Embed caller-provided context groups with the GENERAL model (ingest path).
/// BYOK hits the contextualized endpoint with nested inputs; server-proxy
/// sends nested `input` with `type=general` and re-nests the flat reply.
///
/// # Errors
/// Returns the last [`VoyageError`] if every attempt fails.
pub async fn embed_general_groups(
    groups: Vec<Vec<String>>,
    src: GeneralEmbedSource<'_>,
) -> Result<EmbeddedGroups, VoyageError> {
    let sizes: Vec<usize> = groups.iter().map(Vec::len).collect();
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = match src {
            GeneralEmbedSource::Byok(e) => e
                .embed_groups(groups.clone(), InputType::Document)
                .await
                .map(|o| EmbeddedGroups { groups: o.groups, total_tokens: o.total_tokens }),
            GeneralEmbedSource::Server { base_url, bearer, no_global_limit } => {
                server_embed_once(
                    base_url,
                    bearer,
                    &ServerEmbedBody {
                        input: &groups,
                        input_type: "document",
                        embed_type: "general",
                        no_global_limit,
                    },
                )
                .await
                .and_then(|flat| renest(flat, &sizes))
            }
        };
        match result {
            Ok(out) => return Ok(out),
            Err(e) if attempt < MAX_EMBED_ATTEMPTS && is_retryable(&e) => {
                backoff_sleep(attempt, &e).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Embed texts with the CODE model (voyage-code-3, flat endpoint).
/// BYOK reuses the flat [`VoyageEmbedder`]; server-proxy sends `type=code`.
///
/// # Errors
/// Returns the last [`VoyageError`] if every attempt fails.
pub async fn embed_code(
    texts: Vec<String>,
    input_type: InputType,
    src: EmbedSource<'_>,
) -> Result<Embedded, VoyageError> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = match src {
            EmbedSource::Byok(v) => v.embed(texts.clone(), input_type).await.map(|o| Embedded {
                vectors: o.vectors,
                total_tokens: o.total_tokens,
            }),
            EmbedSource::Server { base_url, bearer, no_global_limit } => {
                server_embed_once(
                    base_url,
                    bearer,
                    &ServerEmbedBody {
                        input: &texts,
                        input_type: input_type_str(input_type),
                        embed_type: "code",
                        no_global_limit,
                    },
                )
                .await
            }
        };
        match result {
            Ok(out) => return Ok(out),
            Err(e) if attempt < MAX_EMBED_ATTEMPTS && is_retryable(&e) => {
                backoff_sleep(attempt, &e).await;
            }
            Err(e) => return Err(e),
        }
    }
}

const fn input_type_str(t: InputType) -> &'static str {
    match t {
        InputType::Query => "query",
        InputType::Document => "document",
    }
}

/// Re-nest a flat row-per-chunk reply into the caller's group sizes.
fn renest(flat: Embedded, sizes: &[usize]) -> Result<EmbeddedGroups, VoyageError> {
    let total: usize = sizes.iter().sum();
    if flat.vectors.len() != total {
        return Err(VoyageError::Decode(format!(
            "expected {total} vectors, got {}",
            flat.vectors.len()
        )));
    }
    let mut it = flat.vectors.into_iter();
    let groups = sizes.iter().map(|&n| it.by_ref().take(n).collect()).collect();
    Ok(EmbeddedGroups { groups, total_tokens: flat.total_tokens })
}

async fn backoff_sleep(attempt: usize, e: &VoyageError) {
    let backoff = std::time::Duration::from_secs(1u64 << (attempt - 1));
    tracing::warn!(
        attempt,
        max = MAX_EMBED_ATTEMPTS,
        backoff_secs = backoff.as_secs(),
        error = %e,
        "Voyage embed failed; retrying after backoff",
    );
    tokio::time::sleep(backoff).await;
}
```

Note: the existing `embed_once` server branch builds its own request body — leave it; refactor the existing `embed()` retry loop to use `backoff_sleep` if clippy flags duplication.

- [ ] **Step 4: Run** — `VOYAGE_API_KEY= cargo test -p mn-embedding && cargo clippy -p mn-embedding --all-targets -- -D warnings` — Expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat(mn-embedding): general/code embed entry points with server type routing + nested groups"`

---

### Task 10: Migration 0011 + store entities (§8)

**Files:**
- Create: `crates/mn-store/migrations/0011_dual_embeddings.sql`
- Modify: `crates/mn-core/src/types.rs` (`SourceVersion` gains `code_embedding_model_id`)
- Modify: `crates/mn-store/src/entities/source_version.rs` (insert/select the new column)
- Modify: `crates/mn-store/src/entities/chunk.rs` (`NewChunk` gains `code_embedding`; INSERT and carry-forward column lists)

NOTE: migration `0010_telemetry_search_daily.sql` exists on this branch's remote (`0ab3e4e`) but may not be in your checkout. If `git pull` brings it in, 0011 is correct; if this plan lands before it, renumber to 0010. NEVER renumber or edit an already-applied migration.

- [ ] **Step 1: Write the migration**

```sql
-- 0011 — contextualized + dual code embeddings (spec 2026-06-10).
--
-- 1. Register voyage-context-3 as the general corpus model. It becomes the
--    newest embedding_model row, so get_active()'s fresh-DB fallback resolves
--    it once existing source_versions are deactivated below.
INSERT INTO embedding_model (name, revision, dim, provider)
VALUES ('voyage-context-3', 1, 1024, 'voyageai')
ON CONFLICT (name, revision) DO NOTHING;

-- 2. Second, code-specialised embedding per chunk (voyage-code-3). Nullable:
--    only DocumentKind::Code chunks of opted-in sources carry it.
ALTER TABLE chunk ADD COLUMN code_embedding vector(1024);

-- Partial HNSW keeps the code ANN graph restricted to code chunks.
CREATE INDEX idx_chunk_code_embedding ON chunk
    USING hnsw (code_embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64)
    WHERE code_embedding IS NOT NULL;

-- 3. Which code model a version's code_embeddings use. NULL ⇔ code
--    embeddings disabled (or no code files) for that version.
ALTER TABLE source_version
    ADD COLUMN code_embedding_model_id uuid REFERENCES embedding_model(id);

-- 4. Extend the cross-table model invariant: a chunk carrying a
--    code_embedding requires its source_version to declare a code model.
--    (CREATE OR REPLACE in a NEW migration — 0002 is applied and immutable.)
CREATE OR REPLACE FUNCTION check_chunk_embedding_model_match() RETURNS trigger AS $fn$
DECLARE
    sv_model uuid;
    sv_code_model uuid;
BEGIN
    SELECT embedding_model_id, code_embedding_model_id
        INTO sv_model, sv_code_model
        FROM source_version WHERE id = NEW.source_version_id;
    IF NEW.embedding_model_id <> sv_model THEN
        RAISE EXCEPTION 'chunk.embedding_model_id (%) does not match source_version.embedding_model_id (%) for source_version %',
            NEW.embedding_model_id, sv_model, NEW.source_version_id;
    END IF;
    IF NEW.code_embedding IS NOT NULL AND sv_code_model IS NULL THEN
        RAISE EXCEPTION 'chunk has code_embedding but source_version % has no code_embedding_model_id',
            NEW.source_version_id;
    END IF;
    RETURN NEW;
END;
$fn$ LANGUAGE plpgsql;

DROP TRIGGER trg_chunk_embedding_model_match ON chunk;
CREATE TRIGGER trg_chunk_embedding_model_match
    BEFORE INSERT OR UPDATE OF embedding_model_id, source_version_id, code_embedding ON chunk
    FOR EACH ROW EXECUTE FUNCTION check_chunk_embedding_model_match();

-- 5. Hard cutover (pre-1.0, D-summary): the corpus is re-ingested against
--    voyage-context-3. Deactivate every source_version (so boot resolution
--    falls back to the newest model = voyage-context-3) and clear vectors
--    that no longer match any active model.
UPDATE source_version SET is_active = false, status = 'inactive'
    WHERE is_active = true;
UPDATE chunk SET embedding = NULL WHERE embedding IS NOT NULL;
```

- [ ] **Step 2: Extend `SourceVersion` in `mn-core/src/types.rs`**

Find `pub struct SourceVersion` and after `embedding_model_id: Uuid` add:

```rust
    /// Code-embedding model for this version's `chunk.code_embedding` vectors.
    /// `None` ⇔ code embeddings disabled (or no code files) for this version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_embedding_model_id: Option<Uuid>,
```

- [ ] **Step 3: Update `source_version.rs` entity**

Every `SELECT` column list gains `code_embedding_model_id`; the row struct gains `code_embedding_model_id: Option<Uuid>`; the `insert`/create function gains a `code_embedding_model_id: Option<Uuid>` parameter bound in the `INSERT` column list. Compile errors are the worklist: `cargo build -p mn-store -p mn-server` after the change and fix each call site (admin_ingest passes `None` until Task 14).

- [ ] **Step 4: Update `chunk.rs` entity**

`NewChunk` (line ~93 region) gains:

```rust
    /// Optional code-model vector (voyage-code-3); None for non-code chunks.
    pub code_embedding: Option<&'a pgvector::Vector>,
```

Add `code_embedding` to the `INSERT INTO chunk (...)` column list and `.bind(c.code_embedding)` in positional order. The carry-forward path (the `INSERT ... SELECT`-style column lists at lines ~231 and ~250) gains `code_embedding` in BOTH lists so carried chunks keep their code vectors. Grep `chunk.rs` for every explicit column list (`heading_path, symbol_path,`) and extend each consistently. Reads (`ChunkRow` structs) do NOT need the column — nothing reads vectors back through entities.

- [ ] **Step 5: Build + unit-test** — `cargo build --workspace && cargo test --workspace` (DB-backed tests verify in CI; locally this validates compile + non-DB tests). Run `sqlx migrate info --source crates/mn-store/migrations` only if a local DATABASE_URL exists (it usually doesn't in the sandbox — skip).

- [ ] **Step 6: Commit** — `git commit -am "feat(mn-store): migration 0011 — code_embedding column, partial HNSW, code model on source_version, hard cutover"`

---

### Task 11: Server boot — code model + contextualized embedder + models route (§7, §8)

**Files:**
- Create: `crates/mn-server/src/code_model.rs`
- Modify: `crates/mn-server/src/lib.rs` (or `main.rs` module decls — match where `corpus_model` is declared: `pub mod code_model;`)
- Modify: `crates/mn-server/src/config.rs` (ServerConfig — find the `voyage_model` field and mirror it)
- Modify: `crates/mn-server/src/app.rs` (AppState)
- Modify: `crates/mn-server/src/main.rs` (boot wiring)
- Modify: `crates/mn-server/src/routes/models.rs` (active-model response gains `code`)
- Modify: `crates/mn-cli/src/commands/models.rs` (`fetch_active` response struct gains optional `code`)

- [ ] **Step 1: `code_model.rs`** (mirror `corpus_model.rs`)

```rust
//! The corpus's active CODE embedding model (voyage-code-3 family), resolved
//! at boot from config (`MIDNIGHT_MANUAL_CODE_MODEL`, default
//! "voyage-code-3@1") against the embedding_model registry. Unlike the
//! general corpus model this is config-pinned, not activity-derived: the code
//! column always encodes with exactly the configured model.
use mn_store::entities::embedding_model;
use sqlx::PgPool;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// The code-embedding model the corpus's `code_embedding` column uses.
#[derive(Debug, Clone)]
pub struct CodeModel {
    /// Wire id, e.g. "voyage-code-3@1".
    pub wire: String,
    /// Primary key, used to gate code-vector ANN by `sv.code_embedding_model_id`.
    pub id: Uuid,
    /// Vector dimension, used to validate inbound code query vectors.
    pub dim: usize,
}

/// Shared handle stored in AppState. `None` until resolved.
pub type Shared = Arc<RwLock<Option<CodeModel>>>;

/// Resolve `wire` ("name@revision") against the registry.
///
/// # Errors
/// Returns an error when the wire id does not parse or is not registered.
pub async fn resolve(pool: &PgPool, wire: &str) -> anyhow::Result<CodeModel> {
    let (name, rev) = wire
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("code model wire id `{wire}` is not name@revision"))?;
    let revision: i32 = rev.parse()?;
    let m = embedding_model::get_by_name_revision(pool, name, revision).await?;
    Ok(CodeModel {
        wire: format!("{}@{}", m.name, m.revision),
        id: m.id,
        dim: usize::try_from(m.dim)
            .map_err(|_| anyhow::anyhow!("code model dim {} out of range", m.dim))?,
    })
}
```

- [ ] **Step 2: ServerConfig fields**

Open `crates/mn-server/src/config.rs`, locate `voyage_model` (the flat-embedder model name used at `main.rs:71`) and add two sibling fields following the exact same env-var/default pattern used there:

- `voyage_context_model: String` — env `MIDNIGHT_MANUAL_VOYAGE_CONTEXT_MODEL`, default `"voyage-context-3"` (raw Voyage model name for the server-side contextualized embedder).
- `code_model_wire: String` — env `MIDNIGHT_MANUAL_CODE_MODEL`, default `"voyage-code-3@1"` (registry wire id).

`voyage_model` itself stays `"voyage-code-3"` — it becomes the `type=code` embedder.

- [ ] **Step 3: AppState + boot**

In `app.rs` after `voyage`:

```rust
    /// Server-side contextualized (general) Voyage embedder for
    /// `POST /v1/embeddings` with `type=general`. `None` when
    /// `VOYAGE_API_KEY` is unset (endpoint 503s).
    pub voyage_ctx: Option<std::sync::Arc<mn_embedding::contextualized::ContextualizedVoyageEmbedder>>,
    /// The corpus's code-embedding model, resolved at boot from config.
    /// `None` when unresolved — code_mode searches then 503.
    pub code_model: crate::code_model::Shared,
```

In `main.rs`, next to the existing `voyage` construction (line ~68):

```rust
let voyage_ctx = cfg.voyage_api_key.as_ref().map(|k| {
    std::sync::Arc::new(mn_embedding::contextualized::ContextualizedVoyageEmbedder::new(
        k,
        &cfg.voyage_context_model,
        cfg.voyage_output_dimension,
        &cfg.voyage_output_dtype,
    ))
});
let code_model: mn_server::code_model::Shared = std::sync::Arc::new(std::sync::RwLock::new(
    match mn_server::code_model::resolve(&pool, &cfg.code_model_wire).await {
        Ok(cm) => {
            tracing::info!(code_model = %cm.wire, "resolved code embedding model");
            Some(cm)
        }
        Err(e) => {
            tracing::warn!(error = %e, "code model unresolved; code_mode searches will 503");
            None
        }
    },
));
```

and pass both into the `AppState` literal. Fix every other `AppState { ... }` construction (integration-test helpers, `app.rs` builders) — `cargo build --workspace` finds them; tests can use `code_model: std::sync::Arc::new(std::sync::RwLock::new(None))` and `voyage_ctx: None` unless they exercise the new paths.

- [ ] **Step 4: models route + CLI fetch_active**

In `routes/models.rs`, find the active-model response struct and add:

```rust
    /// The corpus's code-embedding model, when resolved. `null` means code
    /// search is unavailable server-side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<ActiveModelInfo>,
```

where `ActiveModelInfo` is the existing `{name, revision, dim, provider}` shape the route already returns for the general model (reuse the struct; if the route serializes fields inline, factor the struct). Populate from `state.code_model` read-lock snapshot (look up provider via `embedding_model::get_by_id` if the response includes it, else just name/revision/dim from the snapshot — match the existing response's fields exactly).

In `mn-cli/src/commands/models.rs`, the `fetch_active` deserialization struct gains `#[serde(default)] pub code: Option<ActiveCode>` with `struct ActiveCode { pub name: String, pub revision: i32 }` (plus whatever extra fields the response carries, defaulted).

- [ ] **Step 5: Run** — `cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets --all-features -- -D warnings` — Expected: PASS.

- [ ] **Step 6: Commit** — `git commit -am "feat(mn-server): boot-resolved code model + server-side contextualized embedder + models/active code info"`

---

### Task 12: `POST /v1/embeddings` — `type` + nested input (§9)

**Files:**
- Modify: `crates/mn-server/src/routes/embeddings.rs`

- [ ] **Step 1: Request shape**

```rust
/// Whether the request targets the general (contextualized) or code model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedType {
    /// voyage-context-3 via /v1/contextualizedembeddings. The default.
    #[default]
    General,
    /// voyage-code-3 via the flat /v1/embeddings.
    Code,
}

/// Flat texts, or caller-provided context groups (general type only).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingsInput {
    /// Each string is its own single-chunk document (correct for queries).
    Flat(Vec<String>),
    /// Caller-provided context groups (server-proxy ingestion).
    Nested(Vec<Vec<String>>),
}

impl EmbeddingsInput {
    fn is_empty(&self) -> bool {
        match self {
            Self::Flat(v) => v.is_empty(),
            Self::Nested(g) => g.iter().all(Vec::is_empty),
        }
    }
    fn flat_len(&self) -> usize {
        match self {
            Self::Flat(v) => v.len(),
            Self::Nested(g) => g.iter().map(Vec::len).sum(),
        }
    }
}
```

`EmbeddingsRequest.input` becomes `pub input: EmbeddingsInput` (serde `default` removed — derive needs a manual `Default` or drop the default and let a missing field be a 400; choose: implement `Default for EmbeddingsInput { Self::Flat(Vec::new()) }` and keep `#[serde(default)]`). Add `#[serde(rename = "type", default)] pub embed_type: EmbedType,`.

- [ ] **Step 2: Handler routing** (replacing steps 1, 3, 4, 8 of the existing handler; numbered comments updated)

```rust
// 1. The embedder for the requested type must be configured.
//    (Both are None iff VOYAGE_API_KEY is unset.)
if state.voyage.is_none() && state.voyage_ctx.is_none() {
    return error::service_unavailable("server embedding is not configured (no VOYAGE_API_KEY)", rid);
}

// 3. Shape validation.
if req.input.flat_len() > MAX_INPUTS {
    return error::payload_too_large(
        format!("input exceeds {MAX_INPUTS} texts; batch client-side"), rid);
}
if matches!(req.embed_type, EmbedType::Code) && matches!(req.input, EmbeddingsInput::Nested(_)) {
    return error::into_response(
        CoreError::builder(ErrorCode::InvalidRequest)
            .message("nested input is only valid with type=general")
            .remediation("flatten `input` to a string array for type=code")
            .build(),
        rid,
    );
}
// Per-group budget (general/nested): ≈ tokens via the ~4-bytes/token estimate;
// the 90% headroom on the real Voyage limit absorbs the estimate's slack.
if let EmbeddingsInput::Nested(groups) = &req.input {
    for (i, g) in groups.iter().enumerate() {
        if char_estimate(g) > u64::from(mn_content_group_limit()) {
            return error::payload_too_large(
                format!("input group {i} exceeds the per-document context limit; split it"), rid);
        }
    }
}

// 4. Snapshot the model for the requested type; enforce the optional pin.
let resolved_wire: String = match req.embed_type {
    EmbedType::General => {
        let Some(cm) = state.corpus_model.read().expect("corpus_model lock poisoned").clone()
        else {
            return error::service_unavailable("server has no resolved corpus_model; check boot logs", rid);
        };
        cm.wire
    }
    EmbedType::Code => {
        let Some(cm) = state.code_model.read().expect("code_model lock poisoned").clone()
        else {
            return error::service_unavailable("server has no resolved code model; check boot logs", rid);
        };
        cm.wire
    }
};
if let Some(client_model) = req.model.as_ref() {
    if client_model != &resolved_wire {
        // ... existing 409 EmbeddingModelMismatch builder, with resolved_wire ...
    }
}
```

Add a tiny helper (mn-server must NOT grow an mn-content dependency just for a constant — check `Cargo.toml`; if mn-content is not already a dependency, inline the constant):

```rust
/// 90% of voyage-context-3's 32K per-document limit (mirrors
/// mn_content::context_group::context_group_limit; inlined to avoid the dep).
const fn mn_content_group_limit() -> u32 {
    32_000 / 10 * 9
}
```

Step 8 (the Voyage call) becomes:

```rust
let out = match (&req.embed_type, &req.input) {
    (EmbedType::Code, EmbeddingsInput::Flat(texts)) => {
        let Some(voyage) = state.voyage.clone() else {
            return error::service_unavailable("code embedder not configured", rid);
        };
        voyage.embed(texts.clone(), input_type).await.map(|o| (o.vectors, o.total_tokens))
    }
    (EmbedType::General, input) => {
        let Some(ctx) = state.voyage_ctx.clone() else {
            return error::service_unavailable("general embedder not configured", rid);
        };
        let groups: Vec<Vec<String>> = match input {
            EmbeddingsInput::Flat(texts) => texts.iter().cloned().map(|t| vec![t]).collect(),
            EmbeddingsInput::Nested(g) => g.clone(),
        };
        ctx.embed_groups(groups, input_type)
            .await
            .map(|o| (o.groups.into_iter().flatten().collect::<Vec<_>>(), o.total_tokens))
    }
    (EmbedType::Code, EmbeddingsInput::Nested(_)) => unreachable!("rejected above"),
};
let (vectors, total_tokens) = match out {
    Ok(v) => v,
    Err(e) => { /* existing release-reservation + 502 path */ }
};
```

Response: `model: resolved_wire`, `embeddings: vectors` (flattened row-per-chunk in input order — already the case). `char_estimate` needs a `&[String]` — it already takes that; for nested, estimate over the flattened iterator (add a small adapter or `groups.iter().flatten().cloned().collect::<Vec<_>>()` once and reuse for both estimate and per-group checks).

- [ ] **Step 3: Unit tests** (same file)

```rust
#[test]
fn embed_type_deserializes_with_default() {
    let r: EmbeddingsRequest =
        serde_json::from_value(serde_json::json!({"input": ["x"]})).unwrap();
    assert_eq!(r.embed_type, EmbedType::General);
    assert!(matches!(r.input, EmbeddingsInput::Flat(ref v) if v.len() == 1));

    let r: EmbeddingsRequest = serde_json::from_value(
        serde_json::json!({"input": [["a","b"]], "type": "code"}),
    ).unwrap();
    assert_eq!(r.embed_type, EmbedType::Code);
    assert!(matches!(r.input, EmbeddingsInput::Nested(_)));
}
```

Contract tests (409/413/400 paths) live with the existing embeddings-route integration tests — find them (`grep -rn "v1/embeddings" crates/mn-server/tests`) and add: `type=code` + nested → 400; nested group over the char-estimate limit → 413; `model` pin mismatching the type-resolved model → 409. These are `--features integration` tests verified in CI.

- [ ] **Step 4: Run** — `cargo test -p mn-server && cargo clippy -p mn-server --all-targets --all-features -- -D warnings` — Expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat(mn-server): /v1/embeddings type=general|code routing with nested context groups"`

---

### Task 13: `POST /v1/search` — `code_mode` (§10)

**Files:**
- Modify: `crates/mn-server/src/routes/search.rs`

- [ ] **Step 1: Request/response types**

```rust
/// Whether the code-vector ranked list joins the RRF pool (D5/D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeMode {
    /// Fuse the code-vector list alongside the general lists. Default for
    /// hybrid/vector modes.
    On,
    /// General retrieval only (pre-dual-embeddings behavior). Forced for fts.
    Off,
    /// Code-vector list replaces the general vector list.
    Exclusive,
}
```

`SearchRequest` gains:

```rust
    /// Code-vector fusion mode. Defaults to `on` for hybrid/vector, forced
    /// `off` for fts (where `on`/`exclusive` is a 400).
    #[serde(default)]
    pub code_mode: Option<CodeMode>,
    /// The code-embedding model wire id the client used for `code_vector`s.
    /// REQUIRED when the effective code_mode != off.
    #[serde(default)]
    pub client_code_embedding_model: Option<String>,
    /// Single-query convenience form: the voyage-code-3 embedding for `query`.
    #[serde(default)]
    pub code_vector: Option<Vec<f32>>,
```

`QueryPair` gains:

```rust
    /// The pre-computed code-model embedding; required iff code_mode != off.
    #[serde(default)]
    pub code_vector: Vec<f32>,
```

`PerQueryRecord` gains:

```rust
    /// Code-vector candidate count for this query.
    pub code_vector_candidates: usize,
    /// Code-vector latency in milliseconds.
    pub code_vector_latency_ms: f64,
```

`SearchMetadata` gains:

```rust
    /// The effective code_mode applied (request value or mode-derived default).
    pub code_mode: CodeMode,
```

- [ ] **Step 2: Effective-mode resolution + unit tests**

```rust
/// Resolve the effective code mode for a request (D6 defaults; spec §10.2).
/// `Err(())` = fts with an explicit on/exclusive (400).
const fn effective_code_mode(
    mode: SearchMode,
    requested: Option<CodeMode>,
) -> Result<CodeMode, ()> {
    match (mode, requested) {
        (SearchMode::Fts, None | Some(CodeMode::Off)) => Ok(CodeMode::Off),
        (SearchMode::Fts, Some(_)) => Err(()),
        (_, Some(m)) => Ok(m),
        (_, None) => Ok(CodeMode::On),
    }
}

#[cfg(test)]
mod code_mode_tests {
    use super::*;
    #[test]
    fn matrix() {
        use CodeMode::*;
        use SearchMode::*;
        assert_eq!(effective_code_mode(Hybrid, None), Ok(On));
        assert_eq!(effective_code_mode(Vector, None), Ok(On));
        assert_eq!(effective_code_mode(Fts, None), Ok(Off));
        assert_eq!(effective_code_mode(Hybrid, Some(Off)), Ok(Off));
        assert_eq!(effective_code_mode(Vector, Some(Exclusive)), Ok(Exclusive));
        assert_eq!(effective_code_mode(Fts, Some(Off)), Ok(Off));
        assert_eq!(effective_code_mode(Fts, Some(On)), Err(()));
        assert_eq!(effective_code_mode(Fts, Some(Exclusive)), Err(()));
    }
}
```

- [ ] **Step 3: Handler wiring**

Top of handler, after `run_vector`/`run_fts`:

```rust
let Ok(code_mode) = effective_code_mode(req.mode, req.code_mode) else {
    return error::into_response(
        CoreError::builder(ErrorCode::InvalidRequest)
            .message("code_mode on/exclusive is incompatible with mode=fts")
            .remediation("drop code_mode, or use mode=hybrid/vector")
            .build(),
        rid,
    );
};
let run_general_vector = run_vector && !matches!(code_mode, CodeMode::Exclusive);
let run_code_vector = run_vector && !matches!(code_mode, CodeMode::Off);
```

Replace `run_vector` with `run_general_vector` in: the general `client_embedding_model` requirement guard, the per-query general dim check, the per-query `vector_search` gate, and the `ranked_lists.push(vector_ids)` gate. (`normalize_queries`' fts text-only case is unchanged; extend the convenience-form arm to carry `code_vector: req.code_vector.clone().unwrap_or_default()` and the `queries`-array path already deserializes it. The "requires both query and vector" error stays keyed on the GENERAL vector except when `code_mode == Exclusive` — in exclusive mode the general `vector` may be absent: accept `(Some(text), None)` when `run_general_vector` is false, building `vector: Vec::new()`. Pass `code_mode` into `normalize_queries` as a parameter.)

Code-model validation block, after the general one:

```rust
let mut code_model_id: Option<Uuid> = None;
if run_code_vector {
    let snapshot = state.code_model.read().expect("code_model lock poisoned").clone();
    let Some(km) = snapshot else {
        return error::service_unavailable("server has no resolved code model; check boot logs", rid);
    };
    let Some(client_model) = req.client_code_embedding_model.as_deref() else {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message("client_code_embedding_model is required when code_mode != off")
                .remediation("supply client_code_embedding_model, or set code_mode=off")
                .build(),
            rid,
        );
    };
    if client_model != km.wire {
        return error::into_response(
            CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                .message(format!(
                    "client_code_embedding_model `{client_model}` does not match code model `{}`",
                    km.wire,
                ))
                .remediation("re-embed code queries with the corpus code model")
                .context("code_model", km.wire.clone())
                .context("client_model", client_model.to_owned())
                .build(),
            rid,
        );
    }
    for (i, q) in queries.iter().enumerate() {
        if q.code_vector.len() != km.dim {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message(format!(
                        "queries[{i}].code_vector has {} dimensions; expected {}",
                        q.code_vector.len(), km.dim,
                    ))
                    .remediation("re-embed with the corpus code model")
                    .build(),
                rid,
            );
        }
    }
    code_model_id = Some(km.id);
}
```

In the per-query loop, after the general `vector_hits` block:

```rust
let (code_hits, code_vector_latency_ms): (Vec<(Uuid, f64)>, f64) = if run_code_vector {
    let t = std::time::Instant::now();
    let id = code_model_id.expect("validated above");
    let hits = match code_vector_search(&state.pool, &q.code_vector, &req.filters, id).await {
        Ok(hits) => hits,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, query_index = i, "code vector search failed");
            return error::service_unavailable(format!("code vector search failed for query {i}"), rid);
        }
    };
    (hits, t.elapsed().as_secs_f64() * 1000.0)
} else {
    (Vec::new(), 0.0)
};
```

Feed `code_hits` into `matched` / `best_similarity` exactly like `vector_hits`, push its id list into `ranked_lists` when `run_code_vector`, and extend the `PerQueryRecord` literal with `code_vector_candidates: code_hits_len, code_vector_latency_ms`. Add `code_mode` to the `SearchMetadata` literal.

`code_vector_search` is a clone of `vector_search` with two changes (column + gate):

```rust
/// Run the code-vector half (voyage-code-3 over the partial HNSW). Restricted
/// to chunks whose source_version declares the active code model — opt-out
/// versions and NULL code_embeddings can never appear in this list.
async fn code_vector_search(
    pool: &sqlx::PgPool,
    vector: &[f32],
    filters: &SearchFilters,
    code_model_id: Uuid,
) -> Result<Vec<(Uuid, f64)>, sqlx::Error> {
    let mut qb: QueryBuilder<Postgres> =
        QueryBuilder::new("SELECT chunk.id, 1 - (chunk.code_embedding <=> ");
    qb.push_bind(Vector::from(vector.to_vec()));
    qb.push(") AS similarity FROM chunk JOIN source_version sv ON sv.id = chunk.source_version_id");
    push_filter_joins(&mut qb, filters);
    qb.push(
        " WHERE chunk.code_embedding IS NOT NULL AND chunk.status = 'ready' AND sv.is_active = true",
    );
    qb.push(" AND sv.code_embedding_model_id = ");
    qb.push_bind(code_model_id);
    push_filter_predicates(&mut qb, filters);
    qb.push(" ORDER BY chunk.code_embedding <=> ");
    qb.push_bind(Vector::from(vector.to_vec()));
    qb.push(" LIMIT 100");
    let rows = qb.build().fetch_all(pool).await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let id: Uuid = r.try_get("id").ok()?;
            let sim: f64 = r.try_get("similarity").ok()?;
            Some((id, sim))
        })
        .collect())
}
```

- [ ] **Step 4: Contract tests** — extend the search-route integration tests (find via `grep -rn "v1/search" crates/mn-server/tests`): fts+`code_mode=on` → 400; `code_mode` defaulting (hybrid response metadata reports `"code_mode":"on"`); missing `client_code_embedding_model` with hybrid → 400; wrong code dim → 400. Unit-test `normalize_queries` exclusive-mode acceptance of a vector-less single query.

- [ ] **Step 5: Run** — `cargo test -p mn-server && cargo clippy -p mn-server --all-targets --all-features -- -D warnings` — Expected: PASS.

- [ ] **Step 6: Commit** — `git commit -am "feat(mn-server): code_mode search — third RRF list over the partial code-embedding index"`

---

### Task 14: Admin ingest — dual-embedding uploads (§11.1 server side)

**Files:**
- Modify: `crates/mn-server/src/routes/admin_ingest.rs`

- [ ] **Step 1: Start-run request**

`StartIngestRunRequest` (line ~61) gains:

```rust
    /// Code-embedding model wire id for this run's code vectors. Omit/null ⇔
    /// code embeddings disabled for this version (D9 opt-out).
    #[serde(default)]
    pub code_embedding_model: Option<String>,
```

In the handler (after the existing general-model resolution at ~232-265), resolve it the same way: parse `name@revision`, `embedding_model::get_by_name_revision`, 409 `EmbeddingModelMismatch`-style error when unregistered (mirror the existing unregistered-model error, message naming `code_embedding_model`). Pass the resolved `Option<Uuid>` into the `source_version` insert (parameter added in Task 10).

- [ ] **Step 2: Chunk upload**

Server-side `ChunkUpload` (line ~139) gains:

```rust
    /// Optional voyage-code-3 vector; present only for code-kind chunks of
    /// code-embedding-enabled runs.
    #[serde(default)]
    pub code_embedding: Option<Vec<f32>>,
```

Where chunks are inserted (~line 819 `symbol_path: &chunk_upload.symbol_path`), thread `code_embedding` through to the `NewChunk` (converting via `pgvector::Vector::from` like the general embedding is handled — match the existing embedding conversion). Validation, next to the existing "upload supplies embeddings but no embedding_model" check (~line 704): if any uploaded chunk has `code_embedding.is_some()` but the run's source_version has `code_embedding_model_id == None`, return 400:

```rust
.message("upload supplies code_embedding but the run has no code_embedding_model")
.remediation("pass code_embedding_model on start-run, or drop code_embedding from chunks")
```

Also validate dim: `code_embedding.len() == 1024` mismatch → 400 (mirror however the general embedding dim is validated here; if it isn't, validate against the resolved code model's `dim`).

The carry-forward path (~line 886) copies prior chunks — Task 10 already extended the entity column lists; confirm the prior-row struct in this file (if any) carries `code_embedding` through (grep `prior.` usages around line 886) and add the field where the compiler demands.

- [ ] **Step 3: Tests** — extend this route's existing serde tests (lines ~905-934 pattern): start-run request with and without `code_embedding_model` parses; upload with `code_embedding` on a code-model-less run rejects (integration-gated if the existing rejection tests are).

- [ ] **Step 4: Run** — `cargo test -p mn-server && cargo build --workspace` — Expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat(mn-server): ingest accepts code_embedding_model on start-run + per-chunk code_embedding"`

---

### Task 15: CLI ingest — dual embed + grouping + opt-out (§11.1)

**Files:**
- Modify: `crates/mn-core/src/config.rs` (ModelsConfig)
- Modify: `crates/mn-content/src/manifest/mod.rs` (Manifest)
- Modify: `crates/mn-cli/src/commands/ingest/run.rs`

- [ ] **Step 1: Config defaults**

In `ModelsConfig`: change `embedding` default to `"voyage-context-3"` (update the doc comment: "General corpus embedding model name") and add:

```rust
    /// Code-specialised embedding model name (dual embeddings, D1).
    #[serde(default = "default_code_embedding")]
    pub code_embedding: String,
```

```rust
fn default_code_embedding() -> String {
    "voyage-code-3".to_owned()
}
```

Update `Default for ModelsConfig` accordingly. Fix any config round-trip tests asserting the old default.

- [ ] **Step 2: Manifest opt-out — first manifest-level option**

`Manifest` gains:

```rust
    /// Whether code-kind documents also get voyage-code-3 embeddings (D9).
    /// CLI `--no-code-embeddings` overrides this. Default true.
    #[serde(default = "default_true")]
    pub code_embeddings: bool,
```

with `fn default_true() -> bool { true }` and a parse test:

```rust
#[test]
fn code_embeddings_defaults_true_and_parses_false() {
    let m = Manifest::parse("manifest_version: 1\nroot:\n  name: x\n").unwrap();
    assert!(m.code_embeddings);
    let m = Manifest::parse("manifest_version: 1\ncode_embeddings: false\nroot:\n  name: x\n").unwrap();
    assert!(!m.code_embeddings);
}
```

Check `Manifest` serialization round-trips in `generate.rs` (manifest generation) — the new field serializes; if generation tests golden-match output, either add `skip_serializing_if = "is_true"` (with `fn is_true(b: &bool) -> bool { *b }`) to keep generated manifests clean, or update goldens. Prefer `skip_serializing_if`.

- [ ] **Step 3: CLI flag + wiring in `run.rs`**

```rust
    /// Disable voyage-code-3 code embeddings for this run (overrides the
    /// manifest's `code_embeddings` option). Code files still get general
    /// contextualized embeddings.
    #[arg(long)]
    pub no_code_embeddings: bool,
```

After the manifest loads: `let code_embeddings_enabled = !args.no_code_embeddings && manifest.code_embeddings;`

BYOK embedder construction (~line 518) becomes two embedders:

```rust
let byok = voyage_key.as_deref().map(|key| ByokEmbedders {
    general: ContextualizedVoyageEmbedder::new(
        key, &cfg.models.embedding, cfg.models.voyage_output_dimension,
        &cfg.models.voyage_output_dtype,
    ).with_timeout_secs(voyage_timeout_secs),
    code: VoyageEmbedder::new(
        key, &cfg.models.code_embedding, cfg.models.voyage_output_dimension,
        &cfg.models.voyage_output_dtype,
    ).with_timeout_secs(voyage_timeout_secs),
});
```

with `struct ByokEmbedders { general: ContextualizedVoyageEmbedder, code: VoyageEmbedder }` local to `run.rs`.

Code-model wire resolution, next to the general one (~line 535): from `fetch_active(server_url).await?.code` when present, else fall back to `format!("{}@1", cfg.models.code_embedding)`; only when `code_embeddings_enabled`. Send it on start-run: `code_embedding_model: code_embeddings_enabled.then(|| code_wire.clone())`. Add the field to the CLI-side `StartIngestRunRequest` mirror struct.

`ChunkUpload` gains:

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    code_embedding: Option<Vec<f32>>,
```

(constructed as `code_embedding: None` at the build site; filled by the embed phase).

- [ ] **Step 4: Rewrite `embed_batch` for dual embeddings + context groups**

`DocumentUpload` already carries `kind` — confirm its type is `DocumentKind` (it is; `kind: d.kind` from the plan). Replace `embed_batch` with:

```rust
/// Embed every chunk of `docs` in place: general contextualized vectors for
/// all chunks (per-document context groups, §6), plus flat voyage-code-3
/// vectors for chunks of Code-kind documents when enabled. Returns total
/// Voyage tokens consumed.
async fn embed_batch(
    general: GeneralEmbedSource<'_>,
    code: Option<EmbedSource<'_>>,
    docs: &mut [DocumentUpload],
) -> Result<u64> {
    let mut tokens = 0u64;

    // ── General: per-document context groups, packed into Voyage requests ──
    // Each entry: (texts, token_total) for one context group.
    let mut groups: Vec<(Vec<String>, usize)> = Vec::new();
    for d in docs.iter() {
        let counts: Vec<u32> = d.chunks.iter()
            .map(|c| u32::try_from(c.token_count).unwrap_or(0))
            .collect();
        for r in mn_content::context_group::balanced_groups(
            &counts,
            mn_content::context_group::context_group_limit(),
        ) {
            let texts: Vec<String> = d.chunks[r.clone()].iter().map(|c| c.content.clone()).collect();
            let total: usize = counts[r].iter().map(|&t| t as usize).sum();
            groups.push((texts, total));
        }
    }
    if !groups.is_empty() {
        let plan = plan_group_batches(&groups);
        let mut general_vectors: Vec<Vec<f32>> = Vec::new();
        let mut cursor = groups.into_iter();
        for take in plan {
            let req_groups: Vec<Vec<String>> =
                cursor.by_ref().take(take).map(|(texts, _)| texts).collect();
            let out = mn_embedding::client::embed_general_groups(req_groups, general)
                .await
                .map_err(|e| anyhow!("embed context groups via Voyage: {e}"))?;
            tokens = tokens.saturating_add(out.total_tokens);
            general_vectors.extend(out.groups.into_iter().flatten());
        }
        attach_embeddings(docs, general_vectors)?;
    }

    // ── Code: flat embed for Code-kind documents' chunks ──
    if let Some(code_src) = code {
        let mut code_texts: Vec<(String, usize)> = Vec::new();
        for d in docs.iter() {
            if d.kind == mn_core::types::DocumentKind::Code {
                for c in &d.chunks {
                    code_texts.push((c.content.clone(), usize::try_from(c.token_count).unwrap_or(0)));
                }
            }
        }
        if !code_texts.is_empty() {
            let counts: Vec<usize> = code_texts.iter().map(|(_, t)| *t).collect();
            let plan = plan_subbatches(&counts, VOYAGE_MAX_TEXTS_PER_REQUEST, VOYAGE_MAX_TOKENS_PER_REQUEST);
            let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(code_texts.len());
            let mut chunks = code_texts.into_iter().map(|(s, _)| s);
            for size in plan {
                let sub: Vec<String> = chunks.by_ref().take(size).collect();
                let out = mn_embedding::client::embed_code(sub, InputType::Document, code_src)
                    .await
                    .map_err(|e| anyhow!("embed code chunks via Voyage: {e}"))?;
                tokens = tokens.saturating_add(out.total_tokens);
                vectors.extend(out.vectors);
            }
            attach_code_embeddings(docs, vectors)?;
        }
    }
    Ok(tokens)
}

/// Pack context groups into Voyage requests bounded by ≤1 000 inputs,
/// ≤120 K summed tokens, and ≤16 K total chunks per request. Returns
/// group-counts per request, summing to `groups.len()`.
fn plan_group_batches(groups: &[(Vec<String>, usize)]) -> Vec<usize> {
    const MAX_INPUTS: usize = 1_000;
    const MAX_TOKENS: usize = 100_000; // headroom under Voyage's 120K
    const MAX_CHUNKS: usize = 16_000;
    let mut sizes = Vec::new();
    let (mut n, mut toks, mut chunks) = (0usize, 0usize, 0usize);
    for (texts, total) in groups {
        let over = n > 0
            && (n >= MAX_INPUTS
                || toks.saturating_add(*total) > MAX_TOKENS
                || chunks.saturating_add(texts.len()) > MAX_CHUNKS);
        if over {
            sizes.push(n);
            n = 0; toks = 0; chunks = 0;
        }
        n += 1;
        toks = toks.saturating_add(*total);
        chunks = chunks.saturating_add(texts.len());
    }
    if n > 0 { sizes.push(n); }
    sizes
}

/// Distribute one code vector per Code-kind chunk, in document-then-chunk order.
fn attach_code_embeddings(docs: &mut [DocumentUpload], vectors: Vec<Vec<f32>>) -> Result<()> {
    let total: usize = docs.iter()
        .filter(|d| d.kind == mn_core::types::DocumentKind::Code)
        .map(|d| d.chunks.len())
        .sum();
    if vectors.len() != total {
        return Err(anyhow!("code embedder returned {} vectors for {total} chunks", vectors.len()));
    }
    let mut it = vectors.into_iter();
    for d in docs.iter_mut() {
        if d.kind != mn_core::types::DocumentKind::Code {
            continue;
        }
        for c in &mut d.chunks {
            c.code_embedding = it.next();
        }
    }
    Ok(())
}
```

Update the call site: build `general` from `byok.as_ref().map(...)` → `GeneralEmbedSource::Byok(&b.general)` else `GeneralEmbedSource::Server {...}`; `code = code_embeddings_enabled.then(|| byok.as_ref().map_or(EmbedSource::Server {...}, |b| EmbedSource::Byok(&b.code)))`. Delete the old `EmbedCtx` enum if nothing else uses it. Add unit tests for `plan_group_batches` (mirror the `plan_subbatches` test style: item cap, token cap, chunk cap, empty) and `attach_code_embeddings` (skips non-code docs; count mismatch errors).

- [ ] **Step 5: Run** — `VOYAGE_API_KEY= cargo test -p mn-cli && cargo clippy -p mn-cli --all-targets -- -D warnings` (expect the 2 known loopback auth failures locally) — Expected: PASS otherwise.

- [ ] **Step 6: Commit** — `git commit -am "feat(mn-cli): dual-embedding ingest — context groups, code vectors, manifest/flag opt-out"`

---

### Task 16: CLI search — `--code-mode` (§11.2)

**Files:**
- Modify: `crates/mn-cli/src/commands/search.rs`

- [ ] **Step 1: Flag**

```rust
    /// Code-vector fusion mode: on (default for hybrid/vector), off, or
    /// exclusive (code vectors replace the general vector list). Incompatible
    /// with --mode fts.
    #[arg(long = "code-mode", value_parser = ["on", "off", "exclusive"])]
    pub code_mode: Option<String>,
```

- [ ] **Step 2: Embed + request wiring**

Compute the effective need client-side (mirror server defaults; never sniff queries):

```rust
let embed_code_query = args.mode != "fts"
    && args.code_mode.as_deref() != Some("off");
let embed_general_query = args.mode != "fts"
    && args.code_mode.as_deref() != Some("exclusive");
```

Replace the single embed call: general queries via `mn_embedding::client::embed_general` with `GeneralEmbedSource::Byok(&ContextualizedVoyageEmbedder::new(key, &cfg.models.embedding, ...))` or `::Server{...}` (only when `embed_general_query`); code queries via `embed_code` with the flat `VoyageEmbedder::new(key, &cfg.models.code_embedding, ...)` or server `type=code` (only when `embed_code_query`). Build `QueryPair { text, vector, code_vector }` with empty vecs for the halves not embedded. Resolve `client_code_embedding_model` from `fetch_active(...).code` (fall back `format!("{}@1", cfg.models.code_embedding)`); send it + `code_mode: args.code_mode` on the request body (add both fields to the CLI's request mirror struct; serialize `code_mode` as the raw string, `skip_serializing_if = "Option::is_none"`).

If the CLI emits a search telemetry event (grep `telemetry` usage in this file), add `code_mode` to its payload the same way `mode` is recorded.

- [ ] **Step 3: Tests** — this command's unit tests cover request-building helpers; add one asserting the body includes `"code_mode":"exclusive"` and an empty general vector is permitted in exclusive mode, and one asserting fts sends neither vector nor code fields.

- [ ] **Step 4: Run** — `VOYAGE_API_KEY= cargo test -p mn-cli && cargo clippy -p mn-cli --all-targets -- -D warnings` — Expected: PASS. Then delete `mn_embedding::client::embed`'s remaining general-typed usages if any linger (`grep -rn "client::embed(" crates/`) — once zero remain, delete the legacy `embed`/`embed_once` pair and their imports; if mn-mcp still uses them, defer deletion to Task 17.

- [ ] **Step 5: Commit** — `git commit -am "feat(mn-cli): mnm search --code-mode with dual query embedding"`

---

### Task 17: MCP — `code_mode` on the search tool (§11.2)

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`

- [ ] **Step 1: Schema** — in `search_input_schema()` add after `"mode"`:

```rust
"code_mode": { "type": "string", "enum": ["on", "off", "exclusive"],
    "description": "Code-vector fusion (dual embeddings): on (default for hybrid/vector) fuses a voyage-code-3 ranked list into RRF alongside the general results; off = general retrieval only; exclusive = code vectors replace the general vector list (use for API-shaped/code-identifier queries). Incompatible with mode=fts." },
```

- [ ] **Step 2: Parse + flow** — the parsed-params struct for search gains `code_mode: Option<String>` (validated against the enum the same way `mode` is). `run_search` embeds the code query via `embed_code` (BYOK flat `VoyageEmbedder::new(key, &models.code_embedding, ...)` or server `type=code`) when `mode != "fts" && code_mode != Some("off")`, embeds general via `embed_general` when not exclusive, sends `code_mode` + `client_code_embedding_model` + per-pair `code_vector` on the cloud request (extend the MCP request mirror structs), and reports `corpus_code_embedding_model` alongside the existing `corpus_embedding_model` in the result metadata when code search ran. Resolve the code wire from `GET /v1/models/active`'s `code` field (the MCP already fetches the active model — extend that struct like the CLI's).

- [ ] **Step 3: Tool description** — extend the `search` tool description (line ~51) with one sentence: `"Code-heavy queries (function names, API signatures, error strings from code) benefit from code_mode=exclusive; conceptual queries should keep the default."`

- [ ] **Step 4: Skill docs** — `grep -rln "advanced" crates/mn-mcp docs | grep -iv target` to find the advanced-search skill markdown served by `install_search_skill` (and the cookbook `docs/cookbook/query-enhancement.md`). Add a short `## code_mode` section documenting the three values and the defaults table from spec §10.2 (copy the table verbatim from the spec).

- [ ] **Step 5: Run** — `VOYAGE_API_KEY= cargo test -p mn-mcp && cargo clippy -p mn-mcp --all-targets -- -D warnings` — MCP tool-schema golden tests will need the new field added. Expected: PASS. If Task 16 deferred deleting legacy `client::embed`, do it now and re-run workspace clippy.

- [ ] **Step 6: Commit** — `git commit -am "feat(mn-mcp): code_mode on the search tool + skill docs"`

---

### Task 18: Integration tests + final verification

**Files:**
- Modify: mn-server integration tests (wherever `--features integration` search/ingest tests live — `grep -rn "features.*integration\|testcontainers" crates/mn-server/tests crates/mn-server/Cargo.toml`)

- [ ] **Step 1: Add the end-to-end integration test** (CI-only; follows the existing testcontainers harness patterns in that directory)

Scenario, one test fn per assertion group:
1. Run migrations (0001–0011) against an ephemeral Postgres+pgvector; start-run with `embedding_model: "voyage-context-3@1"` and `code_embedding_model: "voyage-code-3@1"`; upload one markdown document (embedding only) and one code document (embedding + code_embedding, synthetic 1024-dim vectors); finalize.
2. `POST /v1/search` hybrid with synthetic vectors: `code_mode` defaulted → response metadata `code_mode == "on"`, code chunk reachable via `code_vector`; `code_mode=off` → identical to pre-cutover behavior (no code list; `code_vector_candidates == 0`); `code_mode=exclusive` → markdown chunk absent from vector-derived results.
3. Opt-out: a second source ingested WITHOUT `code_embedding_model`; uploading a chunk with `code_embedding` → 400; its chunks never appear in code-vector candidates.
4. fts + `code_mode=on` → 400.
5. The DB trigger: inserting a chunk row with `code_embedding` under a code-model-less source_version errors (assert the raised exception).

- [ ] **Step 2: Full local gate**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
VOYAGE_API_KEY= cargo test --workspace
```

Expected: PASS (modulo the 2 known sandbox auth-loopback failures). Integration tests compile (`cargo test --workspace --features integration --no-run`) even though they can't run locally.

- [ ] **Step 3: Docs touch-ups** — `docs/cookbook/query-enhancement.md` gains a `code_mode` paragraph (done in Task 17 if the grep found it there; otherwise here). README "Telemetry & Privacy" unaffected. CLAUDE.md "Recent Changes": add a line for this feature.

- [ ] **Step 4: Commit** — `git commit -am "test: dual-embedding ingest + code_mode search integration coverage"`

- [ ] **Step 5: Open the PR** — after CI is green (integration tests run per-PR in CI), note in the PR description: full corpus re-ingest required after deploy; migration 0011 deactivates all source_versions (search returns nothing until re-ingest); `tests/recall/` harness is a follow-up plan.

---

## Self-Review Notes (already applied)

- **Spec coverage:** §5.1→Tasks 3/5/6, §5.2→Tasks 4/5, §5.3→Task 2/3, §5.4→Task 1, §6→Task 7, §7→Tasks 8/9/11, §8→Task 10, §9→Task 12, §10→Task 13, §11.1→Tasks 14/15, §11.2→Tasks 16/17, §13→distributed + Task 18 (recall harness explicitly deferred to a follow-up plan).
- **Known intentional deviations:** (1) recall harness deferred; (2) code model resolution is config-pinned (`MIDNIGHT_MANUAL_CODE_MODEL`) rather than activity-derived — deterministic and simpler than extending `get_active()`'s heuristic; (3) `mn-server` inlines the 28,800 constant rather than depending on mn-content.
- **Type consistency check:** `coalesce_target` (Tasks 3-7), `GeneralEmbedSource`/`embed_code`/`embed_general_groups` (Tasks 9/12/15/16/17), `CodeModel`/`code_model` (Tasks 11/13/14), `code_embedding_model` wire field name (Tasks 14/15), `ChunkUpload.code_embedding` (Tasks 14/15) — names match across tasks.

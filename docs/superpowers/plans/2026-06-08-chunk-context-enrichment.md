# Chunk Context Enrichment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give chunks more usable context — a rolling sentence window around markdown chunks, an enclosing-symbol breadcrumb on split code chunks, light code coalescing — and dedup the overlap that the window introduces at search time.

**Architecture:** Three independent slices. (A) `mn-content` markdown chunker gains a sentence-window pass that expands each in-budget chunk's `content`. (B) `mn-retrieval` gains a pure `dedup` pass that `mn-server`'s search route calls after scoring, before truncation. (C) `mn-content` code driver (`run_tree_sitter`) gains a coalescing pass and a wrapper-breadcrumb pass. All thresholds live in `ChunkerConfig`.

**Tech Stack:** Rust (workspace, MSRV 1.91). `pulldown-cmark` (markdown), `tree-sitter` + `text-splitter` (code), `crate::tokens::count` (BPE budgeting), `sqlx`/`axum` (server), `proptest` (property tests). Default cargo features: `core-grammars` + `compact`.

**Source design:** `docs/superpowers/specs/2026-06-08-chunk-context-design.md`.

**Planning refinement vs the design doc:** the design said "replace `min_tokens` with `window_core_pct`." During planning we found `min_tokens` is wired to the `--md-min-tokens` CLI flag and several integration tests. To follow the established absolute-token convention and avoid CLI churn, we **keep `min_tokens` as the absolute core floor** (default `128` → `280`, i.e. 70% of `max_tokens`) and add the *upper* window markers as percentages. Behaviour matches the design; only the field name differs.

---

## Part A — Markdown rolling window (`mn-content`)

### Task 1: ChunkerConfig — raise the floor, add window + code knobs

**Files:**
- Modify: `crates/mn-content/src/chunk.rs:29-53` (struct + `Default`)
- Modify: `crates/mn-content/src/chunk.rs:81-90` (default-config test)
- Modify: `crates/mn-cli/src/commands/ingest/run.rs:144-145` (clap default) and `:387-392` (literal construction)
- Modify: `crates/mn-cli/src/commands/models.rs:493`

- [ ] **Step 1: Update the failing test first**

In `crates/mn-content/src/chunk.rs`, replace the body of `default_config_is_token_budgeted` (around line 81) with:

```rust
    #[test]
    fn default_config_is_token_budgeted() {
        let c = ChunkerConfig::default();
        assert_eq!(c.max_tokens, 400);
        assert_eq!(c.min_tokens, 280);
        assert_eq!(c.window_switch_pct, 0.80);
        assert_eq!(c.window_target_pct, 0.90);
        assert_eq!(c.window_cap_pct, 1.00);
        assert_eq!(c.code_min_tokens, 64);
        assert_eq!(c.fallback_lines, 60);
        assert_eq!(c.fallback_overlap_lines, 20);
        assert_eq!(c.max_file_bytes, 10 * 1024 * 1024);
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p mn-content chunk::tests::default_config_is_token_budgeted`
Expected: FAIL — `no field window_switch_pct` / `min_tokens` mismatch.

- [ ] **Step 3: Add the fields and update `Default`**

In `crates/mn-content/src/chunk.rs`, extend the struct (after the `min_tokens` field, line 34) and the `Default` impl:

```rust
    /// Soft minimum chunk size in BPE tokens — the markdown coalescing target
    /// AND the core-body floor (~70% of `max_tokens`) the rolling window pads
    /// out from. Adjacent small markdown sections merge up to this. Markdown-only.
    pub min_tokens: u32,
    /// Rolling-window: fill the smaller side up to this fraction of `max_tokens`
    /// before switching sides. Markdown-only.
    pub window_switch_pct: f32,
    /// Rolling-window: final fill target, as a fraction of `max_tokens`.
    pub window_target_pct: f32,
    /// Rolling-window: hard cap, as a fraction of `max_tokens` (never exceeded).
    pub window_cap_pct: f32,
    /// Code coalescing floor in BPE tokens: adjacent same-scope code chunks merge
    /// up to this. Code-only.
    pub code_min_tokens: u32,
```

```rust
impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            max_tokens: 400,
            min_tokens: 280,
            window_switch_pct: 0.80,
            window_target_pct: 0.90,
            window_cap_pct: 1.00,
            code_min_tokens: 64,
            fallback_lines: 60,
            fallback_overlap_lines: 20,
            max_file_bytes: 10 * 1024 * 1024,
        }
    }
}
```

- [ ] **Step 4: Fix the non-spread CLI literal**

In `crates/mn-cli/src/commands/ingest/run.rs`, the construction at line 387 is a full literal. Add a spread so the new fields are picked up from defaults:

```rust
    let chunker_config = mn_content::chunk::ChunkerConfig {
        max_tokens: args.code_chunk_tokens,
        min_tokens: args.md_min_tokens,
        fallback_lines: args.code_chunk_lines,
        fallback_overlap_lines: args.code_chunk_overlap,
        max_file_bytes: args.max_file_size,
        ..mn_content::chunk::ChunkerConfig::default()
    };
```

Update the clap default at line 144 (`#[arg(long, default_value_t = 128)]` above `pub md_min_tokens: u32,`) to `default_value_t = 280`, and its doc comment to "(~70% of the code-chunk budget)".

Update `crates/mn-cli/src/commands/models.rs:493` from `md_min_tokens: 128,` to `md_min_tokens: 280,`.

- [ ] **Step 5: Run the affected suites**

Run: `cargo test -p mn-content && cargo build -p mn-cli`
Expected: PASS / builds. The markdown coalescing tests use `.contains()` / "merges" assertions and a higher floor only merges *more*, so they stay green.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-content/src/chunk.rs crates/mn-cli/src/commands/ingest/run.rs crates/mn-cli/src/commands/models.rs
git commit -m "feat(mn-content): raise core floor to 280 + add window/code chunker knobs"
```

---

### Task 2: Sentence segmentation helper

**Files:**
- Modify: `crates/mn-content/src/markdown.rs` (add private fns + tests)

- [ ] **Step 1: Write failing tests**

Add to the `tests` module in `crates/mn-content/src/markdown.rs`:

```rust
    #[test]
    fn segments_prose_into_sentences() {
        let r = "First one. Second two! Third three?";
        let units: Vec<&str> = segment_sentences(r).iter().map(|x| &r[x.clone()]).collect();
        assert_eq!(units, vec!["First one.", "Second two!", "Third three?"]);
    }

    #[test]
    fn fenced_code_block_is_one_atomic_unit() {
        let r = "Intro line.\n\n```rust\nfn a() {}\nfn b() {}\n```\n\nOutro line.";
        let units: Vec<&str> = segment_sentences(r).iter().map(|x| &r[x.clone()]).collect();
        assert!(units.iter().any(|u| u.starts_with("```rust") && u.contains("fn b() {}")));
        assert!(units.iter().any(|u| *u == "Intro line."));
        assert!(units.iter().any(|u| *u == "Outro line."));
    }

    #[test]
    fn table_rows_group_into_one_unit() {
        let r = "Before.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nAfter.";
        let units: Vec<&str> = segment_sentences(r).iter().map(|x| &r[x.clone()]).collect();
        assert!(units.iter().any(|u| u.starts_with("| a | b |") && u.contains("| 1 | 2 |")));
    }

    #[test]
    fn empty_region_yields_no_units() {
        assert!(segment_sentences("   \n\n  ").is_empty());
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p mn-content markdown::tests::segments_prose_into_sentences`
Expected: FAIL — `cannot find function segment_sentences`.

- [ ] **Step 3: Implement the segmenter**

Add to `crates/mn-content/src/markdown.rs` (top-level, after the imports add `use std::ops::Range;`):

```rust
/// Split `region` into sentence-granular byte ranges (absolute within `region`).
///
/// Prose splits on `.`/`!`/`?` followed by whitespace or end-of-input, and at
/// blank-line (paragraph) breaks. Fenced code blocks (```` ``` ````/`~~~`) and
/// runs of table lines (`|…`) are emitted whole — never split. Whitespace-only
/// spans are dropped; returned ranges are in document order.
fn segment_sentences(region: &str) -> Vec<Range<usize>> {
    let bytes = region.as_bytes();
    let n = region.len();
    let mut units: Vec<Range<usize>> = Vec::new();
    let mut unit_start = 0usize;
    let mut i = 0usize;

    while i < n {
        let line_start = i;
        let line_end = region[i..].find('\n').map_or(n, |o| i + o);
        let next_line = if line_end < n { line_end + 1 } else { n };
        let trimmed = region[line_start..line_end].trim_start();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            push_trimmed(region, &mut units, unit_start, line_start);
            let fence = if trimmed.starts_with("```") { "```" } else { "~~~" };
            let mut j = next_line;
            let block_end = loop {
                if j >= n {
                    break n;
                }
                let jl_end = region[j..].find('\n').map_or(n, |o| j + o);
                if region[j..jl_end].trim_start().starts_with(fence) {
                    break jl_end;
                }
                j = if jl_end < n { jl_end + 1 } else { n };
            };
            units.push(line_start..block_end);
            i = if block_end < n { block_end + 1 } else { n };
            unit_start = i;
            continue;
        }

        if trimmed.starts_with('|') {
            push_trimmed(region, &mut units, unit_start, line_start);
            let mut end = line_end;
            let mut k = next_line;
            while k < n {
                let kl_end = region[k..].find('\n').map_or(n, |o| k + o);
                if region[k..kl_end].trim_start().starts_with('|') {
                    end = kl_end;
                    k = if kl_end < n { kl_end + 1 } else { n };
                } else {
                    break;
                }
            }
            units.push(line_start..end);
            i = if end < n { end + 1 } else { n };
            unit_start = i;
            continue;
        }

        if trimmed.is_empty() {
            push_trimmed(region, &mut units, unit_start, line_start);
            i = next_line;
            unit_start = i;
            continue;
        }

        // Prose line: split on sentence terminators (ASCII bytes, so multibyte
        // UTF-8 is never matched mid-codepoint).
        let mut p = line_start;
        while p < line_end {
            let b = bytes[p];
            if (b == b'.' || b == b'!' || b == b'?')
                && (p + 1 >= line_end || bytes[p + 1].is_ascii_whitespace())
            {
                push_trimmed(region, &mut units, unit_start, p + 1);
                unit_start = p + 1;
            }
            p += 1;
        }
        i = next_line;
    }
    push_trimmed(region, &mut units, unit_start, n);
    units
}

/// Push `[start, end)` with surrounding whitespace trimmed off; skip if empty.
fn push_trimmed(region: &str, units: &mut Vec<Range<usize>>, start: usize, end: usize) {
    if start >= end || end > region.len() {
        return;
    }
    let slice = &region[start..end];
    let s = start + (slice.len() - slice.trim_start().len());
    let e = end - (slice.len() - slice.trim_end().len());
    if s < e {
        units.push(s..e);
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p mn-content markdown::tests::`
Expected: PASS (the four new tests plus the existing markdown tests).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/markdown.rs
git commit -m "feat(mn-content): sentence segmenter with atomic code/table units"
```

---

### Task 3: Window-expand function

**Files:**
- Modify: `crates/mn-content/src/markdown.rs` (add `expand_window` + `pct_tokens` + `grow_side` + tests)

- [ ] **Step 1: Write failing tests**

Add to the `tests` module. These use a small budget so token math is easy to reason about (`max=60` ⇒ switch≈48, target≈54, cap=60):

```rust
    fn window_cfg() -> ChunkerConfig {
        ChunkerConfig { max_tokens: 60, min_tokens: 1, ..ChunkerConfig::default() }
    }

    #[test]
    fn expand_grows_both_sides_when_centered() {
        // Many short sentences; a small core in the middle should pull context
        // from BOTH sides and end under the cap.
        let s = "alpha beta gamma delta. ".repeat(40); // ~plenty on both sides
        let body = s.as_str();
        let core_start = body.len() / 2;
        let core_end = (core_start + 20).min(body.len());
        let (ws, we) = expand_window(body, core_start, core_end, &window_cfg());
        assert!(ws < core_start, "should pull `before` context");
        assert!(we > core_end, "should pull `after` context");
        assert!(crate::tokens::count(&body[ws..we]) <= 60, "must not exceed the cap");
    }

    #[test]
    fn expand_is_noop_when_core_already_at_target() {
        let body = "one two three four five. ".repeat(40);
        // A core that is already ≥ target leaves the range unchanged.
        let big_end = body.len();
        let (ws, we) = expand_window(&body, 0, big_end, &window_cfg());
        assert_eq!((ws, we), (0, big_end));
    }

    #[test]
    fn expand_at_document_start_only_grows_forward() {
        let body = "aa bb cc dd ee ff. ".repeat(40);
        let core_end = 18.min(body.len());
        let (ws, we) = expand_window(&body, 0, core_end, &window_cfg());
        assert_eq!(ws, 0, "no `before` text available at doc start");
        assert!(we > core_end, "must still grow forward");
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p mn-content markdown::tests::expand_grows_both_sides_when_centered`
Expected: FAIL — `cannot find function expand_window`.

- [ ] **Step 3: Implement**

Add to `crates/mn-content/src/markdown.rs` (top-level):

```rust
/// `round(pct * max)` as a token count.
fn pct_tokens(pct: f32, max: u32) -> u32 {
    (pct * max as f32).round().max(0.0) as u32
}

/// Grow one side of the window by whole sentence units (ordered nearest-core
/// first) until `limit` tokens or the side is exhausted, never letting the
/// window exceed `cap`. Updates `ws`/`we`/`idx` in place.
fn grow_side(
    body: &str,
    units: &[Range<usize>],
    idx: &mut usize,
    before: bool,
    ws: &mut usize,
    we: &mut usize,
    limit: u32,
    cap: u32,
) {
    loop {
        if crate::tokens::count(&body[*ws..*we]) >= limit {
            break;
        }
        let Some(r) = units.get(*idx) else { break };
        let (nws, nwe) = if before { (r.start, *we) } else { (*ws, r.end) };
        if crate::tokens::count(&body[nws..nwe]) > cap {
            break; // atomic unit would breach the cap → stop this side
        }
        *ws = nws;
        *we = nwe;
        *idx += 1;
    }
}

/// Expand the core byte range `[core_start, core_end)` with surrounding sentence
/// context per the rolling-window policy. Returns the (possibly unchanged)
/// expanded byte range. Smaller side first to the switch point, then the larger
/// side to the target; budgets in BPE tokens.
fn expand_window(
    body: &str,
    core_start: usize,
    core_end: usize,
    cfg: &ChunkerConfig,
) -> (usize, usize) {
    let max = cfg.max_tokens;
    let switch = pct_tokens(cfg.window_switch_pct, max);
    let target = pct_tokens(cfg.window_target_pct, max);
    let cap = pct_tokens(cfg.window_cap_pct, max);

    if crate::tokens::count(&body[core_start..core_end]) >= target {
        return (core_start, core_end);
    }

    // `before` nearest-core first = reverse document order; `after` already is.
    let before: Vec<Range<usize>> = segment_sentences(&body[..core_start])
        .into_iter()
        .rev()
        .collect();
    let after: Vec<Range<usize>> = segment_sentences(&body[core_end..])
        .into_iter()
        .map(|r| (core_end + r.start)..(core_end + r.end))
        .collect();

    let before_avail = crate::tokens::count(&body[..core_start]);
    let after_avail = crate::tokens::count(&body[core_end..]);
    let smaller_is_before = before_avail <= after_avail;

    let mut ws = core_start;
    let mut we = core_end;
    let mut bi = 0usize;
    let mut ai = 0usize;

    let core_tokens = crate::tokens::count(&body[core_start..core_end]);
    // Phase 1: smaller side to the switch point (skipped when core ≥ switch).
    if core_tokens < switch {
        if smaller_is_before {
            grow_side(body, &before, &mut bi, true, &mut ws, &mut we, switch, cap);
        } else {
            grow_side(body, &after, &mut ai, false, &mut ws, &mut we, switch, cap);
        }
    }
    // Phase 2: the larger side to the target.
    if smaller_is_before {
        grow_side(body, &after, &mut ai, false, &mut ws, &mut we, target, cap);
    } else {
        grow_side(body, &before, &mut bi, true, &mut ws, &mut we, target, cap);
    }

    (ws, we)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p mn-content markdown::tests::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/markdown.rs
git commit -m "feat(mn-content): rolling-window expansion (smaller-side-first, sentence units)"
```

---

### Task 4: Wire the window into `MarkdownChunker`

**Files:**
- Modify: `crates/mn-content/src/markdown.rs:91-101` (in-budget branch)

- [ ] **Step 1: Write a failing integration test**

Add to the `tests` module:

```rust
    #[test]
    fn in_budget_chunk_gains_surrounding_context() {
        // Three real paragraphs under one heading; with min_tokens=1 each
        // paragraph is its own core, and the middle chunk must pull text from
        // its neighbours into `content`.
        let md = "# Doc\n\n\
            The first paragraph talks about apples and how they grow on trees in orchards.\n\n\
            The second paragraph is the focus and mentions zebras crossing the savannah plains.\n\n\
            The third paragraph closes with notes about boats sailing across calm harbours.\n";
        let cfg = ChunkerConfig { max_tokens: 80, min_tokens: 1, ..ChunkerConfig::default() };
        let chunks = MarkdownChunker.chunk(md, &cfg).unwrap();
        let middle = chunks
            .iter()
            .find(|c| c.content.contains("zebras"))
            .expect("a chunk anchored on the second paragraph");
        // It pulled in at least one neighbouring paragraph's distinctive word.
        assert!(
            middle.content.contains("apples") || middle.content.contains("boats"),
            "middle chunk should carry neighbouring context: {:?}",
            middle.content
        );
        // Never exceeds the cap (= max_tokens at the default cap pct).
        for c in &chunks {
            assert!(c.token_count <= cfg.max_tokens, "chunk over cap: {}", c.token_count);
        }
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p mn-content markdown::tests::in_budget_chunk_gains_surrounding_context`
Expected: FAIL — the middle chunk currently contains only its own paragraph.

- [ ] **Step 3: Apply the window in the in-budget branch**

In `crates/mn-content/src/markdown.rs`, replace the in-budget arm (currently lines ~91-101, the `if crate::tokens::count(text) <= cfg.max_tokens { ... }` block) with:

```rust
            if crate::tokens::count(text) <= cfg.max_tokens {
                // Expand the core section with a surrounding sentence window.
                let (ws, we) = expand_window(body, seg.start, seg.end, cfg);
                let content = &body[ws..we];
                chunks.push(Chunk {
                    content: content.to_owned(),
                    heading_path: seg.heading_path.clone(),
                    symbol_path: Vec::new(),
                    start_byte: ws,
                    end_byte: we,
                    token_count: crate::tokens::count(content),
                    chunk_index: 0, // filled in below
                    fallback_used: false,
                });
            } else {
```

Leave the `else` (oversize `token_window_split`) arm unchanged — oversize sections get no window.

- [ ] **Step 4: Run the full markdown suite**

Run: `cargo test -p mn-content`
Expected: PASS. Existing tests assert via `.contains()` / structural invariants and tolerate the larger content; `coalesced_chunks_have_valid_offsets_and_indices` still holds (`end_byte` is clamped to the document).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/markdown.rs
git commit -m "feat(mn-content): embed rolling context window in markdown chunks"
```

---

## Part B — Result-set overlap dedup (`mn-retrieval` + `mn-server`)

### Task 5: Pure dedup module in `mn-retrieval`

**Files:**
- Create: `crates/mn-retrieval/src/dedup.rs`
- Modify: `crates/mn-retrieval/src/lib.rs:14-16` (export `dedup`)

- [ ] **Step 1: Write the module with failing tests**

Create `crates/mn-retrieval/src/dedup.rs`:

```rust
//! Result-set overlap dedup.
//!
//! When several retrieved chunks come from the same document their stored
//! `content` can overlap (the markdown chunker embeds a rolling context window,
//! so neighbouring chunks share text). This pass walks results in rank order and
//! removes already-shown text: fully-covered chunks are dropped, partially-
//! covered chunks have the duplicated span trimmed out (`…` marks the cut).
//!
//! Pure and storage-agnostic: callers implement [`OverlapItem`]. Only
//! *byte-aligned* content (where `content.len() == end - start`, i.e. verbatim
//! `body[start..end]`) is trimmed; content carrying a synthetic prefix (e.g. a
//! code breadcrumb) is left intact but still contributes its byte span.

use std::collections::HashMap;
use std::hash::Hash;

/// Marker inserted between non-contiguous kept spans of a trimmed item.
const ELISION: &str = "\n…\n";

/// A retrieval result that can be overlap-deduplicated.
pub trait OverlapItem {
    /// Document grouping key. Only items sharing a key are compared.
    type Key: Eq + Hash;

    /// The document key for this item.
    fn document_key(&self) -> Self::Key;
    /// Byte range `[start, end)` of this item within its document.
    fn byte_range(&self) -> (usize, usize);
    /// The item's current text content.
    fn content(&self) -> &str;
    /// Replace the item's content (used when trimming overlap).
    fn set_content(&mut self, content: String);
}

/// Outcome counts from a dedup pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DedupStats {
    /// Items dropped because every byte was already shown by a higher-ranked
    /// same-document item.
    pub dropped: usize,
    /// Items whose content was trimmed (some, not all, bytes already shown).
    pub trimmed: usize,
}

/// Trim overlapping text from same-document items, processing in the given order
/// (callers pass results already sorted best-rank-first). Returns the surviving
/// items in their original order, plus [`DedupStats`]. Cross-document items never
/// interact; ordering is preserved.
#[must_use]
pub fn trim_overlaps<T: OverlapItem>(items: Vec<T>) -> (Vec<T>, DedupStats) {
    let mut covered: HashMap<T::Key, Vec<(usize, usize)>> = HashMap::new();
    let mut out = Vec::with_capacity(items.len());
    let mut stats = DedupStats::default();

    for mut item in items {
        let key = item.document_key();
        let (start, end) = item.byte_range();
        let intervals = covered.entry(key).or_default();

        let gaps = subtract(start, end, intervals);
        insert(intervals, start, end);

        if gaps.is_empty() {
            stats.dropped += 1;
            continue;
        }

        let aligned = item.content().len() == end.saturating_sub(start);
        let fully_uncovered = gaps.len() == 1 && gaps[0] == (start, end);
        if aligned && !fully_uncovered {
            let content = item.content();
            let mut kept = String::new();
            for (i, &(gs, ge)) in gaps.iter().enumerate() {
                if i > 0 {
                    kept.push_str(ELISION);
                }
                if let Some(slice) = content.get(gs - start..ge - start) {
                    kept.push_str(slice);
                }
            }
            item.set_content(kept);
            stats.trimmed += 1;
        }

        out.push(item);
    }

    (out, stats)
}

/// Uncovered sub-ranges of `[start, end)` given `covered` (need not be sorted).
fn subtract(start: usize, end: usize, covered: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if start >= end {
        return Vec::new();
    }
    let mut overlaps: Vec<(usize, usize)> = covered
        .iter()
        .copied()
        .filter(|&(s, e)| e > start && s < end)
        .collect();
    overlaps.sort_unstable();

    let mut gaps = Vec::new();
    let mut cursor = start;
    for (s, e) in overlaps {
        if s > cursor {
            gaps.push((cursor, s.min(end)));
        }
        cursor = cursor.max(e);
        if cursor >= end {
            break;
        }
    }
    if cursor < end {
        gaps.push((cursor, end));
    }
    gaps
}

/// Insert `[start, end)`, keeping `intervals` merged and sorted.
fn insert(intervals: &mut Vec<(usize, usize)>, start: usize, end: usize) {
    if start >= end {
        return;
    }
    intervals.push((start, end));
    intervals.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(intervals.len());
    for &(s, e) in intervals.iter() {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    *intervals = merged;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct R {
        doc: u32,
        s: usize,
        e: usize,
        c: String,
    }
    impl OverlapItem for R {
        type Key = u32;
        fn document_key(&self) -> u32 {
            self.doc
        }
        fn byte_range(&self) -> (usize, usize) {
            (self.s, self.e)
        }
        fn content(&self) -> &str {
            &self.c
        }
        fn set_content(&mut self, c: String) {
            self.c = c;
        }
    }
    fn r(doc: u32, s: usize, e: usize, c: &str) -> R {
        R { doc, s, e, c: c.into() }
    }

    #[test]
    fn trims_trailing_overlap_of_lower_ranked_neighbour() {
        let items = vec![r(1, 0, 10, "0123456789"), r(1, 5, 15, "56789ABCDE")];
        let (out, stats) = trim_overlaps(items);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].c, "0123456789");
        assert_eq!(out[1].c, "ABCDE");
        assert_eq!(stats, DedupStats { dropped: 0, trimmed: 1 });
    }

    #[test]
    fn drops_fully_covered_item() {
        let items = vec![r(1, 0, 10, "0123456789"), r(1, 2, 8, "234567")];
        let (out, stats) = trim_overlaps(items);
        assert_eq!(out.len(), 1);
        assert_eq!(stats.dropped, 1);
    }

    #[test]
    fn cross_document_never_interacts() {
        let items = vec![r(1, 0, 10, "0123456789"), r(2, 0, 10, "abcdefghij")];
        let (out, stats) = trim_overlaps(items);
        assert_eq!(out.len(), 2);
        assert_eq!(stats, DedupStats::default());
    }

    #[test]
    fn non_byte_aligned_content_is_left_intact() {
        // content longer than the byte span (a breadcrumb-augmented chunk):
        // overlap is recorded but content is not re-sliced.
        let items = vec![
            r(1, 0, 10, "0123456789"),
            r(1, 5, 15, "// crumb\n56789ABCDE"),
        ];
        let (out, stats) = trim_overlaps(items);
        assert_eq!(out[1].c, "// crumb\n56789ABCDE");
        assert_eq!(stats.trimmed, 0);
    }

    #[test]
    fn middle_gap_uses_elision_marker() {
        // [0,4) and [8,12) already shown; new [0,12) keeps the [4,8) gap only.
        let items = vec![
            r(1, 0, 4, "0123"),
            r(1, 8, 12, "89AB"),
            r(1, 0, 12, "0123456789AB"),
        ];
        let (out, _) = trim_overlaps(items);
        let third = &out[2];
        assert!(third.c.contains("4567"));
        assert!(third.c.contains('…'));
    }
}
```

In `crates/mn-retrieval/src/lib.rs`, add the export alongside the others:

```rust
pub mod dedup;
pub mod facets;
pub mod filters;
pub mod rrf;
```

And add a one-line bullet to the crate-level doc comment list:

```rust
//! - [`dedup`] — result-set overlap dedup over same-document chunk windows.
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p mn-retrieval dedup::`
Expected: PASS (five tests).

- [ ] **Step 3: Commit**

```bash
git add crates/mn-retrieval/src/dedup.rs crates/mn-retrieval/src/lib.rs
git commit -m "feat(mn-retrieval): pure result-set overlap dedup pass"
```

---

### Task 6: Wire dedup into the search route

**Files:**
- Modify: `crates/mn-server/src/routes/search.rs` — `ScoringRow` (~683), `fetch_scoring_rows` SELECT (~705) + decode (~726), `ScoredCandidate` (~607) + its push (~561), `SearchMetadata` (~183), response assembly (~585-599)

> **Verification note:** the search route's behaviour is covered by DB-backed e2e tests gated behind the `integration` feature, which run in CI (per-PR + nightly), not in this sandbox. Local verification here is **build + clippy**; the algorithm itself is already unit-tested in Task 5.

- [ ] **Step 1: Add byte columns to the scoring fetch**

In `ScoringRow` (around line 683) add two fields:

```rust
struct ScoringRow {
    content: String,
    document_id: Uuid,
    source_version_id: Uuid,
    chunk_index: i32,
    total_chunks: i32,
    start_byte: i32,
    end_byte: i32,
    created_at: OffsetDateTime,
    provenance: Provenance,
    source_modified_at: Option<OffsetDateTime>,
    ingested_at: OffsetDateTime,
}
```

In `fetch_scoring_rows`, extend the SELECT column list (line ~706) to include the byte columns:

```rust
        "SELECT chunk.id, chunk.document_id, chunk.source_version_id, chunk.chunk_index, \
                chunk.total_chunks, chunk.start_byte, chunk.end_byte, chunk.content, chunk.created_at, \
                d.provenance AS provenance, d.source_modified_at AS source_modified_at, \
                sv.ingested_at AS ingested_at \
         FROM chunk \
         JOIN document d ON d.id = chunk.document_id \
         JOIN source_version sv ON sv.id = chunk.source_version_id \
         WHERE chunk.id = ANY($1)",
```

And decode them in the row loop (after `total_chunks`):

```rust
                chunk_index: r.try_get("chunk_index")?,
                total_chunks: r.try_get("total_chunks")?,
                start_byte: r.try_get("start_byte")?,
                end_byte: r.try_get("end_byte")?,
                created_at: r.try_get("created_at")?,
```

- [ ] **Step 2: Carry byte offsets on `ScoredCandidate` and implement `OverlapItem`**

Add the two fields to `ScoredCandidate` (around line 607, after `total_chunks`):

```rust
    chunk_index: i32,
    total_chunks: i32,
    start_byte: i32,
    end_byte: i32,
    created_at: OffsetDateTime,
```

Populate them where candidates are pushed (around line 561, after `total_chunks: row.total_chunks,`):

```rust
            chunk_index: row.chunk_index,
            total_chunks: row.total_chunks,
            start_byte: row.start_byte,
            end_byte: row.end_byte,
            created_at: row.created_at,
```

Add the trait import near the top of the file (with the other `use mn_retrieval::...` lines) and implement it. Put the impl just below the `ScoredCandidate` struct:

```rust
impl mn_retrieval::dedup::OverlapItem for ScoredCandidate {
    type Key = Uuid;
    fn document_key(&self) -> Uuid {
        self.document_id
    }
    fn byte_range(&self) -> (usize, usize) {
        let s = usize::try_from(self.start_byte).unwrap_or(0);
        let e = usize::try_from(self.end_byte).unwrap_or(0);
        (s, e.max(s))
    }
    fn content(&self) -> &str {
        &self.content
    }
    fn set_content(&mut self, content: String) {
        self.content = content;
    }
}
```

- [ ] **Step 3: Add the metadata field**

In `SearchMetadata` (around line 190, after `deduplicated_count`):

```rust
    /// How many results were dropped as fully-overlapping duplicates of a
    /// higher-ranked chunk from the same document (rolling-window dedup).
    pub overlap_dropped_count: usize,
```

- [ ] **Step 4: Run dedup after sort, before truncate**

In the handler, replace the sort/truncate block (around lines 591-593) with:

```rust
    // Sort by the requested key (#9), then dedup overlapping same-document
    // windows over the FULL candidate set, then truncate — so dropping a
    // duplicate does not shrink the result page below `limit`.
    sort_candidates(&mut scored, req.sort_by);
    let (mut scored, dedup_stats) = if dedup_enabled() {
        mn_retrieval::dedup::trim_overlaps(scored)
    } else {
        (scored, mn_retrieval::dedup::DedupStats::default())
    };
    scored.truncate(limit as usize);
```

Set the metadata field in the `SearchMetadata { ... }` literal (around line 593-599), after `deduplicated_count,`:

```rust
            deduplicated_count,
            overlap_dropped_count: dedup_stats.dropped,
            filtered_by_confidence,
```

Add the toggle helper near `max_limit()`:

```rust
/// Whether result-set overlap dedup runs. Default on; set `MNM_SEARCH_DEDUP=0`
/// (or `false`) to disable as an escape hatch.
fn dedup_enabled() -> bool {
    !matches!(
        std::env::var("MNM_SEARCH_DEDUP").as_deref(),
        Ok("0") | Ok("false")
    )
}
```

- [ ] **Step 5: Build, lint, and prepare offline sqlx**

Run:
```bash
cargo build -p mn-server
cargo clippy -p mn-server --all-targets -- -D warnings
```
Expected: builds clean. (If the workspace uses sqlx offline mode, regenerate the query cache: `cargo sqlx prepare --workspace -- --tests` — only if `.sqlx/` exists and CI checks it; the query here is runtime `sqlx::query(...)`, not the `query!` macro, so offline prep is typically unaffected.)

- [ ] **Step 6: Commit**

```bash
git add crates/mn-server/src/routes/search.rs
git commit -m "feat(mn-server): apply result-set overlap dedup before truncation"
```

---

## Part C — Code wrapper breadcrumb + coalescing (`mn-content`)

### Task 7: `enclosing_symbol_headers` helper

**Files:**
- Modify: `crates/mn-content/src/code/symbols.rs` (add fn + test)

- [ ] **Step 1: Write a failing test**

Add to the `tests` module in `crates/mn-content/src/code/symbols.rs` (it is gated `#[cfg(all(test, feature = "core-grammars"))]`):

```rust
    #[test]
    fn enclosing_headers_capture_signature_lines() {
        let src = "namespace Big {\n  function big(x: number): number {\n    return x;\n  }\n}\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let off = src.find("return x").unwrap();
        let table = crate::code::ts::ts_kind_table();
        let headers = enclosing_symbol_headers(&tree, src, off, table);
        let lines: Vec<&str> = headers.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(lines, vec!["namespace Big", "function big(x: number): number"]);
        // Outermost first; node_start strictly ascending and < the offset.
        assert!(headers[0].0 < headers[1].0 && headers[1].0 < off);
    }
```

(Add `tree-sitter-typescript` is already available under `core-grammars`; the test module already imports `super::*`.)

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p mn-content --features core-grammars symbols::tests::enclosing_headers_capture_signature_lines`
Expected: FAIL — `cannot find function enclosing_symbol_headers`.

- [ ] **Step 3: Implement**

Add to `crates/mn-content/src/code/symbols.rs`:

```rust
/// Enclosing in-table symbol headers for the node at `byte_offset`, outermost
/// first. Each entry is `(node_start_byte, first_line)` where `first_line` is the
/// symbol's opening source line — trimmed, with a trailing `{` removed.
///
/// Mirrors [`symbol_path_at`]'s descent but captures node geometry, so callers
/// can tell which symbols a chunk *opens* (`node_start == chunk start`) from
/// those it is merely *inside* (`node_start < chunk start`).
#[must_use]
pub fn enclosing_symbol_headers(
    tree: &tree_sitter::Tree,
    src: &str,
    byte_offset: usize,
    table: KindTable,
) -> Vec<(usize, String)> {
    let mut headers = Vec::new();
    let mut node = tree.root_node();
    loop {
        if table.iter().any(|e| e.node_kind == node.kind()) {
            let start = node.start_byte();
            let line_end = src[start..].find('\n').map_or(src.len(), |off| start + off);
            let first_line = src
                .get(start..line_end)
                .unwrap_or_default()
                .trim()
                .trim_end_matches('{')
                .trim_end()
                .to_string();
            headers.push((start, first_line));
        }
        let next = {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|c| c.start_byte() <= byte_offset && byte_offset < c.end_byte())
        };
        match next {
            Some(child) => node = child,
            None => break,
        }
    }
    headers
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p mn-content --features core-grammars symbols::tests::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/code/symbols.rs
git commit -m "feat(mn-content): enclosing_symbol_headers for code breadcrumbs"
```

---

### Task 8: Code coalescing pass

**Files:**
- Modify: `crates/mn-content/src/code/mod.rs` (add `coalesce_code` + `common_prefix_len`, wire into `run_tree_sitter`, renumber at end, add tests)

- [ ] **Step 1: Write failing unit tests**

Add a test module at the bottom of `crates/mn-content/src/code/mod.rs`:

```rust
#[cfg(test)]
mod coalesce_tests {
    use super::*;
    use mn_core::types::SymbolSegment;

    fn seg(kind: &str, name: &str) -> SymbolSegment {
        SymbolSegment { kind: kind.into(), name: name.into() }
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
    fn folds_wrapper_only_fragment_into_child() {
        // body where [0,14) is a tiny wrapper, [14,..] its child; same scope.
        let body = "namespace Big ".to_string() + &"x ".repeat(40);
        let cfg = ChunkerConfig::default(); // code_min_tokens = 64
        let chunks = vec![
            chunk(0, 14, vec![seg("namespace", "Big")]),
            chunk(14, body.len(), vec![seg("namespace", "Big")]),
        ];
        let out = coalesce_code(&body, chunks, &cfg);
        assert_eq!(out.len(), 1, "tiny wrapper should fold into its child");
        assert_eq!(out[0].start_byte, 0);
        assert_eq!(out[0].symbol_path, vec![seg("namespace", "Big")]);
    }

    #[test]
    fn does_not_merge_distinct_top_level_symbols() {
        let body = "fn a fn b".to_string();
        let cfg = ChunkerConfig::default();
        let chunks = vec![
            chunk(0, 4, vec![seg("fn", "a")]),
            chunk(4, 9, vec![seg("fn", "b")]),
        ];
        let out = coalesce_code(&body, chunks, &cfg);
        assert_eq!(out.len(), 2, "unrelated top-level symbols stay separate");
    }

    #[test]
    fn does_not_merge_past_floor() {
        // First chunk already exceeds the floor → stands alone.
        let body = "word ".repeat(80);
        let cfg = ChunkerConfig::default();
        let big = crate::tokens::count(&body[0..body.len() / 2]);
        assert!(big >= cfg.code_min_tokens, "precondition: first half over floor");
        let mid = body.len() / 2;
        let chunks = vec![
            chunk(0, mid, vec![seg("mod", "m"), seg("fn", "a")]),
            chunk(mid, body.len(), vec![seg("mod", "m"), seg("fn", "b")]),
        ];
        let out = coalesce_code(&body, chunks, &cfg);
        assert_eq!(out.len(), 2);
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p mn-content code::coalesce_tests::folds_wrapper_only_fragment_into_child`
Expected: FAIL — `cannot find function coalesce_code`.

- [ ] **Step 3: Implement `coalesce_code` + helper**

Add to `crates/mn-content/src/code/mod.rs`. First extend the imports at the top:

```rust
use crate::chunk::{Chunk, Chunker, ChunkerConfig};
use crate::code::symbols::{symbol_path_at, KindTable};
use mn_core::types::SymbolSegment;
```

Then add the functions (top-level in the module):

```rust
/// Length of the shared symbol-path prefix of `a` and `b`.
fn common_prefix_len(a: &[SymbolSegment], b: &[SymbolSegment]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

/// Light coalescing for code chunks: merge adjacent chunks that share a
/// non-empty enclosing symbol scope until each run reaches `code_min_tokens`,
/// never exceeding `max_tokens`. Folds tiny structural fragments (e.g. a
/// wrapper-only `namespace Foo`) into their following sibling/child while leaving
/// distinct top-level symbols (no shared named scope) separate. Merged runs keep
/// the FIRST chunk's `symbol_path` and span `[first.start, last.end)`.
fn coalesce_code(body: &str, chunks: Vec<Chunk>, cfg: &ChunkerConfig) -> Vec<Chunk> {
    let min = cfg.code_min_tokens;
    let max = cfg.max_tokens;
    let mut out: Vec<Chunk> = Vec::new();
    let mut i = 0usize;
    while i < chunks.len() {
        let mut end = i;
        while end + 1 < chunks.len() {
            let next = &chunks[end + 1];
            // Require a shared *named* enclosing scope (empty prefix = unrelated
            // top-level symbols or file-level preamble → never merge).
            if common_prefix_len(&chunks[i].symbol_path, &next.symbol_path) == 0 {
                break;
            }
            if crate::tokens::count(&body[chunks[i].start_byte..next.end_byte]) > max {
                break;
            }
            if crate::tokens::count(&body[chunks[i].start_byte..chunks[end].end_byte]) >= min {
                break; // run already meets the floor
            }
            end += 1;
        }
        if end == i {
            out.push(chunks[i].clone());
        } else {
            let start_byte = chunks[i].start_byte;
            let end_byte = chunks[end].end_byte;
            let content = body[start_byte..end_byte].to_string();
            out.push(Chunk {
                token_count: crate::tokens::count(&content),
                content,
                symbol_path: chunks[i].symbol_path.clone(),
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

- [ ] **Step 4: Wire it into `run_tree_sitter` and renumber at the end**

In `run_tree_sitter`, the build loop currently sets `chunk_index: u32::try_from(i)...`. Change that line to `chunk_index: 0,` (renumbering moves to the end). Then, replace the tail of the function (the `if chunks.is_empty() { ... } Ok(chunks)` block) with:

```rust
    if chunks.is_empty() {
        return LineWindowChunker.chunk(body, cfg);
    }

    // Pass A: fold tiny same-scope fragments together.
    let mut chunks = coalesce_code(body, chunks, cfg);

    // Assign sequential chunk indices after all transforms.
    for (i, c) in chunks.iter_mut().enumerate() {
        c.chunk_index = u32::try_from(i).unwrap_or(u32::MAX);
    }
    Ok(chunks)
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p mn-content --features core-grammars code::`
Expected: PASS — new `coalesce_tests` plus existing per-language symbol tests.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-content/src/code/mod.rs
git commit -m "feat(mn-content): light same-scope coalescing for code chunks"
```

---

### Task 9: Wrapper breadcrumb pass + per-language comment tokens

**Files:**
- Modify: `crates/mn-content/src/code/mod.rs` (`run_tree_sitter` signature + breadcrumb pass)
- Modify: all 18 `run_tree_sitter` call sites (one comment token each)

- [ ] **Step 1: Write failing tests**

Add to `crates/mn-content/src/code/ts.rs` `tests` module:

```rust
    #[test]
    fn split_symbol_interior_chunk_gets_breadcrumb() {
        // Force a split with a tiny budget so the function body spans >1 chunk.
        let mut src = String::from("function big(x: number): number {\n");
        for i in 0..40 {
            src.push_str(&format!("  const v{i} = x + {i};\n"));
        }
        src.push_str("  return x;\n}\n");
        let cfg = ChunkerConfig { max_tokens: 40, ..ChunkerConfig::default() };
        let chunks = TypeScriptChunker { tsx: false }.chunk(&src, &cfg).unwrap();
        // The first chunk opens the function — no breadcrumb.
        assert!(!chunks[0].content.starts_with("//"));
        // Some later interior chunk carries the signature as a breadcrumb.
        assert!(
            chunks.iter().skip(1).any(|c| c
                .content
                .starts_with("// function big(x: number): number")),
            "an interior chunk should carry the wrapper breadcrumb: {:#?}",
            chunks.iter().map(|c| c.content.lines().next().unwrap_or("")).collect::<Vec<_>>()
        );
    }
```

Add to `crates/mn-content/src/code/rust.rs` `tests` module:

```rust
    #[test]
    fn split_fn_interior_chunk_gets_breadcrumb() {
        let mut src = String::from("fn big(x: i32) -> i32 {\n");
        for i in 0..60 {
            src.push_str(&format!("    let v{i} = x + {i};\n"));
        }
        src.push_str("    x\n}\n");
        let cfg = ChunkerConfig { max_tokens: 40, ..ChunkerConfig::default() };
        let chunks = RustChunker.chunk(&src, &cfg).unwrap();
        assert!(
            chunks.iter().skip(1).any(|c| c.content.starts_with("// fn big(x: i32) -> i32")),
            "an interior chunk should carry the wrapper breadcrumb"
        );
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p mn-content --features core-grammars ts::tests::split_symbol_interior_chunk_gets_breadcrumb`
Expected: FAIL — interior chunks have no breadcrumb yet (and the signature change in Step 3 will also be required to compile).

- [ ] **Step 3: Add the `line_comment` parameter + breadcrumb pass**

Change the `run_tree_sitter` signature in `crates/mn-content/src/code/mod.rs`:

```rust
pub(crate) fn run_tree_sitter(
    body: &str,
    cfg: &ChunkerConfig,
    language: &tree_sitter::Language,
    table: KindTable,
    line_comment: &'static str,
) -> Result<Vec<Chunk>, crate::chunk::ChunkError> {
```

Insert the breadcrumb pass between coalescing and renumbering (from Task 8's tail), so the tail becomes:

```rust
    // Pass A: fold tiny same-scope fragments together.
    let mut chunks = coalesce_code(body, chunks, cfg);

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
```

Also add the `symbols` import path if needed — the module already has `use crate::code::symbols::{symbol_path_at, KindTable};`; the breadcrumb pass calls `symbols::enclosing_symbol_headers`, which resolves via the `pub mod symbols;` declaration, so no import change is required.

- [ ] **Step 4: Update all 18 call sites with their comment token**

Append the comment-token argument to each `run_tree_sitter(...)` call. The token per file:

| File | token | File | token |
|---|---|---|---|
| `rust.rs` | `"//"` | `swift.rs` | `"//"` |
| `ts.rs` | `"//"` | `ruby.rs` | `"#"` |
| `js.rs` | `"//"` | `kotlin.rs` | `"//"` |
| `bash.rs` | `"#"` | `csharp.rs` | `"//"` |
| `scheme.rs` | `";"` | `haskell.rs` | `"--"` |
| `go.rs` | `"//"` | `java.rs` | `"//"` |
| `python.rs` | `"#"` | `html.rs` | `""` |
| `solidity.rs` | `"//"` | `xml.rs` | `""` |
| `toml.rs` | `"#"` | `yaml.rs` | `"#"` |

For the single-line call sites (`ts.rs:59`, `go.rs:33`, `scheme.rs:45`), add the argument inline, e.g.:

```rust
        crate::code::run_tree_sitter(body, cfg, &lang, ts_kind_table(), "//")
```

For the multi-line call sites (`rust.rs`, `js.rs`, `bash.rs`, `xml.rs`, `haskell.rs`, `solidity.rs`, `ruby.rs`, `html.rs`, `java.rs`, `kotlin.rs`, `python.rs`, `csharp.rs`, `toml.rs`, `swift.rs`, `yaml.rs`), add the token as the final argument before the closing `)`, e.g. for `rust.rs`:

```rust
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_rust::LANGUAGE.into(),
            rust_kind_table(),
            "//",
        )
```

- [ ] **Step 5: Run tests under default and all features**

Run:
```bash
cargo test -p mn-content --features core-grammars ts::tests:: rust::tests::
cargo build -p mn-content --all-features
```
Expected: the breadcrumb tests PASS; `--all-features` compiles (confirms every feature-gated call site was updated).

- [ ] **Step 6: Commit**

```bash
git add crates/mn-content/src/code/
git commit -m "feat(mn-content): enclosing-symbol breadcrumb on split code chunks"
```

---

### Task 10: Cleanup + full CI-surface verification

**Files:**
- Delete: `crates/mn-content/tests/scratch_wrapper.rs`

- [ ] **Step 1: Remove the throwaway scratch test**

```bash
git rm crates/mn-content/tests/scratch_wrapper.rs
```

- [ ] **Step 2: Run the full pre-push surface**

Run:
```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```
Expected: formatting clean, no clippy warnings, all tests pass. (DB-gated `integration` tests are exercised in CI, not here — see the Task 6 verification note and the project's "integration tests are CI-only" constraint.)

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "chore(mn-content): remove scratch wrapper probe; finalize chunk-context work"
```

---

## Self-Review

**Spec coverage**
- Markdown rolling window (70% floor, 80% switch, 90% target, sentence edges, smaller-side-first, code/table atomic, expand into `content`) → Tasks 1–4. ✓
- Result-set dedup (mn-retrieval pure pass, same-doc overlap, drop-fully-covered, `…` elision, byte-aligned guard, over-fetch-then-truncate, default-on toggle, separate metadata count) → Tasks 5–6. ✓
- Code wrapper breadcrumb (comment header with enclosing signatures on interior chunks of split symbols; first chunk untouched; per-language tokens; markup langs skipped) → Tasks 7, 9. ✓
- Code coalescing (`code_min_tokens=64`, same-scope only, wrapper-only fold, keep first symbol_path, respect max) → Task 8. ✓
- Config + plumbing (`min_tokens` 280, window pcts, `code_min_tokens`; `start_byte`/`end_byte` into the search SELECT; CLI defaults) → Tasks 1, 6. ✓
- Cleanup (delete `scratch_wrapper.rs`) → Task 10. ✓
- No DB migration required (columns pre-exist) — confirmed, none added. ✓

**Placeholder scan:** none — every code step carries complete code; every run step has an exact command and expected outcome.

**Type/name consistency:** `expand_window`/`segment_sentences`/`grow_side`/`pct_tokens` (Tasks 2–4), `trim_overlaps`/`OverlapItem`/`DedupStats`/`subtract`/`insert` (Task 5) used identically in Task 6, `coalesce_code`/`common_prefix_len` (Task 8) and `enclosing_symbol_headers` (Task 7) used in Task 9. `run_tree_sitter`'s new `line_comment: &'static str` parameter is added in Task 9 and every call site updated in the same task. `ChunkerConfig` field names (`window_switch_pct`, `window_target_pct`, `window_cap_pct`, `code_min_tokens`) are consistent across Tasks 1, 3, 8.

**Independence:** Part A (markdown), Part B (dedup), Part C (code) are independently testable and could ship as separate commits/PRs; they share only the Task 1 config change.

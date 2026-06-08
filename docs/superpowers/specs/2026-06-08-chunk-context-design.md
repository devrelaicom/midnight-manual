# Chunk context enrichment — design

**Date:** 2026-06-08
**Status:** draft
**Touches:** `mn-content` (`chunk.rs` config, `markdown.rs` rolling window +
raised coalescing floor, `code/mod.rs` coalescing + breadcrumb passes,
per-language `line_comment` descriptors), `mn-retrieval` (new `dedup` module),
`mn-server` (`routes/search.rs`: fetch `start_byte`/`end_byte`, call dedup).
No DB migration.

This adds surrounding context to chunks so retrieval hits carry enough
neighbouring text to be useful, and fixes a code-chunking gap where the
enclosing symbol wrapper is lost from a split symbol's interior chunks.

## Problem

Two distinct shortfalls, both observed in the current chunkers:

1. **Markdown chunks are under-contextualised.** Coalescing (PR #75) raised the
   floor for tiny sections, but a chunk still ends at its section boundary and
   carries no surrounding text. A hit on a precise paragraph gives the consumer
   (reranker + LLM) no lead-in or follow-on.

2. **Code split-symbol interior chunks lose their wrapper.** Verified
   empirically (throwaway `tests/scratch_wrapper.rs`):
   - A function that *fits* `max_tokens` keeps its full wrapper —
     `function foo(bar): string { return bar; }` is one chunk. The plain case
     is already correct.
   - A symbol that *exceeds* `max_tokens` is split by `text-splitter` into
     interior body slices like `const v39 = …; return x; }`. The signature is
     gone from `content`; the symbol survives only in `symbol_path`
     (e.g. `[function:big]`).
   - An oversized `namespace`/module emits the wrapper as its own useless
     ~5-token chunk (`namespace Big`), and member bodies don't carry the
     `namespace Big {` wrapper either.

   There is also **no coalescing for code** — `run_tree_sitter` emits
   `text-splitter` ranges directly; the `min_tokens` floor is markdown-only.
   `text-splitter` greedily packs adjacent small nodes but still emits tiny
   structural fragments.

A consequence of (1) once windows are added: adjacent chunks overlap, so a
single document can return near-duplicate text across several hits.

## Goals

- **Markdown rolling window:** coalesce the core body toward ~70% of
  `max_tokens`, then pad with surrounding sentences to ~90%, balancing the two
  sides so a chunk near a document boundary isn't starved.
- **Result-set dedup:** when multiple hits from one document overlap, trim the
  duplicated text so the consumer sees each region once.
- **Code wrapper breadcrumb:** re-attach the enclosing symbol signature(s) to
  the interior chunks of a *split* symbol, as a comment header.
- **Code coalescing:** a light minimum-size floor that folds tiny structural
  fragments (e.g. a wrapper-only `namespace Big`) into their neighbour, without
  merging across unrelated symbols.
- Everything tunable via config; all window markers expressed as a fraction of
  `max_tokens` so they stay correct if the budget changes.

## Non-goals (out of scope)

- **LLM-generated contextual summaries** (Anthropic-style contextual
  retrieval). This is a positional word/sentence window, not a generated
  summary.
- **Passage merging into a single result entry.** Dedup trims and drops; it
  keeps separate ranked hits with their own scores.
- **DB migration.** The window only enlarges `content`; `start_byte`/`end_byte`
  columns already exist.
- **Code overlap windows.** Code chunks get the breadcrumb + coalescing only;
  no rolling word window (semantic boundaries already carry their own context).

## Decisions (resolved during brainstorming)

- **Window target = expand `content` itself.** The window is embedded, FTS-
  indexed, returned, and hashed — all from the single `content` field. Accepted
  trade-offs: broader embeddings, FTS overlap across neighbours, visible
  duplication (the last mitigated by dedup). Viable here because change
  detection is **document-level**, so the window is a pure function of the
  document with no cross-file coupling and no neighbour re-embed churn.
- **Window edges = sentence boundaries** (prose), with fenced code blocks and
  tables treated as atomic units.
- **Core floor = 70%, switch = 80%, target = 90%, cap = 100%.** Balanced: when
  both sides have room each contributes ~10% of `max_tokens`.
- **Wrapper style = comment breadcrumb with signature** (not literal
  reconstruction, not metadata-only).
- **Code coalescing = light floor, same-parent only.**

## Section 1 — Markdown rolling window

A new pass in `MarkdownChunker::chunk`, after coalescing and after the oversize
window-split, augmenting each in-budget chunk's `content` with surrounding
document text. The chunker has the full `body` and each chunk's
`start_byte`/`end_byte`, so the window is computed from absolute offsets.

### Token budget (`max_tokens = 400`)

| Marker | % of max | Tokens | Meaning |
|---|---|---|---|
| Core floor | 70% | 280 | coalescing target (raises `min_tokens` 128 → 280) |
| Switch point | 80% | 320 | fill the *smaller* side until here |
| Final target | 90% | 360 | fill the *other* side until here |
| Hard cap | 100% | 400 | never exceeded |

All markers derive from `pct × max_tokens`.

### Fill algorithm (per core chunk)

1. `before = body[..core.start]`, `after = body[core.end..]`. Compute available
   tokens on each side.
2. If `core_tokens ≥ 360` → no context. If `320 ≤ core_tokens < 360` → skip to
   step 4 using the *larger* side only.
3. **Phase 1 — smaller side first.** "Smaller" = the side with fewer available
   tokens. Add whole sentences from that side, working inward-out (nearest the
   core first), until the running total reaches ~320 **or** that side is
   exhausted (hit doc start/end).
4. **Phase 2 — switch sides.** Add sentences from the other side until the total
   reaches ~360 **or** that side is exhausted.
5. The expanded span `[new_start, new_end)` becomes the chunk's `content`,
   `start_byte`, `end_byte`; `token_count` and `content_hash` recompute over it.

Ties on "smaller" resolve to the `before` side by convention.

### Worked examples (core = 280 tokens, i.e. at the floor)

- `before`=500, `after`=25 → smaller is `after`. Add all 25 (exhausted) → 305.
  Switch to `before`, add 55 → **360**. ⇒ 55 before / 25 after (the constrained
  side gives what it has; the other side fills the rest).
- `before`=15, `after`=600 → smaller is `before`. Add 15 (exhausted) → 295.
  Switch to `after`, add 65 → **360**. ⇒ 15 before / 65 after.
- `before`=400, `after`=400 (tie → before) → fill `before` to the 320 switch
  (+40), switch, fill `after` to 360 (+40). ⇒ **40 before / 40 after** —
  balanced, each side ~10% of `max_tokens`.

**Undersized core** (below the floor — a lone section that couldn't coalesce,
e.g. core = 200) with both sides ample: Phase 1 fills the smaller side to 320
(+120), Phase 2 the other to 360 (+40). The window compensates for the small
core, so the split is front-heavy by design — balance (~10%/10%) holds only
when the core is at the floor.

### Sentence segmentation rules

- **Prose:** split on terminal punctuation `.`/`!`/`?` followed by
  whitespace/newline; the punctuation stays with its sentence.
- **Atomic units** (never split; each counts as one unit): fenced code blocks
  ` ``` … ``` `, tables (`|…|` runs). If adding an atomic unit would breach the
  100% cap, stop that side rather than partially include it.
- Context may cross heading boundaries — intended.

### Config

`ChunkerConfig`:

- Replace `min_tokens` (128) with `window_core_pct: f32 = 0.70`.
- Add `window_switch_pct: f32 = 0.80`, `window_target_pct: f32 = 0.90`,
  `window_cap_pct: f32 = 1.00`.

Oversize sections routed through `token_window_split` are already ~`max`-sized
and receive **no** added context.

## Section 1b — Result-set overlap dedup

A reusable function in **`mn-retrieval`** (new `dedup` module), called from
`mn-server/src/routes/search.rs` **after scoring/rerank, before truncation**.
MCP and the CLI consume the HTTP search endpoint, so they inherit it with no
per-client work.

### Required plumbing

`fetch_scoring_rows` (search.rs) does not currently SELECT `start_byte` /
`end_byte`. Add both columns to the query and the internal scoring-row /
candidate structs. They feed dedup; they need not be exposed in the public
`SearchResult` response.

### Algorithm

Results carry `document_id`, `start_byte`, `end_byte`, `content`. For markdown
windowed chunks `content == body[start..end]`, so byte offsets map cleanly into
`content`.

1. Group results by `document_id`. Cross-document never dedups.
2. Process best-rank-first, maintaining the set of byte intervals already shown
   for that document.
3. For each subsequent same-doc chunk, subtract the covered intervals from its
   `[start_byte, end_byte)`:
   - **Fully covered** → drop from results.
   - **Partially covered** → trim `content` to the uncovered sub-range(s),
     joining non-contiguous kept pieces with an `…` elision marker. Score, rank
     position, and byte metadata are preserved.
4. Order is never changed; only content is trimmed and full duplicates removed.

### Guards

- **Byte-aligned only:** trim a chunk only when `content.len() == end_byte -
  start_byte`. Code chunks carrying a synthetic breadcrumb prefix (Section 2)
  fail this and are left untouched — they barely overlap anyway.
- **K preservation:** over-fetch candidates, then dedup, then take top-K, so a
  dropped duplicate doesn't shorten the result set.
- **Toggle:** a retrieval/server-side `dedup` flag, default on (not part of
  `ChunkerConfig`).

## Section 2 — Code wrapper breadcrumb + code coalescing

Both live in `run_tree_sitter` (`crates/mn-content/src/code/mod.rs`) as two
passes after the tree-sitter split. **Pass order:** split → coalesce (A) →
breadcrumb (B) → renumber `chunk_index`, recompute `token_count` /
`content_hash` where changed.

### Pass A — code coalescing

Mirrors markdown's `coalesce_segments`, keyed on `symbol_path` instead of
heading depth.

- Merge *adjacent* chunks up to a small floor, **only when they share an
  enclosing scope** — same `symbol_path`, or one is an ancestor/prefix of the
  other. Never merge across unrelated top-level symbols (different
  `symbol_path` roots). Never exceed `max_tokens`.
- **Wrapper-only fragment:** a tiny header chunk (e.g. `namespace Big`) whose
  next chunk is its child is folded into that child. The merged chunk keeps the
  shallower (first) `symbol_path`.
- Floor: new `code_min_tokens: u32 = 64` — enough to kill fragments without
  over-merging distinct functions (separate from markdown's 280, which would be
  too aggressive for code). Tunable.

### Pass B — wrapper breadcrumb

- A chunk is **interior** if its `start_byte` is past the start of an enclosing
  in-table symbol node (it does not open that symbol). The *first* chunk of a
  symbol already shows the real signature and is left alone.
- Prepend a line-comment breadcrumb built from each enclosing symbol's **first
  source line** (trimmed), outermost→innermost, joined by ` > `, with a
  continuation marker:

  ```
  // namespace Big > function big(x: number): number { … (continued)
    const v39 = x + 39 * 2 - 39;
    return x;
  }
  ```

- Needs a per-language line-comment token: add `line_comment: &'static str` to
  each language descriptor (`//` for C-likes, `#` for python/ruby/bash/yaml/
  toml, `--` haskell, `;` scheme). Markup languages without a line comment
  (html/xml) use the block form or skip the breadcrumb.

### Invariant change

For breadcrumbed chunks, `content = synthetic_prefix + body[range]`, so
`content` is no longer byte-identical to `body[start..end]`. The byte range
still points at the real source slice; `content_hash` / `token_count` compute
over the augmented content. This is exactly what the dedup byte-aligned guard
(Section 1b) keys on, so such chunks are never mis-sliced.

## Cross-cutting

- **Storage:** no migration. `content` grows; `content_hash` / `token_count` /
  byte range recompute over it; columns already exist.
- **Embedding / FTS:** markdown embeds grow to ~280–360 tokens (was 128–400) —
  modest cost increase, richer context, accepted precision trade-off. FTS
  overlap across neighbours is mitigated by dedup; the reranker benefits from
  the added context.

## Testing

- **Markdown window:** smaller-side-first fill; side exhaustion at doc boundary;
  balanced tie; core ≥ switch skips Phase 1; core ≥ target ⇒ no window; sentence
  edges; code-fence/table atomicity; cap never exceeded.
- **Dedup:** same-doc overlap trimmed; fully-covered dropped; `…` elision
  between non-contiguous pieces; cross-doc never dedups; byte-aligned guard
  skips augmented/code content; order preserved; over-fetch preserves K.
- **Code:** interior chunk of a split symbol gets the breadcrumb with signature;
  first chunk does not; nested enclosers; per-language comment tokens;
  coalescing merges tiny same-scope fragments only; wrapper-only fragment folded
  into child; `max_tokens` respected.
- **Maintenance:** update existing tests that assume `content == body[range]`
  for code or depend on `min_tokens` / chunk counts / sizes; **delete
  `crates/mn-content/tests/scratch_wrapper.rs`**.
- Optional `proptest`: window never exceeds the cap; dedup output bytes ⊆ union
  of input bytes (no fabricated text); `chunk_index` sequential.

## Files touched (estimate)

- `crates/mn-content/src/chunk.rs` — `ChunkerConfig` fields.
- `crates/mn-content/src/markdown.rs` — raised core floor; new window pass +
  sentence segmentation.
- `crates/mn-content/src/code/mod.rs` — coalescing pass A + breadcrumb pass B.
- `crates/mn-content/src/code/language.rs` + per-language descriptors —
  `line_comment` token.
- `crates/mn-content/src/code/symbols.rs` — helpers for interior detection /
  enclosing-symbol first line (if needed).
- `crates/mn-retrieval/src/` — new `dedup` module (+ `lib.rs` export).
- `crates/mn-server/src/routes/search.rs` — fetch `start_byte`/`end_byte`; call
  dedup after scoring, before truncation; dedup toggle.
- Tests across the above; delete `crates/mn-content/tests/scratch_wrapper.rs`.

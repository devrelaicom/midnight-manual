# Code Chunkers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build Phase 6 code chunking — real per-language semantic chunkers for every in-scope language except Compact, replacing the line-window stopgap that currently text-blobs all code.

**Architecture:** A shared `Chunker` trait (in `mn-content/src/chunk.rs`) implemented by the existing markdown chunker (pulldown-cmark), a new code chunker family (tree-sitter + `text-splitter`, one module per language), and a line-window fallback. File-list generation uses the `ignore` crate. `symbol_path` becomes structured `{kind,name}` stored as JSONB. Grammars are Cargo-feature-gated and degrade to line-window when absent.

**Tech Stack:** Rust, `tree-sitter` + per-language grammar crates, `text-splitter` (token-budgeted `CodeSplitter`), `ignore` (gitignore-aware walking), `pulldown-cmark` (markdown, retained), `sqlx`/Postgres (JSONB migration), `toml`/`serde_json` (package detection).

**Spec:** `docs/superpowers/specs/2026-05-28-code-chunkers-design.md`

**Branch:** `spec/code-chunkers` (already created).

---

## Implementation notes for the engineer

- **MSRV/toolchain**: workspace pins Rust 1.91. Run all checks with the workspace toolchain.
- **Per-task verification gate**: every task ends green on `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the task's tests. The repo has a `just check` recipe.
- **Implementation refinement vs spec**: the spec describes a unified in-memory `path: Vec<PathSegment>`. For zero-translation persistence we instead give the `Chunk` struct **two** fields — `heading_path: Vec<String>` (markdown) and `symbol_path: Vec<SymbolSegment>` (code) — matching the two existing DB columns 1:1. Same outcome, simpler mapping.
- **Grammar/runtime ABI**: every `tree-sitter-<lang>` crate must be ABI-compatible with the pinned `tree-sitter` runtime. When `cargo add`-ing a grammar, if the build fails with an ABI/version error, pin the grammar to the newest version whose `tree-sitter` requirement matches ours. Record chosen versions in `Cargo.toml` comments.
- **`tokf` note**: some shell tool output in this repo is token-filtered. When grepping, trust the files over filtered terminal output.

---

# Phase A — Shared types, migration, dependencies

## Task 1: `SymbolSegment` type in `mn-core` ✅ DONE

**Files:**
- Modify: `crates/mn-core/src/types.rs` (add after `DocumentKind`, ~line 236)
- Test: same file's `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Add to the tests module in `crates/mn-core/src/types.rs`:

```rust
#[test]
fn symbol_segment_json_roundtrip() {
    let seg = SymbolSegment { kind: "impl".to_string(), name: "Foo".to_string() };
    let json = serde_json::to_string(&seg).unwrap();
    assert_eq!(json, r#"{"kind":"impl","name":"Foo"}"#);
    let back: SymbolSegment = serde_json::from_str(&json).unwrap();
    assert_eq!(back, seg);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mn-core symbol_segment_json_roundtrip`
Expected: FAIL — `cannot find type SymbolSegment`.

- [ ] **Step 3: Add the type**

In `crates/mn-core/src/types.rs`, after the `DocumentKind` enum:

```rust
/// One segment of a code chunk's symbol path — e.g. `{kind:"impl", name:"Foo"}`.
/// Persisted as JSONB in `chunk.symbol_path`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SymbolSegment {
    /// Syntactic kind: "impl", "fn", "class", "interface", "key", "element", …
    pub kind: String,
    /// Identifier or label for this segment.
    pub name: String,
}
```

(If `serde` isn't already imported in this file, use the fully-qualified `serde::` path as shown.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mn-core symbol_segment_json_roundtrip`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-core/src/types.rs
git commit -m "feat(mn-core): add SymbolSegment type for structured symbol paths"
```

---

## Task 2: Migration 0007 — `chunk.symbol_path` → JSONB ✅ DONE

**Files:**
- Create: `crates/mn-store/migrations/0007_symbol_path_jsonb.sql`

- [ ] **Step 1: Write the migration**

```sql
-- Change chunk.symbol_path from text[] to jsonb to hold structured
-- {kind,name} segments. Greenfield: no code chunks exist yet (markdown uses
-- heading_path), so existing rows hold only the empty-array default.
ALTER TABLE chunk
    ALTER COLUMN symbol_path DROP DEFAULT;

ALTER TABLE chunk
    ALTER COLUMN symbol_path TYPE jsonb
    USING '[]'::jsonb;

ALTER TABLE chunk
    ALTER COLUMN symbol_path SET DEFAULT '[]'::jsonb;

ALTER TABLE chunk
    ALTER COLUMN symbol_path SET NOT NULL;

-- GIN index so symbol_path containment queries (@>) are fast,
-- e.g. find all chunks inside an `fn`: symbol_path @> '[{"kind":"fn"}]'.
CREATE INDEX idx_chunk_symbol_path ON chunk USING gin (symbol_path);
```

- [ ] **Step 2: Apply and verify against a scratch DB**

Run:
```bash
sqlx migrate run --source crates/mn-store/migrations
sqlx migrate info --source crates/mn-store/migrations | tail -3
```
Expected: `0007/symbol path jsonb ... installed`.

If you don't have a DATABASE_URL handy, the integration test suite boots one; verify there instead in Step 4 of Task 3.

- [ ] **Step 3: Commit**

```bash
git add crates/mn-store/migrations/0007_symbol_path_jsonb.sql
git commit -m "feat(mn-store): migrate chunk.symbol_path to jsonb (0007)"
```

---

## Task 3: `mn-store` chunk entity → structured symbol_path ✅ DONE

**Files:**
- Modify: `crates/mn-store/src/entities/chunk.rs` (`NewChunk` field ~line 92; INSERT bind ~line 133; any read struct ~line 275)
- Test: integration test in `crates/mn-store/tests/` (find the existing chunk insert/read test; extend it)

- [ ] **Step 1: Write the failing test**

Locate the existing chunk insert+read integration test (grep `insert.*chunk` under `crates/mn-store/tests/`). Add a case that inserts a chunk with a non-empty structured symbol_path and reads it back:

```rust
#[tokio::test]
#[cfg(feature = "integration")]
async fn chunk_symbol_path_roundtrips_structured() {
    // ... reuse the test harness's pool + seeded source_version/document/node ...
    let segs = vec![
        mn_core::types::SymbolSegment { kind: "impl".into(), name: "Foo".into() },
        mn_core::types::SymbolSegment { kind: "fn".into(), name: "bar".into() },
    ];
    let id = chunk::insert(&pool, NewChunk {
        // ... existing required fields from the harness ...
        symbol_path: &segs,
        ../* existing builder */
    }).await.unwrap();
    let got = chunk::symbol_path_of(&pool, id).await.unwrap();
    assert_eq!(got, segs);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-store --features integration chunk_symbol_path_roundtrips_structured`
Expected: FAIL — `symbol_path` expects `&[String]`, and `symbol_path_of` doesn't exist.

- [ ] **Step 3: Change the field, bind, and add a reader**

In `crates/mn-store/src/entities/chunk.rs`:

Change the `NewChunk` field:
```rust
    /// Code-symbol path (structured, persisted as JSONB).
    pub symbol_path: &'a [mn_core::types::SymbolSegment],
```

Change the bind in `insert` (the `.bind(c.symbol_path)` line) to wrap in sqlx JSON:
```rust
    .bind(sqlx::types::Json(c.symbol_path))
```

Add a reader helper:
```rust
/// Read a chunk's structured symbol path. Test/diagnostic helper.
///
/// # Errors
/// Propagates query errors.
pub async fn symbol_path_of(pool: &PgPool, id: Uuid) -> Result<Vec<mn_core::types::SymbolSegment>> {
    let row: (sqlx::types::Json<Vec<mn_core::types::SymbolSegment>>,) =
        sqlx::query_as("SELECT symbol_path FROM chunk WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?;
    Ok(row.0 .0)
}
```

Any existing read path that selected `symbol_path` as `Vec<String>` (search the file) must change its row type to `sqlx::types::Json<Vec<SymbolSegment>>` and unwrap `.0`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-store --features integration chunk_symbol_path_roundtrips_structured`
Expected: PASS (boots Postgres, applies migrations incl. 0007).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-store/src/entities/chunk.rs crates/mn-store/tests
git commit -m "feat(mn-store): structured symbol_path via JSONB in chunk entity"
```

---

## Task 4: `mn-server` ChunkUpload → structured symbol_path ✅ DONE

**Files:**
- Modify: `crates/mn-server/src/routes/admin_ingest.rs` (`ChunkUpload.symbol_path` ~line 134; the two `symbol_path: &chunk_upload.symbol_path` / `&prior.symbol_path` binds ~lines 659, 726; any prior-chunk read struct)
- Test: extend the existing admin_ingest route test (grep `ChunkUpload` in `crates/mn-server/tests/`)

- [ ] **Step 1: Write the failing test**

Extend the upload route test to post a chunk carrying a structured symbol_path and assert it persists (read back via `chunk::symbol_path_of` from Task 3):

```rust
// in the existing admin-ingest upload test
let upload = ChunkUpload {
    // ... existing fields ...
    symbol_path: vec![
        mn_core::types::SymbolSegment { kind: "class".into(), name: "Widget".into() },
    ],
    ..Default::default() // if ChunkUpload derives Default; else fill fields
};
// ... post, then ...
let segs = mn_store::entities::chunk::symbol_path_of(&pool, persisted_chunk_id).await.unwrap();
assert_eq!(segs[0].name, "Widget");
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-server --features integration <upload_test_name>`
Expected: FAIL — `symbol_path` expects `Vec<String>`.

- [ ] **Step 3: Change the wire type and binds**

In `crates/mn-server/src/routes/admin_ingest.rs`, change the field:
```rust
    /// Symbol path (code), structured {kind,name}.
    #[serde(default)]
    pub symbol_path: Vec<mn_core::types::SymbolSegment>,
```

The two `NewChunk { … symbol_path: &chunk_upload.symbol_path … }` and `… symbol_path: &prior.symbol_path …` sites already pass `&Vec<…>` which coerces to `&[SymbolSegment]` — no change needed beyond the type. If a "prior chunk" struct (used to re-link carried chunks) reads `symbol_path` from the DB as `Vec<String>`, change it to `Vec<SymbolSegment>` via `sqlx::types::Json` (mirror Task 3's reader).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-server --features integration <upload_test_name>`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/routes/admin_ingest.rs crates/mn-server/tests
git commit -m "feat(mn-server): structured symbol_path on ChunkUpload + carry path"
```

---

## Task 5: Dependencies + feature flags ✅ DONE (text-splitter pinned 0.27 not 0.30; crates are tree-sitter-toml-ng / tree-sitter-kotlin-ng; runtime tree-sitter 0.25.10)

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `crates/mn-content/Cargo.toml` (`[dependencies]`, `[features]`, `[build-dependencies]`)

- [ ] **Step 1: Add workspace deps**

In root `Cargo.toml` `[workspace.dependencies]` (verify each version with `cargo add --dry-run <crate>` so the `tree-sitter` ABI lines up; pin the runtime first, then grammars to match):

```toml
tree-sitter            = "0.25"   # pin first; all grammars must match this ABI
text-splitter          = { version = "0.30", features = ["code", "tokenizers"] }
ignore                 = "0.4"
tree-sitter-rust       = "0.24"
tree-sitter-typescript = "0.23"
tree-sitter-javascript = "0.23"
tree-sitter-bash       = "0.25"
tree-sitter-go         = "0.25"
tree-sitter-python     = "0.25"
tree-sitter-solidity   = "1.2"    # JoranHonig canonical
tree-sitter-toml       = "0.7"    # tree-sitter-grammars fork
tree-sitter-yaml       = "0.7"    # tree-sitter-grammars fork
tree-sitter-html       = "0.23"
tree-sitter-xml        = "0.7"
tree-sitter-swift      = "0.7"
tree-sitter-ruby       = "0.23"
tree-sitter-kotlin     = "0.3"    # fwcd
tree-sitter-c-sharp    = "0.23"
tree-sitter-haskell    = "0.23"
tree-sitter-java       = "0.23"
cc                     = "1"      # build-dep: compile vendored Scheme parser.c
```

> The exact versions above are starting points — `cargo add` may select newer. The hard rule: every grammar's `tree-sitter` dependency must equal the runtime version you pinned. If a grammar lags, either pin it back or pin the runtime to its level.

- [ ] **Step 2: Wire features in `crates/mn-content/Cargo.toml`**

```toml
[dependencies]
tree-sitter   = { workspace = true }
text-splitter = { workspace = true }
ignore        = { workspace = true }
# Grammar crates are optional; each enabled by a feature below.
tree-sitter-rust       = { workspace = true, optional = true }
tree-sitter-typescript = { workspace = true, optional = true }
tree-sitter-javascript = { workspace = true, optional = true }
tree-sitter-bash       = { workspace = true, optional = true }
tree-sitter-go         = { workspace = true, optional = true }
tree-sitter-python     = { workspace = true, optional = true }
tree-sitter-solidity   = { workspace = true, optional = true }
tree-sitter-toml       = { workspace = true, optional = true }
tree-sitter-yaml       = { workspace = true, optional = true }
tree-sitter-html       = { workspace = true, optional = true }
tree-sitter-xml        = { workspace = true, optional = true }
tree-sitter-swift      = { workspace = true, optional = true }
tree-sitter-ruby       = { workspace = true, optional = true }
tree-sitter-kotlin     = { workspace = true, optional = true }
tree-sitter-c-sharp    = { workspace = true, optional = true }
tree-sitter-haskell    = { workspace = true, optional = true }
tree-sitter-java       = { workspace = true, optional = true }

[build-dependencies]
cc = { workspace = true }

[features]
default = ["core-grammars"]
# Scheme is vendored (no crate) — its parser.c compiles unconditionally in
# build.rs, but its chunker module is gated by `scheme` so default builds get it.
core-grammars = [
    "dep:tree-sitter-rust", "dep:tree-sitter-typescript",
    "dep:tree-sitter-javascript", "dep:tree-sitter-bash", "scheme",
]
scheme = []
markup-grammars = [
    "dep:tree-sitter-toml", "dep:tree-sitter-yaml",
    "dep:tree-sitter-html", "dep:tree-sitter-xml",
]
extended-grammars = [
    "dep:tree-sitter-go", "dep:tree-sitter-python", "dep:tree-sitter-solidity",
]
all-grammars = [
    "markup-grammars", "extended-grammars",
    "dep:tree-sitter-swift", "dep:tree-sitter-ruby", "dep:tree-sitter-kotlin",
    "dep:tree-sitter-c-sharp", "dep:tree-sitter-haskell", "dep:tree-sitter-java",
]
```

- [ ] **Step 3: Verify it builds (no usage yet)**

Run: `cargo build -p mn-content && cargo build -p mn-content --features all-grammars`
Expected: both succeed (grammars compile but are unused — that's fine).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/mn-content/Cargo.toml Cargo.lock
git commit -m "build(mn-content): add tree-sitter/text-splitter/ignore deps + grammar features"
```

---

# Phase B — Shared trait, config, fallback, markdown refactor

## Task 6: Shared `Chunker` trait + `Chunk` + `ChunkerConfig` + `ChunkError` ✅ DONE

**Files:**
- Create: `crates/mn-content/src/chunk.rs`
- Modify: `crates/mn-content/src/lib.rs` (add `pub mod chunk;`)

- [ ] **Step 1: Write the failing test**

In `crates/mn-content/src/chunk.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_token_budgeted() {
        let c = ChunkerConfig::default();
        assert_eq!(c.max_tokens, 400);
        assert_eq!(c.fallback_lines, 60);
        assert_eq!(c.fallback_overlap_lines, 20);
        assert_eq!(c.max_file_bytes, 10 * 1024 * 1024);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content chunk::tests::default_config_is_token_budgeted`
Expected: FAIL — module `chunk` not found.

- [ ] **Step 3: Implement the module**

`crates/mn-content/src/chunk.rs`:

```rust
//! Shared chunker contract: one trait, one config, one output shape, used by
//! the markdown chunker, the code chunkers, and the line-window fallback.

use mn_core::types::SymbolSegment;

/// One chunk emitted by any [`Chunker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Verbatim chunk content.
    pub content: String,
    /// Ancestor heading path (markdown). Empty for code/plaintext.
    pub heading_path: Vec<String>,
    /// Structured symbol path (code). Empty for markdown/plaintext.
    pub symbol_path: Vec<SymbolSegment>,
    /// Byte offset of the chunk's first character in the source.
    pub start_byte: usize,
    /// Byte offset just past the chunk's last character.
    pub end_byte: usize,
    /// Token count of this chunk (BPE, via [`crate::tokens::count`]).
    pub token_count: u32,
    /// 0-indexed position among the document's chunks.
    pub chunk_index: u32,
    /// True iff produced by the line-window fallback.
    pub fallback_used: bool,
}

/// Configuration shared by all chunkers. Token-budgeted.
#[derive(Debug, Clone, Copy)]
pub struct ChunkerConfig {
    /// Max chunk size in BPE tokens before splitting.
    pub max_tokens: u32,
    /// Line-window fallback size (lines).
    pub fallback_lines: u32,
    /// Line-window fallback overlap (lines).
    pub fallback_overlap_lines: u32,
    /// Files larger than this are skipped by callers (EC-52).
    pub max_file_bytes: u64,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            max_tokens: 400,
            fallback_lines: 60,
            fallback_overlap_lines: 20,
            max_file_bytes: 10 * 1024 * 1024,
        }
    }
}

/// Errors a chunker can surface for one file. Never panics; the planner maps
/// these to a per-file warning (default) or a run failure (`--strict`).
#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    /// The parser failed badly enough that we fell back to line-window.
    /// Carries the reason for the warning message.
    #[error("parser fell back to line-window: {0}")]
    ParserFallback(String),
}

/// The chunking contract. Implementations: markdown, each code language,
/// and the line-window fallback.
pub trait Chunker {
    /// Chunk `body`. Returns at least one chunk for non-empty, non-whitespace
    /// input; an empty vec for empty/whitespace input.
    ///
    /// # Errors
    /// Returns [`ChunkError`] only when the caller asked for strict behavior;
    /// the default implementations recover internally and return `Ok`.
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError>;
}
```

Add to `crates/mn-content/src/lib.rs` (in the `pub mod` list):
```rust
pub mod chunk;
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content chunk::tests::default_config_is_token_budgeted`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/chunk.rs crates/mn-content/src/lib.rs
git commit -m "feat(mn-content): shared Chunker trait, Chunk, token-budgeted ChunkerConfig"
```

---

## Task 7: Line-window fallback chunker ✅ DONE

**Files:**
- Create: `crates/mn-content/src/code/line_window.rs`
- Create: `crates/mn-content/src/code/mod.rs` (skeleton — fleshed out in Task 10)
- Modify: `crates/mn-content/src/lib.rs` (add `pub mod code;`)

- [ ] **Step 1: Write the failing test**

In `crates/mn-content/src/code/line_window.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn splits_into_overlapping_line_windows() {
        let body: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let cfg = ChunkerConfig { fallback_lines: 60, fallback_overlap_lines: 20, ..ChunkerConfig::default() };
        let chunks = LineWindowChunker.chunk(&body, &cfg).unwrap();
        assert!(chunks.len() >= 3, "200 lines / (60-20 step) ≈ 5 windows");
        assert!(chunks.iter().all(|c| c.fallback_used));
        assert!(chunks.iter().all(|c| c.symbol_path.is_empty()));
        // overlap: window 2 starts before window 1 ends (by line)
        assert!(chunks[1].start_byte < chunks[0].end_byte);
    }

    #[test]
    fn empty_input_no_chunks() {
        assert!(LineWindowChunker.chunk("  \n ", &ChunkerConfig::default()).unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content code::line_window`
Expected: FAIL — module/type missing.

- [ ] **Step 3: Implement**

`crates/mn-content/src/code/mod.rs` (skeleton):
```rust
//! Code chunkers: tree-sitter + text-splitter per language, plus the shared
//! line-window fallback. Dispatch lands in Task 10.

pub mod line_window;
```

Add to `crates/mn-content/src/lib.rs`:
```rust
pub mod code;
```

`crates/mn-content/src/code/line_window.rs`:
```rust
//! Line-window fallback — used for unknown languages, parser-error recovery,
//! and Compact (until compactp). No syntax awareness; overlapping windows.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};

/// Splits source into fixed line-count windows with overlap.
pub struct LineWindowChunker;

impl Chunker for LineWindowChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }
        // Precompute byte offset of the start of each line.
        let mut line_starts = vec![0usize];
        for (i, b) in body.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let total_lines = line_starts.len();
        let window = cfg.fallback_lines.max(1) as usize;
        let overlap = cfg.fallback_overlap_lines.min(cfg.fallback_lines.saturating_sub(1)) as usize;
        let step = window.saturating_sub(overlap).max(1);

        let mut chunks = Vec::new();
        let mut start_line = 0usize;
        let mut idx = 0u32;
        while start_line < total_lines {
            let end_line = (start_line + window).min(total_lines);
            let start_byte = line_starts[start_line];
            let end_byte = if end_line < total_lines { line_starts[end_line] } else { body.len() };
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
            if end_line >= total_lines { break; }
            start_line += step;
        }
        Ok(chunks)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content code::line_window`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/code crates/mn-content/src/lib.rs
git commit -m "feat(mn-content): line-window fallback chunker"
```

---

## Task 8: Refactor markdown chunker to implement `Chunker` (token units) ✅ DONE

**Files:**
- Modify: `crates/mn-content/src/markdown.rs` (replace `ChunkerConfig` usage with the shared one; impl `Chunker`; switch to token budgeting; update tests)

- [ ] **Step 1: Update the existing tests to the new contract**

In `crates/mn-content/src/markdown.rs` tests, replace byte-unit config literals with token units and the new output type. Example replacements:

```rust
// was: ChunkerConfig { max_bytes: 1000, window_bytes: 800, overlap_bytes: 100 }
let cfg = crate::chunk::ChunkerConfig { max_tokens: 50, ..crate::chunk::ChunkerConfig::default() };
// was: chunk_markdown(md, ChunkerConfig::default())
let chunks = MarkdownChunker.chunk(md, &crate::chunk::ChunkerConfig::default()).unwrap();
// heading_path assertions stay; access via chunks[i].heading_path
// over_sized test: assert chunks split when a section exceeds max_tokens
```

Keep `nested_headings_record_path`, `chunk_indices_are_sequential`, `empty_input_produces_no_chunks`, `headingless_document_produces_chunks` — adapt their construction to `MarkdownChunker.chunk(...).unwrap()`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p mn-content markdown`
Expected: FAIL — `MarkdownChunker` missing; `ChunkerConfig` fields changed.

- [ ] **Step 3: Refactor the implementation**

In `crates/mn-content/src/markdown.rs`:
- Delete the local `ChunkerConfig` struct and its `Default`. Import the shared one: `use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};`
- Delete `MarkdownChunk` (replace its uses with `Chunk`). Where it set `heading_path`, also set `symbol_path: Vec::new()`, `token_count`, `fallback_used: false`.
- Replace the free function `chunk_markdown` with a `MarkdownChunker` unit struct implementing `Chunker`. Keep the heading-walk logic verbatim; change the over-size decision from `text.len() <= cfg.max_bytes` (bytes) to tokens:

```rust
pub struct MarkdownChunker;

impl Chunker for MarkdownChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        // ... existing first-pass heading segmentation, unchanged ...
        // Second pass: token-budget instead of byte-budget.
        let mut chunks: Vec<Chunk> = Vec::new();
        for seg in segments {
            let text = &body[seg.start..seg.end];
            if text.trim().is_empty() { continue; }
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
                for window in token_window_split(text, seg.start, cfg) {
                    chunks.push(window); // window builder sets fields incl. heading_path
                }
            }
        }
        for (i, c) in chunks.iter_mut().enumerate() {
            c.chunk_index = u32::try_from(i).unwrap_or(u32::MAX);
        }
        Ok(chunks)
    }
}
```

- Replace `window_split` (byte windows) with `token_window_split` that grows a window line-by-line (or paragraph-by-paragraph) until `tokens::count` would exceed `cfg.max_tokens`, then steps back by an overlap proportional to `cfg.fallback_overlap_lines`. Reuse `char_boundary_at_or_below` for safety. Each emitted window is a full `Chunk` carrying the segment's `heading_path`, `fallback_used: false` (markdown's own windowing is not the line-window fallback), and its `token_count`.
- Keep a thin `#[must_use] pub fn chunk_markdown(body, cfg) -> Vec<Chunk>` wrapper that calls `MarkdownChunker.chunk(body, &cfg).unwrap_or_default()` **only if** other modules still call it; otherwise update call sites (Task 9 updates `plan.rs`).

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p mn-content markdown`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/markdown.rs
git commit -m "refactor(mn-content): markdown chunker implements Chunker, token-budgeted"
```

---

## Task 9: Planner — add `symbol_path` to `PlannedChunk`, map through to upload ✅ DONE

**Files:**
- Modify: `crates/mn-content/src/ingest/plan.rs` (`PlannedChunk` ~line 47; chunk-build loop ~line 273)
- Modify: `crates/mn-cli/src/commands/ingest/run.rs` (the `PlannedChunk` → `ChunkUpload` mapping)

- [ ] **Step 1: Write the failing test**

In `plan.rs` tests, assert a planned chunk carries an (empty, for markdown) structured symbol_path and that the field exists:

```rust
#[test]
fn planned_chunk_has_symbol_path_field() {
    // build a minimal markdown PlannedDocument via the builder (reuse existing test helpers)
    // assert the first chunk's symbol_path is an (empty) Vec<SymbolSegment>
    let pc = /* first planned chunk from a markdown doc */;
    assert!(pc.symbol_path.is_empty());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content planned_chunk_has_symbol_path_field`
Expected: FAIL — no `symbol_path` field.

- [ ] **Step 3: Add the field and populate it**

In `PlannedChunk` add:
```rust
    /// Structured code-symbol path. Empty for markdown/plaintext.
    pub symbol_path: Vec<mn_core::types::SymbolSegment>,
```

In the chunk-build loop (currently mapping markdown `Chunk` → `PlannedChunk`), set:
```rust
    symbol_path: c.symbol_path,
    heading_path: c.heading_path,
    // start_byte/end_byte/content/token_count already mapped
```
(Now that `Chunk` carries both paths, map both straight through.)

In `crates/mn-cli/src/commands/ingest/run.rs`, the local struct that becomes `ChunkUpload` must forward `symbol_path: c.symbol_path.clone()` instead of defaulting to empty.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content planned_chunk_has_symbol_path_field && cargo build -p mn-cli`
Expected: PASS + build OK.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/ingest/plan.rs crates/mn-cli/src/commands/ingest/run.rs
git commit -m "feat(mn-content): thread structured symbol_path from chunk to upload"
```

---

# Phase C — Code chunker engine + languages

## Task 10: `Language` enum + dispatch ✅ DONE

**Files:**
- Create: `crates/mn-content/src/code/language.rs`
- Modify: `crates/mn-content/src/code/mod.rs` (add `chunker_for`)

- [ ] **Step 1: Write the failing test**

`crates/mn-content/src/code/language.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extension_mapping() {
        assert_eq!(Language::for_extension("rs"), Language::Rust);
        assert_eq!(Language::for_extension("tsx"), Language::TypeScript);
        assert_eq!(Language::for_extension("mjs"), Language::JavaScript);
        assert_eq!(Language::for_extension("compact"), Language::Compact);
        assert_eq!(Language::for_extension("zzz"), Language::Other);
    }
    #[test]
    fn shebang_detects_bash() {
        assert_eq!(Language::for_shebang("#!/usr/bin/env bash\n..."), Some(Language::Bash));
        assert_eq!(Language::for_shebang("no shebang"), None);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content code::language`
Expected: FAIL.

- [ ] **Step 3: Implement**

`crates/mn-content/src/code/language.rs`:
```rust
//! Extension/shebang → Language, and the per-language dispatch key.

/// A source language the chunker recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust, TypeScript, JavaScript, Scheme, Bash,
    Go, Python, Solidity,
    Toml, Yaml, Html, Xml,
    Swift, Ruby, Kotlin, CSharp, Haskell, Java,
    Compact,
    /// Unknown extension → line-window fallback.
    Other,
}

impl Language {
    /// Map a lowercased extension (no dot) to a language.
    #[must_use]
    pub fn for_extension(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Self::Rust,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "scm" | "ss" | "sld" => Self::Scheme,
            "sh" | "bash" => Self::Bash,
            "go" => Self::Go,
            "py" | "pyi" => Self::Python,
            "sol" => Self::Solidity,
            "toml" => Self::Toml,
            "yaml" | "yml" => Self::Yaml,
            "html" | "htm" => Self::Html,
            "xml" | "csproj" | "nuspec" | "plist" => Self::Xml,
            "swift" => Self::Swift,
            "rb" => Self::Ruby,
            "kt" | "kts" => Self::Kotlin,
            "cs" => Self::CSharp,
            "hs" => Self::Haskell,
            "java" => Self::Java,
            "compact" => Self::Compact,
            _ => Self::Other,
        }
    }

    /// Detect language from a shebang line (EC-53). Returns `None` if absent.
    #[must_use]
    pub fn for_shebang(body: &str) -> Option<Self> {
        let first = body.lines().next()?;
        let first = first.strip_prefix("#!")?;
        if first.contains("bash") || first.contains("/sh") || first.ends_with("sh") {
            Some(Self::Bash)
        } else if first.contains("python") {
            Some(Self::Python)
        } else if first.contains("node") {
            Some(Self::JavaScript)
        } else {
            None
        }
    }
}
```

`crates/mn-content/src/code/mod.rs` — add dispatch:
```rust
pub mod language;
pub mod line_window;
// language modules added per task; each gated by its feature.

use crate::chunk::Chunker;
use language::Language;
use line_window::LineWindowChunker;

/// Return the chunker for a language. Languages whose grammar feature is not
/// compiled fall back to line-window (graceful degradation).
#[must_use]
pub fn chunker_for(lang: Language) -> Box<dyn Chunker> {
    match lang {
        #[cfg(feature = "dep:tree-sitter-rust")]
        Language::Rust => Box::new(rust::RustChunker),
        // ... one arm per language, each #[cfg(...)] gated; added in later tasks ...
        _ => Box::new(LineWindowChunker),
    }
}
```

> Note on `#[cfg]`: feature predicates use the **feature name**, not `dep:`. Use `#[cfg(feature = "core-grammars")]` for the core langs, `#[cfg(feature = "extended-grammars")]` for Go/Python/Solidity, etc. The `dep:` form is only valid inside `[features]` in Cargo.toml. Each language task below specifies its exact `cfg`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content code::language`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/code
git commit -m "feat(mn-content): Language enum + chunker_for dispatch"
```

---

## Task 11: `text-splitter` wrapper (`splitter.rs`) ✅ DONE (text-splitter 0.27 API confirmed: CodeSplitter::new returns Result, chunk_indices, &Tokenizer:ChunkSizer)

**Files:**
- Create: `crates/mn-content/src/code/splitter.rs`

- [ ] **Step 1: Write the failing test** (gated on a core grammar so a real language exists)

```rust
#[cfg(all(test, feature = "core-grammars"))]
mod tests {
    use super::*;
    use crate::chunk::ChunkerConfig;

    #[test]
    fn splits_rust_into_budgeted_ranges() {
        let src = "fn a() {}\nfn b() {}\nstruct S { x: u32 }\n";
        let lang = tree_sitter_rust::LANGUAGE.into();
        let cfg = ChunkerConfig { max_tokens: 16, ..ChunkerConfig::default() };
        let ranges = split_ranges(src, &lang, &cfg).unwrap();
        assert!(!ranges.is_empty());
        // ranges cover the source contiguously enough to reconstruct items
        assert!(ranges.iter().all(|r| r.end <= src.len() && r.start < r.end));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content --features core-grammars code::splitter`
Expected: FAIL — `split_ranges` missing.

- [ ] **Step 3: Implement**

`crates/mn-content/src/code/splitter.rs`:
```rust
//! Wrapper over text-splitter's CodeSplitter: given source + a tree-sitter
//! language + a token budget, return byte ranges of budgeted semantic chunks.

use std::ops::Range;
use text_splitter::{ChunkConfig, CodeSplitter};
use crate::chunk::{ChunkError, ChunkerConfig};

/// Split `src` into byte ranges, each ≤ `cfg.max_tokens` BPE tokens where the
/// grammar allows, falling on the largest semantic node that fits.
///
/// # Errors
/// Returns [`ChunkError::ParserFallback`] if the splitter cannot build (e.g.
/// grammar/runtime ABI mismatch) — caller then uses line-window.
pub fn split_ranges(
    src: &str,
    language: &tree_sitter::Language,
    cfg: &ChunkerConfig,
) -> Result<Vec<Range<usize>>, ChunkError> {
    // Budget by tokens using the project tokenizer. text-splitter's tokenizers
    // feature accepts a `tokenizers::Tokenizer`; reuse the one behind
    // crate::tokens (expose it via a getter — see note below).
    let tokenizer = crate::tokens::tokenizer();
    let splitter = CodeSplitter::new(
        language.clone(),
        ChunkConfig::new(cfg.max_tokens as usize).with_sizer(tokenizer),
    )
    .map_err(|e| ChunkError::ParserFallback(format!("code splitter init: {e}")))?;

    Ok(splitter
        .chunk_indices(src)
        .map(|(start, piece)| start..start + piece.len())
        .collect())
}
```

> **Tokenizer reuse:** Task 11 needs `crate::tokens::tokenizer() -> &'static tokenizers::Tokenizer`. The accurate-token-count work loaded a `tokenizers::Tokenizer` into a `OnceLock` in `tokens.rs`. Add a `pub(crate) fn tokenizer() -> &'static tokenizers::Tokenizer` returning that singleton. If `tokens.rs` stored only a wrapper, expose the inner `Tokenizer`. Verify `text-splitter`'s `with_sizer` accepts `&Tokenizer` (it implements `ChunkSizer` for `tokenizers::Tokenizer` under the `tokenizers` feature).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content --features core-grammars code::splitter`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/code/splitter.rs crates/mn-content/src/tokens.rs
git commit -m "feat(mn-content): text-splitter wrapper for token-budgeted code ranges"
```

---

## Task 12: Symbol-path extraction (`symbols.rs`) ✅ DONE (impl_item name via "type" field)

**Files:**
- Create: `crates/mn-content/src/code/symbols.rs`

- [ ] **Step 1: Write the failing test** (gated on core-grammars)

```rust
#[cfg(all(test, feature = "core-grammars"))]
mod tests {
    use super::*;
    #[test]
    fn rust_symbol_path_for_byte_range() {
        let src = "impl Foo {\n    fn bar(&self) {}\n}\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        // byte offset inside `fn bar`
        let off = src.find("fn bar").unwrap() + 1;
        let table = rust_kind_table();
        let path = symbol_path_at(&tree, src, off, table);
        assert_eq!(path.iter().map(|s| (s.kind.as_str(), s.name.as_str())).collect::<Vec<_>>(),
                   vec![("impl", "Foo"), ("fn", "bar")]);
    }
}
```

(`rust_kind_table` lives in `rust.rs` Task 13; for this task's test, inline a minimal table or move the test to Task 13. Keep `symbols.rs` generic.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content --features core-grammars code::symbols`
Expected: FAIL.

- [ ] **Step 3: Implement the generic walker**

`crates/mn-content/src/code/symbols.rs`:
```rust
//! Generic symbol-path extraction. Each language supplies a `KindTable`
//! mapping tree-sitter node kinds → (symbol kind label, name-field name).

use mn_core::types::SymbolSegment;

/// Maps a tree-sitter node-kind string to how it contributes to a symbol path.
pub struct KindEntry {
    /// tree-sitter node kind, e.g. "impl_item", "function_item".
    pub node_kind: &'static str,
    /// Symbol-path kind label, e.g. "impl", "fn".
    pub label: &'static str,
    /// The field name holding the identifier (e.g. "name"); None → use first
    /// `identifier`/`type_identifier` descendant.
    pub name_field: Option<&'static str>,
}

/// A language's full node-kind → symbol mapping.
pub type KindTable = &'static [KindEntry];

/// Build the symbol path for the node containing `byte_offset`: walk from root
/// down to the deepest node containing the offset, collecting segments for
/// nodes whose kind is in `table`.
#[must_use]
pub fn symbol_path_at(
    tree: &tree_sitter::Tree,
    src: &str,
    byte_offset: usize,
    table: KindTable,
) -> Vec<SymbolSegment> {
    let mut path = Vec::new();
    let mut node = tree.root_node();
    loop {
        if let Some(entry) = table.iter().find(|e| e.node_kind == node.kind()) {
            if let Some(name) = node_name(node, src, entry.name_field) {
                path.push(SymbolSegment { kind: entry.label.to_string(), name });
            }
        }
        match node.named_children(&mut node.walk())
            .find(|c| c.start_byte() <= byte_offset && byte_offset < c.end_byte())
        {
            Some(child) => node = child,
            None => break,
        }
    }
    path
}

fn node_name(node: tree_sitter::Node, src: &str, field: Option<&str>) -> Option<String> {
    let n = match field {
        Some(f) => node.child_by_field_name(f)?,
        None => {
            let mut w = node.walk();
            node.named_children(&mut w)
                .find(|c| c.kind().contains("identifier"))?
        }
    };
    src.get(n.start_byte()..n.end_byte()).map(str::to_string)
}
```

- [ ] **Step 4: Run to verify it passes** (after Task 13 provides `rust_kind_table`, or inline a temp table here)

Run: `cargo test -p mn-content --features core-grammars code::symbols`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/code/symbols.rs
git commit -m "feat(mn-content): generic tree-sitter symbol-path extractor"
```

---

## Task 13: Rust chunker (fully-worked language template) ✅ DONE (catastrophic heuristic = root.has_error() + full-descendant ERROR/MISSING byte sum >50%)

**This task is the template every other language follows.** It combines: a kind table, a `Chunker` impl that runs the parser, the splitter, symbol-path extraction, and parser-error fallback.

**Files:**
- Create: `crates/mn-content/src/code/rust.rs`
- Modify: `crates/mn-content/src/code/mod.rs` (add `#[cfg(feature = "core-grammars")] pub mod rust;` + dispatch arm)

- [ ] **Step 1: Write the failing test**

`crates/mn-content/src/code/rust.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn chunks_carry_symbol_path() {
        let src = "impl Foo {\n    fn bar(&self) { let x = 1; }\n}\n\nfn free() {}\n";
        let chunks = RustChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(!chunks.is_empty());
        let bar = chunks.iter().find(|c| c.content.contains("fn bar")).unwrap();
        let kinds: Vec<_> = bar.symbol_path.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"impl"));
        assert!(bar.symbol_path.iter().any(|s| s.name == "Foo"));
        assert!(!bar.fallback_used);
    }

    #[test]
    fn malformed_falls_back_to_line_window() {
        let src = "fn broken( { { { unterminated\n".repeat(40);
        let chunks = RustChunker.chunk(&src, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().any(|c| c.fallback_used));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content --features core-grammars code::rust`
Expected: FAIL.

- [ ] **Step 3: Implement**

`crates/mn-content/src/code/rust.rs`:
```rust
//! Rust chunker: tree-sitter-rust + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::line_window::LineWindowChunker;
use crate::code::symbols::{symbol_path_at, KindEntry, KindTable};

/// Node-kind → symbol mapping for Rust.
pub fn rust_kind_table() -> KindTable {
    &[
        KindEntry { node_kind: "mod_item",      label: "mod",    name_field: Some("name") },
        KindEntry { node_kind: "impl_item",     label: "impl",   name_field: Some("type") },
        KindEntry { node_kind: "trait_item",    label: "trait",  name_field: Some("name") },
        KindEntry { node_kind: "struct_item",   label: "struct", name_field: Some("name") },
        KindEntry { node_kind: "enum_item",     label: "enum",   name_field: Some("name") },
        KindEntry { node_kind: "function_item", label: "fn",     name_field: Some("name") },
    ]
}

/// Rust code chunker.
pub struct RustChunker;

impl Chunker for RustChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body, cfg, &tree_sitter_rust::LANGUAGE.into(), rust_kind_table(),
        )
    }
}
```

Add the **shared driver** `run_tree_sitter` to `crates/mn-content/src/code/mod.rs` (used by every language so the parse→split→symbols→fallback flow is written once):
```rust
use crate::chunk::{Chunk, ChunkError, ChunkerConfig};
use crate::code::symbols::{symbol_path_at, KindTable};

/// Shared driver: parse, detect catastrophic error, split into budgeted ranges,
/// attach symbol paths. Falls back to line-window when parsing is unusable.
pub(crate) fn run_tree_sitter(
    body: &str,
    cfg: &ChunkerConfig,
    language: &tree_sitter::Language,
    table: KindTable,
) -> Result<Vec<Chunk>, ChunkError> {
    use crate::code::line_window::LineWindowChunker;
    use crate::chunk::Chunker;

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
    // Catastrophic-error heuristic: root ERROR child spanning >50% of bytes.
    let root = tree.root_node();
    let err_bytes: usize = {
        let mut w = root.walk();
        root.children(&mut w)
            .filter(|c| c.is_error())
            .map(|c| c.end_byte() - c.start_byte())
            .sum()
    };
    if err_bytes * 2 > body.len() {
        return LineWindowChunker.chunk(body, cfg);
    }

    let ranges = match crate::code::splitter::split_ranges(body, language, cfg) {
        Ok(r) => r,
        Err(_) => return LineWindowChunker.chunk(body, cfg),
    };
    let mut chunks = Vec::with_capacity(ranges.len());
    for (i, r) in ranges.into_iter().enumerate() {
        let content = body[r.clone()].to_string();
        if content.trim().is_empty() { continue; }
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
```

Wire the dispatch arm in `mod.rs`:
```rust
#[cfg(feature = "core-grammars")] pub mod rust;
// in chunker_for:
#[cfg(feature = "core-grammars")]
Language::Rust => Box::new(rust::RustChunker),
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content --features core-grammars code::rust`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/code
git commit -m "feat(mn-content): Rust chunker + shared tree-sitter driver"
```

---

## Task 14: TypeScript chunker (`.ts` + `.tsx`) ✅ DONE

**Files:**
- Create: `crates/mn-content/src/code/ts.rs`
- Modify: `crates/mn-content/src/code/mod.rs` (module + dispatch)

- [ ] **Step 1: Write the failing test**

`crates/mn-content/src/code/ts.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};
    #[test]
    fn class_method_symbol_path() {
        let src = "class Widget {\n  render() { return 1; }\n}\nexport function f() {}\n";
        let chunks = TypeScriptChunker { tsx: false }.chunk(src, &ChunkerConfig::default()).unwrap();
        let m = chunks.iter().find(|c| c.content.contains("render")).unwrap();
        assert!(m.symbol_path.iter().any(|s| s.kind == "class" && s.name == "Widget"));
    }
    #[test]
    fn tsx_component_parses() {
        let src = "function App() { return <div>{x}</div>; }\n";
        let chunks = TypeScriptChunker { tsx: true }.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| !c.fallback_used));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content --features core-grammars code::ts`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! TypeScript/TSX chunker. tree-sitter-typescript ships two grammars:
//! `LANGUAGE_TYPESCRIPT` and `LANGUAGE_TSX`. We pick based on `tsx`.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

pub fn ts_kind_table() -> KindTable {
    &[
        KindEntry { node_kind: "internal_module",       label: "namespace", name_field: Some("name") },
        KindEntry { node_kind: "module",                label: "namespace", name_field: Some("name") },
        KindEntry { node_kind: "class_declaration",     label: "class",     name_field: Some("name") },
        KindEntry { node_kind: "interface_declaration", label: "interface", name_field: Some("name") },
        KindEntry { node_kind: "function_declaration",  label: "function",  name_field: Some("name") },
        KindEntry { node_kind: "method_definition",     label: "method",    name_field: Some("name") },
    ]
}

/// TS/TSX chunker. `tsx=true` selects the JSX-aware grammar.
pub struct TypeScriptChunker { pub tsx: bool }

impl Chunker for TypeScriptChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        let lang = if self.tsx {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        };
        crate::code::run_tree_sitter(body, cfg, &lang, ts_kind_table())
    }
}
```

Dispatch in `mod.rs` — TS chooses tsx by extension. Since `chunker_for` takes only `Language`, thread the extension: change `chunker_for` to also accept the original extension, OR have `Language::TypeScript` carry a `tsx: bool`. Simplest: add a sibling `chunker_for_ext(lang, ext)`; the planner already knows the extension. Implement:
```rust
#[cfg(feature = "core-grammars")] pub mod ts;
// add to chunker_for_ext:
#[cfg(feature = "core-grammars")]
Language::TypeScript => Box::new(ts::TypeScriptChunker { tsx: ext.eq_ignore_ascii_case("tsx") }),
```
Keep `chunker_for(lang)` as `chunker_for_ext(lang, "")` for callers that don't care.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content --features core-grammars code::ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/code
git commit -m "feat(mn-content): TypeScript/TSX chunker"
```

---

## Task 15: JavaScript chunker (`.js`/`.jsx`/`.mjs`/`.cjs`) ✅ DONE

**Files:**
- Create: `crates/mn-content/src/code/js.rs`
- Modify: `crates/mn-content/src/code/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};
    #[test]
    fn js_function_and_class_paths() {
        let src = "class A { m() {} }\nfunction g() {}\n";
        let chunks = JavaScriptChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().any(|c| c.symbol_path.iter().any(|s| s.name == "A")));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mn-content --features core-grammars code::js`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! JavaScript/JSX chunker. tree-sitter-javascript handles JSX in the one grammar.
use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

pub fn js_kind_table() -> KindTable {
    &[
        KindEntry { node_kind: "class_declaration",    label: "class",    name_field: Some("name") },
        KindEntry { node_kind: "function_declaration", label: "function", name_field: Some("name") },
        KindEntry { node_kind: "method_definition",    label: "method",   name_field: Some("name") },
    ]
}

pub struct JavaScriptChunker;
impl Chunker for JavaScriptChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(body, cfg, &tree_sitter_javascript::LANGUAGE.into(), js_kind_table())
    }
}
```

Dispatch:
```rust
#[cfg(feature = "core-grammars")] pub mod js;
#[cfg(feature = "core-grammars")]
Language::JavaScript => Box::new(js::JavaScriptChunker),
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content --features core-grammars code::js`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/src/code
git commit -m "feat(mn-content): JavaScript/JSX chunker"
```

---

## Task 16: Scheme chunker (vendored grammar) ✅ DONE (FFI via LanguageFn::from_raw; build.rs uses CARGO_FEATURE_SCHEME env var; workspace unsafe_code forbid→deny — FLAG TO USER; scheme_kind_table=&[] as grammar has no clean define node)

**Files:**
- Create: `crates/mn-content/vendor/tree-sitter-scheme/` (vendored `src/parser.c`, `src/tree_sitter/*.h`, `grammar.js`, LICENSE)
- Create: `crates/mn-content/build.rs`
- Create: `crates/mn-content/src/code/scheme.rs`
- Modify: `crates/mn-content/src/code/mod.rs`

- [ ] **Step 1: Vendor the grammar**

Clone a maintained Scheme grammar (e.g. `6cdh/tree-sitter-scheme`) at a fixed commit; copy `src/parser.c`, any `src/scanner.c`, and `src/tree_sitter/` headers into `crates/mn-content/vendor/tree-sitter-scheme/src/`. Record the source commit in a `VENDOR.md` next to it. **Do not** run `npx`/`npm` — copy files with `cp`/`git`.

- [ ] **Step 2: Write the failing test**

`crates/mn-content/src/code/scheme.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};
    #[test]
    fn scheme_defines_chunk() {
        let src = "(define (square x) (* x x))\n(define y 10)\n";
        let chunks = SchemeChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(!chunks.is_empty());
    }
}
```

- [ ] **Step 3: build.rs + FFI + chunker**

`crates/mn-content/build.rs`:
```rust
fn main() {
    #[cfg(feature = "scheme")]
    {
        let dir = std::path::Path::new("vendor/tree-sitter-scheme/src");
        let mut build = cc::Build::new();
        build.include(dir).file(dir.join("parser.c"));
        let scanner = dir.join("scanner.c");
        if scanner.exists() { build.file(scanner); }
        build.warnings(false).compile("tree_sitter_scheme");
        println!("cargo:rerun-if-changed=vendor/tree-sitter-scheme/src");
    }
}
```

`crates/mn-content/src/code/scheme.rs`:
```rust
//! Scheme chunker over a vendored tree-sitter grammar (Compact compiler is
//! written in Scheme).
use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

extern "C" { fn tree_sitter_scheme() -> tree_sitter::Language; }

fn language() -> tree_sitter::Language { unsafe { tree_sitter_scheme() } }

pub fn scheme_kind_table() -> KindTable {
    // Scheme is list-structured; treat top-level (define ...) forms as symbols.
    &[ KindEntry { node_kind: "definition", label: "define", name_field: None } ]
}

pub struct SchemeChunker;
impl Chunker for SchemeChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(body, cfg, &language(), scheme_kind_table())
    }
}
```

> Adjust `node_kind` to the actual grammar's define-form kind (inspect with `tree-sitter parse` or read `grammar.js`). If the grammar has no distinct define node, `scheme_kind_table()` may return `&[]` — chunks still split semantically, symbol_path just stays empty.

Dispatch:
```rust
#[cfg(feature = "scheme")] pub mod scheme;
#[cfg(feature = "scheme")]
Language::Scheme => Box::new(scheme::SchemeChunker),
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mn-content --features scheme code::scheme`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-content/vendor crates/mn-content/build.rs crates/mn-content/src/code
git commit -m "feat(mn-content): vendored Scheme chunker (compact compiler language)"
```

---

## Task 17: Bash chunker ✅ DONE

**Files:**
- Create: `crates/mn-content/src/code/bash.rs`; Modify `mod.rs`.

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};
    #[test]
    fn bash_function_path() {
        let src = "greet() {\n  echo hi\n}\n";
        let chunks = BashChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().any(|c| c.symbol_path.iter().any(|s| s.name == "greet")));
    }
}
```

- [ ] **Step 2: Run** `cargo test -p mn-content --features core-grammars code::bash` → FAIL.

- [ ] **Step 3: Implement**

```rust
//! Bash/shell chunker.
use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

pub fn bash_kind_table() -> KindTable {
    &[ KindEntry { node_kind: "function_definition", label: "fn", name_field: Some("name") } ]
}
pub struct BashChunker;
impl Chunker for BashChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(body, cfg, &tree_sitter_bash::LANGUAGE.into(), bash_kind_table())
    }
}
```
Dispatch: `#[cfg(feature = "core-grammars")]` `pub mod bash;` + arm `Language::Bash => Box::new(bash::BashChunker)`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit** `feat(mn-content): Bash chunker`.

---

## Task 18: Extended + markup + all-grammars languages (template + data table) ✅ DONE (18 langs; symbols.rs now emits kind-only segments for nameless markup nodes; kotlin=tree_sitter_kotlin_ng, toml=tree_sitter_toml_ng, xml=LANGUAGE_XML; none dropped)

Each remaining language follows the **exact Task-13 template**: a module `code/<lang>.rs` with a kind table + a `Chunker` impl calling `run_tree_sitter`, a `#[cfg]`-gated module + dispatch arm, and a fixture test asserting one symbol path. Below is one fully-worked example (Go), then the precise per-language data table — node kinds verified against each grammar's `node-types.json`; adjust if a grammar version differs.

**Worked example — Go** (`crates/mn-content/src/code/go.rs`, feature `extended-grammars`):

- [ ] **Step 1: Failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};
    #[test]
    fn go_func_and_type_paths() {
        let src = "package p\nfunc Add(a, b int) int { return a+b }\ntype T struct{ X int }\n";
        let chunks = GoChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(chunks.iter().any(|c| c.symbol_path.iter().any(|s| s.kind == "func" && s.name == "Add")));
    }
}
```
- [ ] **Step 2: Run** `cargo test -p mn-content --features extended-grammars code::go` → FAIL.
- [ ] **Step 3: Implement**
```rust
//! Go chunker.
use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};
pub fn go_kind_table() -> KindTable {
    &[
        KindEntry { node_kind: "function_declaration", label: "func",   name_field: Some("name") },
        KindEntry { node_kind: "method_declaration",   label: "method", name_field: Some("name") },
        KindEntry { node_kind: "type_declaration",     label: "type",   name_field: None },
    ]
}
pub struct GoChunker;
impl Chunker for GoChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(body, cfg, &tree_sitter_go::LANGUAGE.into(), go_kind_table())
    }
}
```
Dispatch: `#[cfg(feature = "extended-grammars")] pub mod go;` + `Language::Go => Box::new(go::GoChunker)`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(mn-content): Go chunker`.

**Now repeat the five steps for each language below.** For each: create `code/<module>.rs` using the table's kind entries, gate by the listed feature, add the dispatch arm, write a fixture test asserting the first symbol path entry, run `cargo test -p mn-content --features <feature> code::<module>`, commit.

| Language | feature | module | grammar `LANGUAGE` | kind table entries (`node_kind` → `label`, name_field) |
|---|---|---|---|---|
| Python | extended-grammars | `python` | `tree_sitter_python::LANGUAGE` | `class_definition`→`class`(name); `function_definition`→`def`(name) |
| Solidity | extended-grammars | `solidity` | `tree_sitter_solidity::LANGUAGE` | `contract_declaration`→`contract`(name); `function_definition`→`function`(name); `modifier_definition`→`modifier`(name); `struct_declaration`→`struct`(name) |
| TOML | markup-grammars | `toml` | `tree_sitter_toml::LANGUAGE` | `table`→`key`(None); `table_array_element`→`key`(None) — structural |
| YAML | markup-grammars | `yaml` | `tree_sitter_yaml::LANGUAGE` | `block_mapping_pair`→`key`(`key`) — structural |
| HTML | markup-grammars | `html` | `tree_sitter_html::LANGUAGE` | `element`→`element`(None) — name via first `tag_name` descendant |
| XML | markup-grammars | `xml` | `tree_sitter_xml::LANGUAGE_XML` | `element`→`element`(None) — name via `Name` token |
| Swift | all-grammars | `swift` | `tree_sitter_swift::LANGUAGE` | `class_declaration`→`class`(name); `function_declaration`→`func`(name); `protocol_declaration`→`protocol`(name) |
| Ruby | all-grammars | `ruby` | `tree_sitter_ruby::LANGUAGE` | `class`→`class`(name); `module`→`module`(name); `method`→`def`(name) |
| Kotlin | all-grammars | `kotlin` | `tree_sitter_kotlin::LANGUAGE` | `class_declaration`→`class`(None); `function_declaration`→`fun`(None); `object_declaration`→`object`(None) |
| C# | all-grammars | `csharp` | `tree_sitter_c_sharp::LANGUAGE` | `class_declaration`→`class`(name); `interface_declaration`→`interface`(name); `method_declaration`→`method`(name); `namespace_declaration`→`namespace`(name) |
| Haskell | all-grammars | `haskell` | `tree_sitter_haskell::LANGUAGE` | `function`→`fn`(None); `data_type`→`data`(None); `class`→`class`(None) |
| Java | all-grammars | `java` | `tree_sitter_java::LANGUAGE` | `class_declaration`→`class`(name); `interface_declaration`→`interface`(name); `method_declaration`→`method`(name) |

> Verify each `node_kind` against the installed grammar version: `cargo doc -p tree-sitter-<lang>` won't list node kinds; instead inspect the grammar's `node-types.json` (vendored in the crate) or write the fixture test first and adjust kinds until the symbol path matches. For structural (markup) languages, `name_field: None` plus the symbols walker's "first identifier-ish descendant" rule covers most cases; for YAML keys, use `name_field: Some("key")`.

Markup languages emit structural symbol paths (`kind:"key"`/`"element"`); their tests assert the path's `kind`, not code-symbol semantics.

After all twelve: `cargo test -p mn-content --features all-grammars code::` should pass for every language module.

---

# Phase D — File filtering + package detection

## Task 19: `ignore`-based file filter with precedence ladder

**Files:**
- Create: `crates/mn-content/src/ingest/filter.rs`
- Modify: `crates/mn-content/src/ingest/mod.rs` (add `pub mod filter;`)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precedence_ladder() {
        let f = FileFilter::new(FilterOptions {
            includes: vec!["*.rs".into()],
            excludes: vec!["generated_*.rs".into()],
            respect_gitignore: true,
            default_ignore_list: true,
        });
        assert!(f.allows("src/lib.rs"));
        assert!(!f.allows("src/generated_x.rs"));     // exclude beats include
        assert!(!f.allows("src/main.ts"));            // not in whitelist
        assert!(!f.allows("node_modules/pkg/index.rs")); // default skip
        assert!(!f.allows(".git/config"));            // always
    }
    #[test]
    fn disable_default_list_allows_node_modules() {
        let f = FileFilter::new(FilterOptions {
            includes: vec![], excludes: vec![],
            respect_gitignore: false, default_ignore_list: false,
        });
        assert!(f.allows("node_modules/pkg/x.rs"));
        assert!(!f.allows(".git/config")); // .git still excluded
    }
}
```

- [ ] **Step 2: Run** `cargo test -p mn-content ingest::filter` → FAIL.

- [ ] **Step 3: Implement** using `ignore::overrides::OverrideBuilder` + `ignore::gitignore` for the layered decision, with the explicit precedence from the spec. Default-skip globs: `node_modules/`, `target/`, `vendor/`, `dist/`, plus `*.min.js`, `*.bundle.js`, `*_pb.ts`. `.git/` always excluded. Implement `FileFilter::allows(rel_path) -> bool` applying the 6-step evaluation order from the spec (§Filter precedence). For walking, expose `FileFilter::walk(root) -> impl Iterator<Item = PathBuf>` built on `ignore::WalkBuilder` with `.git_ignore(opts.respect_gitignore)`, `.hidden(false)`, custom overrides, and a manual `.git/`/default-skip filter for the `default_ignore_list` toggle.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit** `feat(mn-content): ignore-based file filter with precedence ladder`.

---

## Task 20: Wire the filter into walk + manifest generation

**Files:**
- Modify: `crates/mn-content/src/manifest/generate.rs` (use `FileFilter::walk` for the generated file list)
- Modify: `crates/mn-content/src/ingest/walker.rs` (path-mode walk uses `FileFilter`; manifest-listed entries stay authoritative)

- [ ] **Step 1: Write the failing test**

Add a test that `manifest generate` over a temp tree with a `.gitignore` and a `node_modules/` dir omits ignored files and includes a `.rs` + `.md` sibling.

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** — replace the `walkdir` enumeration in `generate.rs` with `FileFilter::walk`. In `walker.rs`, when operating in directory mode (no explicit manifest file list), enumerate via `FileFilter::walk`; keep the existing manifest-driven path untouched.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit** `feat(mn-content): gitignore-aware file lists in walk + manifest generate`.

---

## Task 21: Package detection (`package.rs`)

**Files:**
- Create: `crates/mn-content/src/package.rs`
- Modify: `crates/mn-content/src/lib.rs` (`pub mod package;`)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn rust_package_from_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"midnight-foo\"\n").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let f = dir.path().join("src/lib.rs");
        fs::write(&f, "fn x() {}").unwrap();
        let pkg = detect(&f, dir.path()).unwrap();
        assert_eq!(pkg.kind, "rust");
        assert_eq!(pkg.name, "midnight-foo");
    }
    #[test]
    fn workspace_root_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers=[\"a\"]\n").unwrap();
        let f = dir.path().join("a/src/lib.rs");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(dir.path().join("a/Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
        fs::write(&f, "fn x(){}").unwrap();
        let pkg = detect(&f, dir.path()).unwrap();
        assert_eq!(pkg.name, "a"); // nearest [package], not the workspace root
    }
    #[test]
    fn npm_package_from_package_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"@scope/web"}"#).unwrap();
        let f = dir.path().join("src/index.ts");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, "export const x=1;").unwrap();
        let pkg = detect(&f, dir.path()).unwrap();
        assert_eq!(pkg.kind, "npm");
        assert_eq!(pkg.name, "@scope/web");
    }
}
```

- [ ] **Step 2: Run** `cargo test -p mn-content package::` → FAIL.

- [ ] **Step 3: Implement**

```rust
//! Package membership detection: Rust (Cargo.toml [package]) and TS/JS
//! (package.json .name). Everything else → None (FR-006).

use std::path::{Path, PathBuf};

/// Detected package membership for a code file.
pub struct DetectedPackage {
    pub kind: String,           // "rust" | "npm"
    pub name: String,
    pub manifest_path: PathBuf, // relative to `root`
}

/// Walk up from `file` to `root` looking for the nearest manifest.
/// Cargo.toml with a `[package]` table → rust; package.json with `.name` → npm.
/// Workspace-only Cargo.toml (`[workspace]`, no `[package]`) is skipped.
#[must_use]
pub fn detect(file: &Path, root: &Path) -> Option<DetectedPackage> {
    let mut dir = file.parent();
    while let Some(d) = dir {
        let cargo = d.join("Cargo.toml");
        if cargo.is_file() {
            if let Ok(txt) = std::fs::read_to_string(&cargo) {
                if let Ok(v) = txt.parse::<toml::Value>() {
                    if let Some(name) = v.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
                        return Some(DetectedPackage {
                            kind: "rust".into(), name: name.into(),
                            manifest_path: rel(&cargo, root),
                        });
                    }
                    // [workspace]-only → keep walking up
                }
            }
        }
        let pkg = d.join("package.json");
        if pkg.is_file() {
            if let Ok(txt) = std::fs::read_to_string(&pkg) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                        return Some(DetectedPackage {
                            kind: "npm".into(), name: name.into(),
                            manifest_path: rel(&pkg, root),
                        });
                    }
                }
            }
        }
        if d == root { break; }
        dir = d.parent();
    }
    None
}

fn rel(p: &Path, root: &Path) -> PathBuf {
    p.strip_prefix(root).unwrap_or(p).to_path_buf()
}
```

Add `toml` to `mn-content`'s deps if absent (workspace already has `toml`).

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit** `feat(mn-content): Rust/npm package detection`.

---

## Task 22: Persist package membership

**Files:**
- Modify: `crates/mn-content/src/ingest/plan.rs` (`PlannedDocument` already has no package field — add one)
- Modify: `crates/mn-cli/src/commands/ingest/run.rs` (include package in document upload)
- Modify: `crates/mn-server/src/routes/admin_ingest.rs` (accept package, upsert into `package`, set `document.package_id`)

- [ ] **Step 1: Write the failing test**

Server-side integration test: upload a code document with a `package` payload; assert a `package` row exists and `document.package_id` points to it.

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**
- `PlannedDocument`: add `pub package: Option<mn_core::types::PackageRef>` (define `PackageRef { kind, name, manifest_path }` in mn-core, mirroring `DetectedPackage`). Populate it in `add_walked_document` by calling `package::detect` for `DocumentKind::Code` files.
- Upload struct in `run.rs`: forward `package`.
- `admin_ingest.rs`: on document insert, if `package` present, `INSERT ... ON CONFLICT (source_version_id, kind, name) DO UPDATE` into `package` (reuse the unique key), then set `document.package_id`. Add `mn_store::entities::package::upsert`.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit** `feat: persist Rust/npm package membership on code documents`.

---

# Phase E — CLI flags + dispatch swap

## Task 23: New `ingest run` flags

**Files:**
- Modify: `crates/mn-cli/src/commands/ingest/run.rs` (`Args` + plumb into `ChunkerConfig` and `FilterOptions`)

- [ ] **Step 1: Write the failing test**

Clap parse test: `--code-chunk-tokens 256 --include '*.rs' --exclude 'gen_*' --no-respect-gitignore --disable-default-ignore-list --max-file-size 1048576` parses into the expected `Args` fields.

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** — add to `Args`:
```rust
    /// Semantic code-chunk budget in tokens.
    #[arg(long, default_value_t = 400)]
    pub code_chunk_tokens: u32,
    /// Line-window fallback size (lines).
    #[arg(long, default_value_t = 60)]
    pub code_chunk_lines: u32,
    /// Line-window fallback overlap (lines).
    #[arg(long, default_value_t = 20)]
    pub code_chunk_overlap: u32,
    /// Whitelist glob (repeatable).
    #[arg(long)]
    pub include: Vec<String>,
    /// Exclude glob (repeatable), additive over defaults + gitignore.
    #[arg(long)]
    pub exclude: Vec<String>,
    /// Disable .gitignore/.ignore filtering.
    #[arg(long)]
    pub no_respect_gitignore: bool,
    /// Disable the built-in default skip list (node_modules, target, …).
    #[arg(long)]
    pub disable_default_ignore_list: bool,
    /// Skip files larger than this many bytes.
    #[arg(long, default_value_t = 10 * 1024 * 1024)]
    pub max_file_size: u64,
```
Map them into `ChunkerConfig` (`with_chunker_config`) and `FilterOptions` where the walk is constructed.

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit** `feat(mn-cli): code-chunk + ignore flags on ingest run`.

---

## Task 24: Swap the planner dispatch to the code chunkers

**Files:**
- Modify: `crates/mn-content/src/ingest/plan.rs` (the `match walked.kind` block ~line 262; `resolve.rs::kind_for` to also surface the extension)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn code_documents_get_symbol_paths() {
    // Build a WalkContext for a `.rs` file with `impl Foo { fn bar() {} }`
    // Run the builder; assert the resulting PlannedDocument's chunks include
    // one whose symbol_path contains {kind:"impl", name:"Foo"} and none used
    // the markdown chunker (heading_path empty, symbol_path non-empty).
}
```

- [ ] **Step 2: Run** `cargo test -p mn-content --features core-grammars code_documents_get_symbol_paths` → FAIL.

- [ ] **Step 3: Implement** — replace the dispatch:
```rust
let chunks: Vec<crate::chunk::Chunk> = match walked.kind {
    DocumentKind::Markdown => {
        use crate::chunk::Chunker;
        crate::markdown::MarkdownChunker
            .chunk(&walked.split.body, &self.chunker_config)
            .unwrap_or_default()
    }
    DocumentKind::Code => {
        let ext = walked.path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = crate::code::language::Language::for_extension(ext);
        crate::code::chunker_for_ext(lang, ext)
            .chunk(&walked.split.body, &self.chunker_config)
            .unwrap_or_default()
    }
    DocumentKind::Plaintext => {
        use crate::chunk::Chunker;
        crate::code::line_window::LineWindowChunker
            .chunk(&walked.split.body, &self.chunker_config)
            .unwrap_or_default()
    }
};
```
Then map `Chunk` → `PlannedChunk` forwarding both `heading_path` and `symbol_path`. The `unwrap_or_default()` is safe: chunkers recover internally; `ChunkError` only surfaces under a future strict mode (wire `--strict` to propagate if desired — out of scope here, leave a `// TODO(strict)`-free design by mapping err→warning at the caller in Task 25's run wiring).

> Remove the `// Phase 9a only ships the Markdown chunker…` comment.

- [ ] **Step 4: Run** → PASS. Also `cargo test -p mn-content` (no features) must pass — Code falls to line-window when grammars are absent.

- [ ] **Step 5: Commit** `feat(mn-content): dispatch code documents to language chunkers`.

---

# Phase F — Integration + CI

## Task 25: End-to-end smoke (mixed tree, testcontainers)

**Files:**
- Create: `crates/mn-server/tests/code_ingest_e2e.rs`
- Create: `crates/mn-content/tests/corpus/` (≤200 KB, ~10 mixed real-world files: a few `.rs`, `.ts`, `.tsx`, `.md`, one `Cargo.toml`, one `package.json`, one malformed file)

- [ ] **Step 1: Write the failing test**

Boot the server against testcontainers Postgres (mirror `f_bug_e2e.rs` from PR #58). Ingest `crates/mn-content/tests/corpus/` via the CLI ingest path. Assert: every `.rs` chunk has non-empty `symbol_path`; the `.md` file produced `heading_path` chunks; `package` rows exist for the Rust + npm manifests; the malformed file still produced chunks with `fallback_used=true`.

- [ ] **Step 2: Run** `cargo test -p mn-server --features integration code_ingest_e2e` → FAIL.

- [ ] **Step 3: Assemble the corpus + make the test pass.** Add the fixture files; fix any wiring gaps the test exposes.

- [ ] **Step 4: Run** → PASS. Then full `cargo test --workspace --features integration,all-grammars`.

- [ ] **Step 5: Commit** `test(mn-server): end-to-end mixed-tree code ingest smoke`.

---

## Task 26: CI feature matrix

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add an all-grammars test leg**

Add a step (or matrix dimension) running `cargo test -p mn-content --features all-grammars` and `cargo clippy -p mn-content --features all-grammars --all-targets -- -D warnings`, in addition to the existing default-feature run. Note in a comment that the all-grammars build is slower (all parse tables compile).

- [ ] **Step 2: Verify locally**

Run: `cargo test -p mn-content --features all-grammars && cargo clippy --workspace --all-targets --features all-grammars -- -D warnings`
Expected: PASS / clean.

- [ ] **Step 3: Commit** `ci: add all-grammars test + clippy leg`.

---

# Self-review checklist (completed during planning)

- **Spec coverage:** scope (Tasks 23–24 dispatch, 1–4 schema), architecture/trait (6), text-splitter (11), tokens budget incl. markdown (8), structured symbol_path + migration (1–4), filtering ladder + flags (19, 23), package detection rust/npm (21–22), feature gating + graceful degradation (5, 10, 24), per-language set incl. vendored Scheme (13–18), error/fallback ladder (7, 13 driver), testing + CI (25–26), Compact slot left open (dispatch falls to line-window; documented). All spec sections map to a task.
- **Placeholder scan:** no TBD/TODO; per-language long tail expressed as worked template + concrete node-kind table (real content, not placeholder).
- **Type consistency:** `SymbolSegment` (mn-core) used identically in `Chunk`, `PlannedChunk`, `ChunkUpload`, `NewChunk`, migration; `Chunker::chunk(&self, &str, &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError>` consistent across all impls; `run_tree_sitter`/`split_ranges`/`symbol_path_at` signatures match call sites; `chunker_for_ext(lang, ext)` introduced in Task 14 and used in Task 24.

# Known risks / for-the-engineer

- **Grammar ABI drift** is the most likely build failure. Pin `tree-sitter` first; match grammars to it. If one grammar can't match, drop it from its feature and open a follow-up rather than downgrading the runtime for everyone.
- **`text-splitter` API**: `CodeSplitter::new` + `ChunkConfig::with_sizer` and `chunk_indices` names are from the 0.30 line; if `cargo add` pulls a different major, adjust to that version's API (the wrapper in Task 11 is the only place that touches it).
- **tree-sitter node kinds vary by grammar version** — the Task-18 table is verified against current versions; if a fixture test fails on a symbol kind, inspect the grammar's `node-types.json` and adjust the kind table (localized to one module).

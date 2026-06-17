# Corpus Fixture

This directory contains a mixed-tree test corpus for the code-ingest end-to-end
smoke test (`crates/midnight-manual-server/tests/code_ingest_e2e.rs`).

## Purpose

The fixture exercises the full ingest pipeline across multiple file types:

- **Rust** source files (`src/lib.rs`, `src/util.rs`) — tree-sitter chunker
  with `symbol_path` extraction.
- **TypeScript** source files (`web/app.ts`, `web/component.tsx`) — tree-sitter
  chunker with TSX support.
- **Markdown** files (`README.md`, `docs/guide.md`) — heading-aware chunker
  that populates `heading_path`.
- **Malformed Rust** (`src/broken.rs`) — designed to trigger the catastrophic
  error fallback (line-window with empty `symbol_path`).
- **Plain text** (`notes.txt`) — line-window chunker.

## Package Detection

The `Cargo.toml` at the corpus root enables Rust package detection for the
`.rs` files. The `web/package.json` enables npm package detection for the
`.ts`/`.tsx` files.

## Structure

```
corpus/
├── Cargo.toml           # Rust package manifest (detection only)
├── README.md            # This file
├── docs/
│   └── guide.md         # Markdown with nested headings
├── notes.txt            # Plain text file
├── src/
│   ├── broken.rs        # Malformed Rust (fallback test)
│   ├── lib.rs           # Well-formed Rust: struct + impl + fns
│   └── util.rs          # Well-formed Rust: enum + free fns
└── web/
    ├── app.ts           # TypeScript: class + functions
    ├── component.tsx    # TSX: React components
    └── package.json     # npm package manifest (detection only)
```

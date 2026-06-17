# Vendored tree-sitter-scheme grammar

- **Source repo**: https://github.com/6cdh/tree-sitter-scheme
- **Pinned commit**: `c6cb7c7d7a04b3f5d999c28e2e9c0c31b2d50ece` (committed 2026-03-17)
- **Vendored on**: 2026-05-28
- **Why**: The Compact compiler is written in Scheme. We chunk Scheme source
  when reading the compiler's own code (distinct from Compact contracts).

## Files copied (verbatim, no regeneration)

The upstream repo commits a pre-generated `src/parser.c`, so we copy it directly
with `cp` — we never run the tree-sitter CLI / `tree-sitter generate`.

- `src/parser.c` — pre-generated parser (exposes the C symbol `tree_sitter_scheme`)
- `src/tree_sitter/parser.h`, `src/tree_sitter/alloc.h`, `src/tree_sitter/array.h` — parser headers
- `src/node-types.json` — node-kind reference (used to design the kind table)
- `grammar.js` — grammar definition (reference only; not compiled)
- `LICENSE` — upstream license

There is **no** `src/scanner.c` in this grammar (no external scanner), so none
was copied; `build.rs` compiles only `parser.c`.

## Build

`crates/mnm-content/build.rs` compiles `src/parser.c` with the `cc` crate when the
`scheme` cargo feature is enabled (detected via the `CARGO_FEATURE_SCHEME`
environment variable, since build scripts do not receive `#[cfg(feature = ...)]`).

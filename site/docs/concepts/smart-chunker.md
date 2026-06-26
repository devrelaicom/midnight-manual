---
title: The smart chunker
sidebar_label: Smart chunker
description: How Midnight Manual splits Markdown and source code into semantically-bounded, attributable chunks — and why it matters for retrieval quality.
---

# The smart chunker

Retrieval quality is only as good as your chunks. A system that blindly slices text every N characters produces chunks that start mid-sentence, split functions across boundaries, and lose track of which heading a passage belongs to. Midnight Manual doesn't do that.

The chunker understands the structure of what it is reading — Markdown heading hierarchy, programming language syntax, package membership — and splits on real boundaries. The payoff: every search hit lands on a *named thing*, not an arbitrary window.

## Markdown: heading-aware chunks

Markdown files are parsed with `pulldown-cmark` and split along the **heading hierarchy**. A level-2 section stays together. A long level-3 subsection is split at a natural boundary rather than mid-sentence.

Every chunk carries its **`heading_path`** — the full chain of ancestor headings from the document root to the chunk's location. A hit at `/docs/intro.md` that came from inside `## Installation > ### macOS` carries exactly that path, so your AI assistant knows precisely where in the document outline the passage lives.

## Code: semantic, symbol-aware chunks

Source files are parsed with [`tree-sitter`](https://tree-sitter.github.io/) and split on real syntactic boundaries — **functions, classes, `impl` blocks, modules** — never mid-expression.

Every code chunk records a structured **`symbol_path`**, such as `impl Widget › fn render`. Search hits on code land on a named symbol, not an arbitrary window of lines. This makes a significant difference in retrieval: asking "how does Widget render?" finds the `render` method, not a fragment that happens to contain the word "render" in a comment on line 147.

### Supported languages

| | | |
|---|---|---|
| **Compact** `.compact` | Rust `.rs` | TypeScript `.ts` `.tsx` |
| JavaScript `.js` `.jsx` `.mjs` `.cjs` | Python `.py` `.pyi` | Go `.go` |
| Solidity `.sol` | Java `.java` | C# `.cs` |
| Kotlin `.kt` `.kts` | Swift `.swift` | Ruby `.rb` |
| Haskell `.hs` | Bash `.sh` `.bash` | Scheme `.scm` `.ss` `.sld` |
| TOML `.toml` | YAML `.yaml` `.yml` | HTML / XML |

Grammars are organized into tiers (`core-grammars` → `markup-grammars` → `extended-grammars` → `all-grammars`) as Cargo features, so a lean build stays small.

### Compact is a first-class citizen

Midnight's smart-contract language gets full symbol awareness — circuits, ledger declarations, witnesses, and contracts all become their own semantically-bounded, attributable chunks. This is backed by the [`compactp`](https://crates.io/crates/compactp_parser) parser (a default-on feature) rather than tree-sitter, because Compact's grammar predates general tree-sitter support.

### Graceful degradation for unknown languages

When a grammar is absent or a language is unrecognized, the chunker falls back to a **token-budgeted, non-overlapping line-window chunker**: it grows line-by-line to approximately 90% of the token budget, then starts a new window. The file is still ingestible and searchable — it just won't have symbol paths. An absent grammar never aborts an ingest run.

## The details that matter

### Token-budgeted chunks

Chunks target a real **token budget** (default: 1024 tokens) so they fit the embedder comfortably. Token counts are measured with a real subword tokenizer — a vendored BGE tokenizer — not a character-count heuristic. This prevents over-long chunks that get silently truncated by the embedding API and produces embeddings that faithfully represent the chunk's content.

### `.gitignore`-aware file discovery

File lists are built with the [`ignore`](https://docs.rs/ignore) crate and follow a clear precedence ladder:

1. `.git/` is always excluded.
2. Built-in skips: `node_modules`, `target`, `vendor`, `dist`, `*.min.js`, and similar.
3. `.gitignore` / `.ignore` rules in the repository.
4. Your `--exclude` globs (passed at ingest time).
5. Your `--include` whitelist (overrides exclusions for matching files).

This means a standard Midnight project ingests cleanly without any manifest configuration: the chunker already knows to skip compiled output, lock files, and generated code.

### Package membership

Walking up from each file, the chunker attaches **package membership** — the name of the nearest Rust crate (`Cargo.toml` `[package]`, workspace roots skipped) or npm package (`package.json` `.name`). Search results can be filtered and attributed by package, so "find the `deployContract` function in `@midnight-ntwrk/midnight-js-contracts`" actually works.

### Never fails the run for one bad file

A catastrophically malformed source file falls back to line-window chunking and is flagged in the ingest report, rather than aborting the entire run. Chunks that fail to embed (for any reason) land in an `embed_failed` state and are simply skipped by readers — so navigation has clean gaps, never broken links.

## Related pages

- [Models](./models.md) — the embedding models that turn chunks into vectors.
- [Hybrid retrieval & RRF](./hybrid-retrieval.md) — how chunks are scored and ranked at query time.

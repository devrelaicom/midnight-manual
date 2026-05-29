# Code chunkers — design

**Date:** 2026-05-28
**Status:** draft
**Touches:** `mn-content` (new `code/` module, shared `Chunker` trait, markdown
chunker refactor, `ignore`-based file-list filtering, package detection),
`mn-cli` (new ingest flags on `ingest run`), `mn-store` (one migration:
`chunk.symbol_path` → `jsonb`), workspace `Cargo.toml` (new deps + feature
flags). Vendored grammar under `crates/mn-content/vendor/`.

This builds **Phase 6 code chunking** — currently unbuilt. `mn-content`'s
`lib.rs` states "the tree-sitter code chunkers and Compact module scanner land
in Phase 6"; the `tree-sitter-*` deps are declared but unused, and
`plan.rs:262` routes `DocumentKind::Code` through the markdown fallback
windower. This spec replaces that stopgap with real per-language semantic
chunking.

Scope is **all in-scope languages except Compact**. Compact stays on the
line-window fallback until [`compactp`](https://github.com/devrelaicom/compactp)
is published to crates.io, at which point it gets its own chunker (a single new
file + dispatch arm — see "Compact slot" below).

## Problem

Every `.rs` / `.ts` / `.tsx` / `.js` / `.jsx` file ingested today is
line-windowed with no `symbol_path`, no package membership, and no semantic
boundaries — it's text-blobbed. Code retrieval quality suffers: a search hit
lands on an arbitrary 60-line window rather than a function or type, and the
result can't tell you which `impl` / `class` / package it came from.

## Goals

- Semantic chunking for code: chunk boundaries land on syntactic units
  (`fn`, `impl`, `class`, `interface`, …), not arbitrary line windows.
- `symbol_path` on every code chunk: the structured nesting that contains it.
- Package membership for Rust (`Cargo.toml`) and TS/JS (`package.json`).
- `.gitignore`-aware file-list generation everywhere a list is produced.
- A clean extension path: adding a language is one new file; adding Compact
  later is one new file + dispatch arm.
- Architectural consistency: markdown and code share one `Chunker` trait,
  one config, one fallback, one output contract — without forcing markdown
  onto a worse parser.

## Non-goals (out of scope)

- **Compact / compactp** — line-window fallback until compactp ships on
  crates.io. Tracked as a follow-up.
- **Vyper, Move** — dropped (GitHub-only grammars, fragile/dialect-split).
- **Git-mode clone-and-ingest** (`--git` / `--ref`) — a source-acquisition
  feature orthogonal to chunking; its own follow-up (touches temp-dir
  lifecycle, SC-030).
- **Package detection beyond Rust/npm** — Go `go.mod`, Python `pyproject.toml`,
  etc. are not detected. Those languages chunk with `symbol_path` but
  `package_id = null`. (`package.kind` CHECK only allows `rust`/`npm`/
  `compact`/`other`; FR-006 defines only Rust + npm.)
- **Type-aware chunking** (e.g. `oxc`/`swc` semantic analysis) — syntax is
  enough to chunk; type info is a future feature.

## Architecture

Option A: a `Chunker` trait with per-concern modules under `mn-content/src/`.

```
crates/mn-content/src/
├── markdown.rs           // EXISTING — refactored to impl Chunker (keeps pulldown-cmark)
├── chunk.rs              // NEW — trait Chunker + Chunk, PathSegment, ChunkerConfig, ChunkError
├── package.rs            // NEW — Cargo.toml / package.json walkers
└── code/
    ├── mod.rs            // chunker_for(dispatch) over Language; re-exports trait from chunk.rs
    ├── language.rs       // enum Language + for_extension(ext) + shebang detection
    ├── splitter.rs       // text-splitter wrapper, generic over tree_sitter::Language
    ├── symbols.rs        // symbol_path extraction, parameterised by per-language kind table
    ├── line_window.rs    // LineWindowChunker (fallback — shared by Other / errors / Compact)
    ├── rust.rs
    ├── ts.rs             // .ts + .tsx (language_typescript / language_tsx)
    ├── js.rs             // .js, .jsx, .mjs, .cjs
    ├── scheme.rs         // vendored grammar
    ├── bash.rs
    ├── go.rs   python.rs   solidity.rs        // extended-grammars
    ├── toml.rs yaml.rs     html.rs   xml.rs   // markup-grammars
    └── swift.rs ruby.rs kotlin.rs csharp.rs haskell.rs java.rs  // all-grammars
```

### Shared trait and types (`chunk.rs`)

```rust
pub trait Chunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError>;
}

pub struct Chunk {
    pub content: String,
    pub path: Vec<PathSegment>,   // heading path (markdown) or symbol path (code)
    pub start_byte: usize,
    pub end_byte: usize,
    pub token_count: u32,
    pub chunk_index: u32,
    pub fallback_used: bool,       // true iff produced by LineWindowChunker
}

pub struct PathSegment {
    pub kind: String,              // "heading" | "impl" | "fn" | "class" | "key" | "element" | ...
    pub name: String,
}

pub struct ChunkerConfig {
    pub max_tokens: u32,            // default 400
    pub fallback_lines: u32,        // default 60
    pub fallback_overlap_lines: u32,// default 20
    pub max_file_bytes: u64,        // default 10 MiB (EC-52)
}
```

`PathSegment` is the single in-memory shape. At persist time it splits to the
two existing DB columns: markdown chunks → `heading_path` (segment `name`s,
all `kind="heading"`), code chunks → `symbol_path` (full `{kind,name}` JSONB).

### Dispatch

`chunker_for(language) -> Box<dyn Chunker>`. `Language::for_extension` maps
extensions (with shebang fallback per EC-53) to a `Language`. Markdown is
dispatched by `DocumentKind::Markdown` (its own chunker). `Language::Compact`
and `Language::Other` map to `LineWindowChunker` today.

### Markdown refactor

`chunk_markdown` keeps its pulldown-cmark implementation and heading-path
logic unchanged in behavior; it is adapted to implement `Chunker` and to emit
`Chunk { path: [{kind:"heading", name}], … }`. Budget unit switches from bytes
to tokens (see below). This is an interface adaptation, not a rewrite — the
tested CommonMark logic stays.

**Why not tree-sitter for markdown:** markdown isn't context-free;
`tree-sitter-markdown` is a fragile two-grammar block+inline split, strictly
less correct than pulldown-cmark. And `text-splitter`'s `CodeSplitter` doesn't
produce `heading_path` — we'd build it ourselves regardless. Same architecture,
different parser, is the right consistency.

## Budget unit

All chunkers budget in **tokens**, not bytes. Code never overran a byte
heuristic well; with real BPE counts now available (`tokens::count`, from the
tokenizer landed for accurate token counting), tokens is the honest unit.

- **Default budget:** 400 tokens. The embedder (`bge-base-en-v1.5`) caps at
  512; 400 leaves headroom for special tokens and occasional under-count.
- **No overlap** between adjacent *semantic* chunks (boundaries fall on
  syntactic units, so flow-across-boundary context matters less than for prose).
- **Line-window fallback:** 60 lines / 20-line overlap (FR-008/048). Overlap
  stays here because line-window has no syntax awareness.
- **Markdown** moves to the token budget too; its existing tests get updated
  expected sizes.

`text-splitter`'s `CodeSplitter` does the budget-fitting: largest semantic
node that fits; recurse into children when a node is too big; hard-split at a
token boundary only when a single leaf still exceeds budget (`symbol_path`
still reflects the enclosing node).

## File-list generation + filtering

`ignore` (BurntSushi/ripgrep) is the file-list filtering engine **everywhere a
list is generated** — `mnm manifest generate` and any path/directory-based
ingestion. Explicit manifest `file:` entries remain authoritative (FR-017); a
manifest may list markdown and code files freely. `walkdir` stays only on the
existing manifest-driven path; `ignore` powers generated lists. (`globset`,
already a dep, backs the glob overrides.)

Chunker selection is **per file, by extension** — independent of how the file
entered the list. A directory walk that turns up `README.md` next to `lib.rs`
chunks each correctly.

### Filter precedence

Two stages. **Stage 1 — ignore layers** (prune; each independently disableable):

```
.git/                       always excluded, NOT disableable
default skips               node_modules, target, vendor, dist   [off: --disable-default-ignore-list]
.gitignore / .ignore / .git/info/exclude                         [off: --no-respect-gitignore]
```

Also skipped as default excludes (EC-52): generated/minified patterns
`*.min.js`, `*.bundle.js`, `*_pb.ts`.

**Stage 2 — selection** (applied to stage-1 survivors):

```
--include <glob>   WHITELIST: if any present, file must match ≥1 to be kept
--exclude <glob>   removes from the kept set; beats --include
```

**Evaluation order (first match wins):**

1. Under `.git/` → excluded (always)
2. Matches default skip, and `--disable-default-ignore-list` unset → excluded
3. Matches gitignore/.ignore, and `--no-respect-gitignore` unset → excluded
4. `--exclude` matches → excluded
5. `--include` globs exist and file matches none → excluded
6. Otherwise → included

`--include '*.rs'` ⇒ only `.rs`. `--exclude` always carves out, even from a
whitelist. The whitelist narrows the ignore-surviving set; it does not
resurrect ignored files (use the disable flags for that).

## CLI surface

No `mnm ingest md` / `mnm ingest code` subcommands — superseded by the unified
`mnm ingest run` (per the PR #50 ingest-UX rework; `spec.md` annotated
2026-05-28). New flags on `ingest run`:

| Flag | Default | Purpose |
|---|---|---|
| `--code-chunk-tokens <n>` | 400 | semantic chunk budget |
| `--code-chunk-lines <n>` | 60 | line-window fallback size |
| `--code-chunk-overlap <n>` | 20 | line-window fallback overlap |
| `--include <glob>` | — | whitelist (repeatable) |
| `--exclude <glob>` | — | additive exclude (repeatable) |
| `--no-respect-gitignore` | off | disable `.gitignore`/.ignore layer |
| `--disable-default-ignore-list` | off | disable default skips layer |
| `--max-file-size <bytes>` | 10 MiB | oversize skip (EC-52) |

## Data flow

```
mnm ingest run --manifest … --source-slug …
  → file-list generation (manifest entries OR ignore-filtered walk, shared filter)
  → resolve.rs::kind_for → (DocumentKind, Language) per file, by extension (+ shebang, EC-53)
  → package detection (NEW) for code files: nearest Cargo.toml [package] / package.json .name
  → per-file dispatch (plan.rs, CHANGED):
       DocumentKind::Markdown  → MarkdownChunker            (heading_path; token units now)
       DocumentKind::Code      → chunker_for(language)       (symbol_path)  [was markdown fallback]
       DocumentKind::Plaintext → LineWindowChunker           [was markdown fallback]
  → binary sniff (magic number) → skip + warn, count in summary.skipped_files (Story 7 item 12)
  → Chunk → PlannedChunk (heading_path | symbol_path, package, byte ranges) → upload → source_version promote
```

Integration points (the only existing files that change):

- `mn-content/src/ingest/plan.rs:262` — dispatch arm calls `chunker_for(...)`.
- `mn-content/src/ingest/plan.rs` — `PlannedChunk` carries structured
  `symbol_path` + package linkage.
- `mn-content/src/manifest/resolve.rs:234` — `kind_for` also returns `Language`.
- `mn-content/src/markdown.rs` — implements `Chunker`, token units.
- File-list generation switches to `ignore` where lists are generated.

Untouched: walker (manifest path), source_version snapshot/promotion, binary
sniffing.

## Error handling & fallbacks

The chunker never fails a whole run for one bad file (FR-049). Layered:

| Situation | Behavior | Spec |
|---|---|---|
| Parser produces ERROR nodes (recoverable) | chunk the good regions; no warning unless tree is mostly errors | FR-049 |
| Parser fails / root ERROR child spans >50% of file bytes | `LineWindowChunker` for that file; per-file warning; `fallback_used=true`, `symbol_path=[]` | FR-049, SC-029 |
| `--strict` set | parser error promotes to a run failure naming the file | FR-049 |
| Unknown extension (`Language::Other`) | `LineWindowChunker`, `symbol_path=[]` | FR-048 |
| File > `--max-file-size` | skip + warn, count in `summary.skipped_files` | EC-52 |
| Generated/minified (`*.min.js`, `*.bundle.js`, `*_pb.ts`) | excluded before chunking | EC-52 |
| Binary (magic-number sniff) | skip + warn, count in `summary.skipped_files` | Story 7 item 12 |
| Empty / whitespace-only | zero chunks (mirrors `chunk_markdown`) | — |
| Oversized single semantic node | `text-splitter` recurses; hard-split at token boundary if a leaf still overflows; `symbol_path` = enclosing node | — |

`ChunkError` is internal to `code/`; the planner maps it to a warning
(default) or run failure (`--strict`). It never panics. The line-window
fallback is one shared implementation (Other / parser-error / Compact).

## Languages & feature gating

Grammars are only used at **ingest time**. They're feature-gated so a
search-only build can stay lean; an absent grammar **degrades gracefully** to
line-window (ingestible, just no `symbol_path`).

```toml
[features]
default           = ["core-grammars"]
core-grammars     = [rust, javascript, typescript, scheme (vendored), bash]
markup-grammars   = [toml, yaml, html, xml]
extended-grammars = [go, python, solidity]
all-grammars      = ["markup-grammars", "extended-grammars",
                     swift, ruby, kotlin, c-sharp, haskell, java]
```

- **Always compiled, never gated:** markdown (`pulldown-cmark`) + line-window.
- `--no-default-features` ⇒ lean build, every language line-windows.
- **Scheme is vendored** (`vendor/tree-sitter-scheme/` + `build.rs`); part of
  `core-grammars`. Rationale: the Compact compiler is written in Scheme
  (reading the compiler's source, distinct from Compact contracts).
- **Two symbol_path semantics:** code languages → `{kind:"impl"/"fn"/…}`;
  data/markup (TOML/YAML/HTML/XML) → structural `{kind:"key"/"element", name}`.
- **HTML vs XML:** both included in `markup-grammars`. XML pairs with the
  JVM/.NET/mobile additions (`pom.xml`, `.csproj`, `AndroidManifest.xml`).

### Grammar acquisition

Most are `cargo add` from crates.io (pin to a version ABI-compatible with the
`tree-sitter` runtime; verify with `cargo add`/`cargo search` at impl time).
For Solidity, use the canonical `JoranHonig` grammar (an "unofficial" fork also
exists — pick one). Scheme is vendored (`parser.c` + `build.rs` + FFI binding).

### Adding a language later (incl. Compact)

1. Add the grammar (crate or vendored) under the appropriate feature.
2. Add a `code/<lang>.rs` with its symbol-kind table.
3. Add the `Language` variant + `for_extension` mapping + dispatch arm.

Compact: when compactp publishes, add `code/compact.rs` backed by
`compactp_ast` (`Item` enum + rowan byte ranges + built-in error recovery),
flip the `Language::Compact` dispatch arm off line-window. Single-file change.

## Schema

**One migration:** `chunk.symbol_path` `text[]` → `jsonb`, storing
`[{"kind":"impl","name":"Foo"}, …]`. Greenfield column (no code chunks exist;
markdown uses `heading_path`), near-zero risk. jsonb gives kind-filtering via
`@>`. Any `mn-store` code binding `symbol_path` as `Vec<String>` updates to the
structured type.

Everything else is populate-don't-migrate:

- `chunk.start_byte`/`end_byte` — exist (FR-009). No line columns (derive on
  render).
- `document.language` — exists; the chunker sets it (language is per-document,
  not per-chunk).
- `document.package_id` → `package` (kind/name/version/manifest_path/metadata) —
  exists. Populated for `rust`/`npm` only.

## Package detection (`package.rs`)

- **Rust:** nearest ancestor `Cargo.toml` with a `[package]` table → `kind=rust`,
  `name` from `[package].name`, `manifest_path` set. A workspace-virtual-root
  `Cargo.toml` (`[workspace]`, no `[package]`) is skipped (spec line 439).
- **TS/JS:** nearest ancestor `package.json` with `.name` → `kind=npm`,
  `name`, `manifest_path` set.
- **Everything else:** no package row (`package_id = null`).

## Testing

- **Per-language fixtures** (`crates/mn-content/tests/fixtures/<lang>/`): small
  crafted files hitting the patterns that matter per language + one malformed
  file for the recovery path. Tests `#[cfg(feature = "…")]`-gated to match each
  grammar's feature.
- **Per-language assertions:** boundaries land on semantic units; `symbol_path`
  correct (structured); `token_count ≤ budget` except forced hard-splits;
  `fallback_used=true` + `symbol_path=[]` on malformed input.
- **Package detection tests:** Rust `Cargo.toml` → `rust` (workspace root
  skipped); `package.json` → `npm`; others → null.
- **Filter tests:** the full precedence ladder over a synthetic tree
  (pure-function, no I/O).
- **E2E smoke:** small vendored snapshot (<200 KB, ~10 mixed files) ingested
  against testcontainers Postgres; asserts a mixed `.md`+`.rs`+`.ts` tree
  routes per-file correctly.
- **Markdown migration:** existing markdown tests get updated expected sizes
  (byte→token).
- **CI:** one default-feature run + one `--all-features` run (exercises gated
  languages + vendored Scheme; the all-features build is slower — all grammars
  compile).

## Open follow-ups (separate specs)

1. **Compact chunker** via compactp, once published to crates.io.
2. **Git-mode** clone-and-ingest (`--git`/`--ref`, temp-dir lifecycle, SC-030).
3. **Package detection** for Go/Python/etc. (needs a `package.kind` CHECK
   widening + per-ecosystem manifest walkers), if/when wanted.

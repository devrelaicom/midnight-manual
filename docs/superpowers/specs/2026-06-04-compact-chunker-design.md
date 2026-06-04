# Compact chunker (compactp integration) — design

**Date:** 2026-06-04
**Status:** draft
**Touches:** `mn-content` (new `code/compact.rs` chunker + one dispatch arm, new
`compact` feature), workspace `Cargo.toml` (two new optional deps), `mn-cli`
(feature passthrough so the chunker can be excluded from a from-source build),
`README.md` (grammar-tier paragraph + make the "first-class citizen" claim
true). No schema migration. No `mn-server` changes.

This builds the **Compact slot** the [code-chunkers
design](./2026-05-28-code-chunkers-design.md) deferred: *"Compact stays on the
line-window fallback until [`compactp`](https://github.com/devrelaicom/compactp)
is published to crates.io, at which point it gets its own chunker (a single new
file + dispatch arm)."* compactp shipped to crates.io as `0.1.0-beta.1` on
2026-06-01, so the slot can now be filled.

## Problem

`Language::Compact` exists in the language enum and maps `.compact` correctly,
but `chunker_for_ext` has **no arm** for it — so every `.compact` file silently
falls through to `LineWindowChunker` today. A search hit lands on an arbitrary
60-line window with no `symbol_path` and no package membership. Meanwhile the
README advertises Compact as a *"first-class citizen ... circuits, ledger
declarations, witnesses, and contracts all become their own
semantically-bounded, attributable chunks"* — which is currently aspirational,
not true. Spec FR-047 scoped MVP as a "hand-rolled top-level module scanner
until a grammar exists"; that scanner was never built (`PackageKind::Compact`
is a defined type that nothing produces). compactp is the grammar, so we can
deliver real symbol-aware Compact chunking instead of the hand-rolled stopgap.

## Goals

- Semantic, token-budgeted chunking for `.compact`: boundaries land on Compact
  syntactic units (circuit, ledger, witness, contract, module, struct, enum,
  type), never mid-statement.
- `symbol_path` on every Compact chunk (e.g. `[circuit increment]`, or
  `[module FungibleToken › circuit transfer]` when module-nested).
- Module-based package detection: a top-level `module <Name>` → a
  `PackageKind::Compact` package, reusing the existing per-document package
  model (no schema change).
- Graceful degradation: when the `compact` feature is off, `.compact` falls
  back to line-window exactly as it does today — zero behaviour change.
- A clean from-source story: `cargo build` pulls compactp from crates.io with
  nothing for the user to install; the chunker is a default-on feature that is
  a one-liner to exclude.

## Non-goals (out of scope)

- **Per-chunk, multi-module package tagging (EC-06 in full).** The schema links
  package at the *document* level (`document.package_id`); chunks have no
  `package_id`. Tagging each chunk by its lexically-enclosing module ("multiple
  package rows per file") needs a migration + plan/upload/query plumbing. See
  **Decision 4 (P1 vs P2)** — deferred as a clean follow-up, gated on evidence
  that multi-module-per-file files actually occur.
- **Shelling out to the `compactp` CLI.** Rejected — see **Decision 1**.
- **Compact language/semantic verification.** We trust compactp's parser as the
  grammar source of truth; the chunker's output (byte ranges, symbol names) is
  mechanical and unit-testable. No `/midnight-verify` pass is required for this
  work.
- **Changes to other languages' chunkers**, the markdown chunker, the line-window
  fallback, or the ingest orchestrator beyond passing a Compact `PackageRef`.

## Decisions

### Decision 1 — Library dependency, not CLI subprocess

Consume compactp as a **library** (`compactp_parser` + `compactp_ast` from
crates.io), compiled into `mn-content`. Rejected: shelling out to the `compactp`
binary.

- Shelling out makes the "others don't have compactp locally" problem *worse* —
  it becomes a **runtime** dependency (binary on PATH, version-matched, on every
  machine). The library is compiled in at **build** time from crates.io; `cargo
  build` just works.
- Ingest chunks thousands of files; a process spawn + JSON (de)serialize *per
  file* is a throughput tax the in-process library avoids.
- compactp is pure-Rust `rowan` (no native build step), so it compiles cleanly
  into the distroless server image; a subprocess cannot.
- Every other language chunker is already in-process; the library keeps Compact
  consistent.

The honest counter-argument (looser coupling to a `0.1.0-beta.1` API; panic
isolation) is outweighed: a tree-schema change breaks JSON consumers too, and
compactp denies `panic`/`unwrap`/`expect` in its lints, with our line-window
fallback as a backstop.

### Decision 2 — Bespoke rowan walker, not the tree-sitter path

The existing code-chunking machinery (`run_tree_sitter`,
`text-splitter::CodeSplitter`, `symbol_path_at`) is entirely `tree_sitter::*`-
typed. compactp is **rowan**-based, so the Compact chunker **cannot** reuse it.
`code/compact.rs` is a self-contained walker, structurally parallel to
`markdown.rs` (its own implementation behind the shared `Chunker` trait), not to
`rust.rs` (a thin wrapper over `run_tree_sitter`).

### Decision 3 — Uniform recursive largest-fit splitting

Since text-splitter's recursive packer is unavailable, we define our own, and
it is **uniform** (no special-casing of which node kinds to descend into):

> Given a node, if it fits the token budget, emit it as one chunk. Otherwise
> descend into its child boundaries, pack adjacent children greedily up to the
> budget, and recurse into any child that itself exceeds the budget. Only when a
> node has no splittable children *and* still exceeds budget (a single giant
> statement/expression — rare) do we line-window that leaf.

This walks `module → circuit → body block → statements` structurally. An
oversize circuit is split on **statement boundaries**, and every slice keeps
`symbol_path = [circuit foo]` (the same way the tree-sitter path tags every
slice of a big function with its enclosing `fn`). Line-window stops being the
oversize strategy and becomes only the leaf-level last resort.

Rejected alternatives: *one-chunk-per-item* (tiny-chunk explosion for files of
many small structs — bad embedding density for a retrieval corpus); *descend
into modules only* (line-windowing oversize circuits is a quality regression —
splits mid-statement, loses structure).

**Care-point:** rowan is lossless, so the walker must assign inter-node trivia
(whitespace/comments between items) to an adjacent chunk so chunks fully cover
the source with no gaps and no overlaps — **nothing is dropped** (EC-51). This
is a testable invariant (see Testing).

### Decision 4 — P1 (per-document, single module) now; P2 deferred

The package unit is the top-level `module <Name>`. Given the one-package-per-
document schema:

- **P1 (chosen):** if a `.compact` file declares **exactly one** top-level
  module, the document's `package = {kind: compact, name, manifest_path: null}`,
  and all its chunks inherit it via `document.package_id` — identical to how
  Rust/npm packages work. A file with **no** module → `package = null`. Fits the
  existing schema and pipeline with **no migration**. Satisfies SC-028 ("≥1
  chunk tagged `compact`/Foo") for single-module files.
- **P2 (deferred):** add `chunk.package_id` + thread an optional package through
  `PlannedChunk → planner → upload`, tagging each chunk by its enclosing module.
  Fully satisfies EC-06/EC-51. Materially larger (schema + pipeline + queries).

**Why P1 is the right call, reinforced by usage reality:** most contracts
declare *no* module at all (top-level ledger + circuits + witnesses), so most
files get `package = null` under **both** P1 and P2 — the common path is
unaffected by the choice. Multi-module-per-file (P1's only gap) is therefore
rare-squared, so P2's per-chunk machinery would almost never produce more than
one package per file. Package detection's real yield concentrates on module-
based **library** repos (OZ `compact-contracts`), which are typically one module
per file → P1 nails them. For application contracts, `symbol_path` (circuit /
ledger / witness names) is the primary "what is this" signal; package is a bonus
for library repos.

**P1's documented limitation:** a file with **multiple** top-level modules can
carry only one package → we set `package = null` and log it. That is the EC-06
case, left to P2.

## Architecture

A new feature-gated module, parallel to the existing per-language files:

```
crates/mn-content/src/code/
├── mod.rs            // + one dispatch arm (feature-gated)
├── compact.rs        // NEW — CompactChunker (self-contained rowan walker)
└── …                 // unchanged tree-sitter chunkers
```

`compact.rs` implements the existing `Chunker` trait and produces the existing
`Chunk` shape — no new output contract.

**Parse + walk:**

```rust
use compactp_ast::{AstNode, Item, SourceFile};
use compactp_syntax::SyntaxNode;

let parsed = compactp_parser::parse(body);     // ParseResult { green, errors }
let root = SyntaxNode::new_root(parsed.green); // always full-coverage CST
let Some(file) = SourceFile::cast(root) else { /* fall back */ };
for item in file.items() { /* Item::CircuitDef(c) => c.name(), … */ }
```

**Dispatch** (`code/mod.rs`):

```rust
#[cfg(feature = "compact")]
Language::Compact => Box::new(compact::CompactChunker),
```

When the feature is off, `Language::Compact` falls through the existing
`_ => Box::new(LineWindowChunker)` arm — `.compact` degrades to line-window
exactly as today. No language-detection changes (the enum variant already
exists).

### symbol_path

From compactp's authoritative `Item` set, these contribute a
`SymbolSegment { kind, name }` (kind label → source construct):

| kind label  | construct                                  |
|-------------|--------------------------------------------|
| `module`    | `ModuleDef` (also the package unit)        |
| `ledger`    | `LedgerDecl`                               |
| `constructor` | `ConstructorDef`                         |
| `circuit`   | `CircuitDef` **and** `CircuitDecl`         |
| `witness`   | `WitnessDecl`                              |
| `contract`  | `ContractDecl`                             |
| `struct`    | `StructDef`                                |
| `enum`      | `EnumDef`                                  |
| `type`      | `TypeDecl`                                 |

`Pragma` / `Include` / `Import` / `ExportList` are file-level preamble — no
segment (the "preamble before first named item" case the tree-sitter path
handles via `first_symbol_start`). Each chunk's `symbol_path` is the enclosing
named item at its start byte; if the start lands in preamble, use the first
named item contained in the chunk. The **no-module top-level file is the primary
path**: `SourceFile::items()` yields top-level circuits/ledger/witness directly,
so `symbol_path = [circuit foo]` with no module prefix.

### Error handling & fallback

Mirrors `run_tree_sitter`. `compactp_parser::parse` always returns a full-
coverage CST (ERROR nodes wrap unparseable regions) + `errors: Vec<Diagnostic>`,
so parsing never hard-fails. We add the same **catastrophic-error heuristic**:
walk the CST, sum the byte span of `ERROR` nodes; if `error_bytes * 2 >
body.len()` (>50%), fall back to `LineWindowChunker` with `fallback_used = true`.
Also fall back if `SourceFile::cast` fails or yields no items on non-empty
input. A non-catastrophic parse with some diagnostics proceeds on the recovered
CST (optionally logging the diagnostic count via `tracing`). Contract honoured:
empty/whitespace → empty vec; otherwise ≥1 chunk; never panic; never abort the
run for one bad file.

## Dependency wiring & feature gating

- **mn-content:** new feature `compact = ["dep:compactp_parser",
  "dep:compactp_ast"]`; added to default → `default = ["core-grammars",
  "compact"]`. Standalone feature (not folded into a grammar tier) so it's
  reasoned-about independently — matches the "experimental, easy to drop"
  intent.
- **Workspace `[workspace.dependencies]`:** `compactp_parser = { version =
  "=0.1.0-beta.1" }` and `compactp_ast = { version = "=0.1.0-beta.1" }` — exact
  pin because it's a pre-release with an unstable API. Listed `optional = true`
  in `mn-content`. Pure-Rust, no native build step (unlike the tree-sitter
  grammars).
- **MSRV:** compactp requires Rust ≥ 1.90; mnm is pinned at 1.91 — already
  satisfied, **no MSRV bump**.
- **Opt-out UX:** to make `cargo build --no-default-features --features
  <tiers-without-compact>` a clean one-liner, `mn-cli` depends on `mn-content`
  with `default-features = false` and re-exports the tiers (`core-grammars`,
  `markup-grammars`, …, `compact`) as passthrough features, defaulting to the
  full set. (Cargo features are additive, so a subtractive build inherently
  requires `--no-default-features` + re-add — there is no way around that.)
- **From-source integrity:** the committed manifest points at crates.io, so
  `cargo build` works for everyone with nothing to install. Local
  co-development against a compactp checkout is an **uncommitted**
  `[patch.crates-io]` — never committed (a path patch breaks others' builds).

## Data flow (unchanged pipeline, new producer)

```
.compact file ─▶ CompactChunker.chunk()
                   ├─ compactp_parser::parse → CST
                   ├─ recursive largest-fit split (Decision 3) → Vec<Chunk> (+ symbol_path)
                   └─ catastrophic parse → LineWindowChunker (fallback_used=true)
                 ▼
caller computes Compact PackageRef (Decision 4 / P1) ─▶ PlannedDocument.package
                 ▼
existing plan → embed → upload → promote  (no changes)
```

Package detection for Compact is **content-based** (parse for top-level
modules), unlike the filesystem walk in `package.rs::detect`. It runs in the
caller that already supplies `PlannedDocument.package` — for a `.compact` file,
parse once, and if exactly one top-level `ModuleDef` exists, emit the
`PackageRef`. (Reusing the same parse the chunker performs is an optimisation
the plan can specify; correctness does not depend on it.)

## Testing & acceptance

- **Unit** (`#[cfg(all(test, feature = "compact"))]`):
  - top-level circuit `symbol_path` (no module — the common path);
  - nested `module › circuit`;
  - ledger / witness / struct / enum / type kinds;
  - small-sibling packing → one chunk;
  - oversize circuit → recursive statement-boundary split, `symbol_path`
    preserved, **not** `fallback_used`;
  - garbage input → `fallback_used = true`;
  - empty / whitespace → empty vec.
- **Invariant (proptest):** chunk byte-ranges fully cover the source, no
  overlaps, no dropped bytes.
- **Package:** one-module → `{compact, name}`; zero-module → null; multi-module
  → null + logged (P1).
- **Corpus:** add `.compact` fixtures to `tests/sample_corpus.rs`.
- **Acceptance (SC-028):** clone OZ `compact-contracts`, ingest, assert every
  `.compact` with a module declaration has ≥ 1 chunk tagged `compact`/`<module>`
  — **and** confirm those files are ≤ 1 top-level module each. **This is the
  P1 → P2 escalation tripwire:** if multi-module files exist in OZ, raise P2.

## Risks & open questions

- **OZ module-count assumption.** P1 satisfies SC-028 iff OZ `.compact` files
  declare ≤ 1 top-level module each. Verified during implementation via the
  acceptance test; escalate to P2 only if violated.
- **Beta API churn.** compactp is `0.1.0-beta.1`. The exact version pin contains
  the blast radius; the chunker uses only the documented minimal surface
  (`parse`, `SourceFile`, `Item`, `.name()`), reducing exposure.
- **Token counting inside the recursion.** Splitting decisions use
  `crate::tokens::count` (the same BPE tokenizer the rest of the pipeline uses),
  so budgets are consistent across languages.

## Docs to update (in implementation)

- README grammar-tier paragraph: document the `compact` feature + the opt-out
  invocation; the "first-class citizen" claim becomes true.
- Note in spec FR-047 that compactp supersedes the planned hand-rolled scanner.

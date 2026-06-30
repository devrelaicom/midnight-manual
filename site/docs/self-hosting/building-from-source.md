---
title: Building from source
sidebar_label: Building from source
description: Build the midnight-manual CLI and cloud-server binaries from a checkout with cargo, including the optional Cargo features that gate Compact chunking and the tree-sitter language grammars.
---

# Building from source

This is an operator page. You build from source to run your own [cloud server](./cloud-server.md), or to control what the CLI can chunk when you [run an ingest](./running-an-ingest.md) — the CLI's optional features are all ingestion concerns. A search-and-read user never needs any of this; the [prebuilt binaries](/docs/install) cover them.

The server has no prebuilt artifact at all — it ships only as a Docker image — so building it from source is the only way to run a modified server outside the container.

## Prerequisites

- A [Rust toolchain](https://rustup.rs). The workspace MSRV is **1.93**.
- A C compiler (`clang` from the Xcode Command Line Tools on macOS; `gcc` from `build-essential` on Debian/Ubuntu). The tree-sitter grammars and the vendored Scheme parser compile C at build time.
- `git` to clone the repository.

No database is needed to build. `sqlx` runs its queries at runtime rather than checking them at compile time, so the server binary compiles with no `DATABASE_URL` and no offline query cache. Postgres is a runtime dependency only.

```bash
git clone https://github.com/devrelaicom/midnight-manual.git
cd midnight-manual
```

## What the workspace builds

Two crates carry binary targets:

| Binary | Crate | What it is |
|---|---|---|
| `midnight-manual` | `midnight-manual` | The CLI: search, read, ingest, admin, and `mcp serve` (the local MCP server). |
| `mnm` | `midnight-manual` | Short alias for the same CLI, built from the same crate. |
| `midnight-manual-server` | `midnight-manual-server` | The cloud corpus host (`axum` + Postgres/pgvector). See [Cloud server & deploy](./cloud-server.md). |

The other ten `mnm-*` crates are libraries. They compile as dependencies of the two binary crates and produce no binaries of their own.

## Build the CLI

Building the CLI crate produces both `midnight-manual` and `mnm` in `target/release/`:

```bash
cargo build --release -p midnight-manual
install -m 0755 target/release/mnm ~/.local/bin/mnm
```

Or let cargo install both binaries onto your `PATH` in one step:

```bash
cargo install --path crates/midnight-manual
```

### CLI optional features

The CLI's optional features are all about ingestion. The CLI chunks every source locally before upload — [the server never chunks or embeds](./ingestion-pipeline.md) — so the grammars you compile in decide which languages `mnm ingest` can split on symbol boundaries. None of them change search or read, and the server build ignores them entirely.

Compact chunking is on by default. Every other language grammar beyond the core set is opt-in at build time.

| Feature | Default | Adds |
|---|---|---|
| `compact` | **on** | Compact-language chunking via the experimental [`compactp`](https://crates.io/crates/compactp_parser) parser. |
| `mnm-content/markup-grammars` | off | TOML, YAML, HTML, XML grammars. |
| `mnm-content/extended-grammars` | off | Go, Python, Solidity grammars. |
| `mnm-content/all-grammars` | off | Everything in markup + extended, plus Swift, Ruby, Kotlin, C#, Haskell, Java. |

The **core grammars** — Rust, TypeScript, JavaScript, Bash, and Scheme — are always compiled in and cannot be removed with a feature flag.

Drop Compact chunking for a leaner build (the core grammars stay):

```bash
cargo build --release -p midnight-manual --no-default-features
```

Add extra language grammars. The `mnm-content/<feature>` form enables a feature on the dependency from the command line:

```bash
# Compact (default) + TOML/YAML/HTML/XML
cargo build --release -p midnight-manual --features mnm-content/markup-grammars

# Compact (default) + every supported grammar
cargo build --release -p midnight-manual --features mnm-content/all-grammars
```

Combine the two to opt out of Compact while opting into more grammars:

```bash
# No Compact, all other grammars
cargo build --release -p midnight-manual --no-default-features --features mnm-content/all-grammars
```

After `--no-default-features`, re-enable Compact by naming the CLI's own `compact` feature:

```bash
cargo build --release -p midnight-manual --no-default-features --features compact,mnm-content/extended-grammars
```

Match the grammar set to the languages in the sources you ingest. A grammar you compile in lets the [smart chunker](/docs/concepts/smart-chunker) split that language on real syntactic boundaries; a source in a language whose grammar is absent still ingests and stays searchable — it just falls back to line-window chunking with no symbol paths, and never aborts the run. Grammars cost compile time and binary size, which is why only the core set and Compact are on by default.

## Build the server

```bash
cargo build --release -p midnight-manual-server
```

The server binary has no runtime feature flags — there is nothing to toggle at build time. Production deployments run the multi-arch Docker image built from `Dockerfile.server`, not a bare `cargo build`. The [Cloud server & deploy](./cloud-server.md) runbook covers the image, Fly.io provisioning, and the runtime environment variables (`DATABASE_URL`, `MIDNIGHT_MANUAL_JWT_SECRET`, `VOYAGE_API_KEY`, and the rest).

To run the freshly built server against a local Postgres without packaging an image:

```bash
export DATABASE_URL=postgres://localhost/midnight_manual
export MIDNIGHT_MANUAL_USER_STORE=./users.toml
export MIDNIGHT_MANUAL_JWT_SECRET=…     # HS256 signing secret, ≥ 32 bytes
cargo run --release -p midnight-manual-server
```

## Build everything

To compile the whole workspace — both binaries and all libraries — in one pass:

```bash
cargo build --release --workspace
```

The `integration` feature that each crate declares is a **test gate**, not a runtime option. It switches on the Postgres- and network-backed integration tests (run in CI) and has no effect on a normal binary build. You only need it for `cargo test --features integration`, never for `cargo build`.

## Related pages

- [The ingestion pipeline](./ingestion-pipeline.md) — where client-side chunking and embedding fit, and why the grammar features matter.
- [Running an ingest](./running-an-ingest.md) — the operator workflow these CLI features feed.
- [Cloud server & deploy](./cloud-server.md) — the Docker image, Fly.io runbook, and server environment variables.
- [The smart chunker](/docs/concepts/smart-chunker) — what the language grammars are used for.
- [Install mnm](/docs/install) — prebuilt binaries for search-and-read users.

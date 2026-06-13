# CLAUDE.md — midnight-manual

A Rust-based RAG platform for the Midnight Network. Three deliverables from one Cargo workspace: an admin/developer CLI (`midnight-manual` / `mnm`), a local MCP server (`mnm mcp serve` subcommand), and a Fly.io-deployed cloud server (`midnight-manual-server`).

See `CONSTITUTION.md` for non-negotiable principles. See `specs/001-rag-platform/` for the v1 spec, plan, research, data model, contracts, and quickstart.

## Active Technologies

**Language**: Rust stable (MSRV 1.91, pinned in `Cargo.toml`). MSRV bumped from 1.83 → 1.91 during Phase 1 because the transitive-dep ecosystem has moved well past 1.83: `cargo-platform 0.3.x` needs 1.91, `jsonwebtoken 10.x` / `time 0.3.47` / `image 0.25.x` / `darling 0.23.x` need 1.88, and `cargo-platform 0.3.3` (pulled by `cargo-metadata`) needs 1.91. 1.91 is the smallest version that satisfies the full graph.

**Workspace crates** (10 total — 8 lib + 2 bin; see `specs/001-rag-platform/plan.md`):
- `mn-core` — types, errors, config (D17/D18), auth.toml (D28), scoring policy (D24), model-id wire format
- `mn-store` — sqlx + Postgres + pgvector + numbered migrations under `crates/mn-store/migrations/`
- `mn-retrieval` — hybrid query construction, RRF (k=60), confidence scoring (trust × relevance, D24)
- `mn-content` — Markdown chunking (`pulldown-cmark`), code chunking (`tree-sitter` for Rust/TS/JS, hand-rolled scanner for Compact per FR-047), package detection
- `mn-embedding` — `fastembed-rs` wrapper for embedder (bge-base-en-v1.5) + reranker (bge-reranker-base)
- `mn-auth` — `ed25519-dalek` keypairs, `jsonwebtoken` HS256, `oauth2` + `octocrab` for GitHub flow, user-store TOML loader
- `mn-telemetry` — event schemas (FR-109), batching client (FR-113), three-mechanism opt-out (FR-107)
- `mn-mcp` — hand-rolled MCP JSON-RPC framing over stdio, 7 tools, lazy model load
- `mn-cli` (**bin**) — `midnight-manual` + `mnm` aliases; noun-first command tree (D19); admin-visibility hidden by default (D23)
- `mn-server` (**bin**) — `midnight-manual-server`; `axum` HTTP API, sweep job, rate-limit middleware

**Critical Dependencies**: `axum`, `tower`/`tower-http`, `tokio`, `sqlx` (postgres), `pgvector`, `fastembed`, `tree-sitter` + grammars, `jsonwebtoken`, `ed25519-dalek`, `oauth2`, `octocrab`, `clap` v4, `serde`+`serde_json`+`toml`, `serde_yaml`, `pulldown-cmark`, `tracing`+`tracing-subscriber`, `metrics`+`metrics-exporter-prometheus`, `jsonschema`, `utoipa`, `testcontainers`, `proptest`.

**Storage**: PostgreSQL 16+ with pgvector extension (Fly.io managed). Schema: 7 corpus tables + 3 admin tables + 2 telemetry tables = 12 tables total. HNSW index on `chunk.embedding`; GIN index on `chunk.tsvector`. See `specs/001-rag-platform/data-model.md`.

**Testing**: `cargo test` (unit) + `cargo test --features integration` (integration via `testcontainers`). Property-based via `proptest`. Canary CI gates per FR-112.

**Target Platform**:
- CLI/MCP: macOS (x86_64+aarch64), Linux (gnu+musl, x86_64+aarch64), Windows x86_64 (7 targets per FR-099).
- Server: linux/amd64 + linux/arm64 Docker on `gcr.io/distroless/cc`, deployed to Fly.io.

**Build tools**: `cargo`, `sqlx-cli` (for migrations + offline query checks), `cargo-dist` (releases), `release-please` (version bumps), `docker buildx` (multi-arch server image), `flyctl`.

## Recent Changes

- 2026-06-13 — Version provenance & matching: per-document extraction at ingest
  (Compact `language_version` pragma → `language_targets`; allowlisted
  `@midnight-ntwrk/*` + `@openzeppelin/compact-*` npm deps and `midnight-*`/`mn-*`
  cargo deps → `sdk_dependencies`; `package.version` populated). `version_satisfies`
  accepts a concrete version or a semver range; `version_match` is `strict` |
  `permissive` (permissive default — satisfying boost / distance-scaled near-miss
  penalty / breaking-mismatch drop, with the 0.x role shift). `/v1/facets` gains a
  two-level drill for version facets; the search skill is rewritten (two-regime
  guidance + support-matrix playbook). Re-ingest required to populate provenance.
- 2026-06-11 — VoyageAI reranking: inline server rerank in `/v1/search`
  (`rerank` = rerank-2.5 default | rerank-2.5-lite at half-rate billing | none;
  degrade-and-flag on budget/provider failure; `MIDNIGHT_MANUAL_SERVER_RERANK`
  kill switch), client placement auto-resolution (`VOYAGE_API_KEY` ⇒ local
  BYOK), instruction-following (`rerank_instructions`, 400-char cap, derived
  defaults), and full removal of the fastembed/ONNX reranker catalog.
- 2026-06-10 — Contextualized dual embeddings: general corpus model moves to `voyage-context-3` (contextualized chunk embeddings); code chunks gain a second `voyage-code-3` vector (migration 0011, opt-out via start-run without `code_embedding_model`); search/CLI/MCP gain `code_mode` (on/off/exclusive) fused via RRF. Full corpus re-ingest required after deploy.
- 2026-05-13 — `/sdd:plan` Phase 1: data-model, contracts (openapi.yaml + mcp-tools.json), quickstart generated. CLAUDE.md populated with v1 stack.
- 2026-05-13 — Feature branch `001-rag-platform` created; v1 spec (117 FRs, 110 edge cases, 66 success criteria, 28 decisions) bridged from `discovery/SPEC.md` into the sdd workflow.

## File Structure

```text
midnight-manual/
├── Cargo.toml                    # workspace root + shared deps
├── rust-toolchain.toml           # pins stable channel + components
├── rustfmt.toml
├── clippy.toml
├── CONSTITUTION.md               # mirrored to .sdd/memory/constitution.md
├── CLAUDE.md                     # this file
├── README.md                     # incl. "Telemetry & Privacy" section per FR-114
├── crates/
│   ├── mn-core/
│   ├── mn-store/
│   │   └── migrations/           # numbered .sql files
│   ├── mn-retrieval/
│   ├── mn-content/
│   ├── mn-embedding/
│   ├── mn-auth/
│   ├── mn-telemetry/
│   ├── mn-mcp/
│   ├── mn-cli/                   # bin: midnight-manual + mnm
│   └── mn-server/                # bin: midnight-manual-server
├── benches/                      # criterion benchmarks
├── tests/
│   ├── canary/                   # FR-112 / SC-061
│   ├── load/                     # SC-013
│   └── recall/                   # SC-014, SC-021, SC-048, SC-049
├── docs/
│   ├── cookbook/
│   │   └── query-enhancement.md  # Story 7 / FR-092
│   └── README-deploy.md
├── specs/001-rag-platform/       # spec + plan + research + design artifacts
├── discovery/                    # spec-writer artifacts (preserved for trace)
├── .sdd/                         # sdd workflow metadata
├── .github/workflows/
│   ├── ci.yml                    # PR gates: fmt, clippy, test, audit, canary
│   ├── release.yml               # release-please + cargo-dist + flyctl
│   └── canary.yml                # privacy canary tests (release gate)
├── Dockerfile.server             # multi-stage build for mn-server
└── fly.toml
```

## Common Commands

### Development

```bash
# One-shot pre-push checks (matches CI)
just check                       # cargo fmt --check && clippy -D warnings && test
just check-msrv                  # same against pinned MSRV toolchain

# Per-step
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace --features integration   # boots ephemeral Postgres+pgvector
cargo bench --workspace                          # criterion (scoring + RRF)

# Build artifacts
cargo build --release -p mn-cli                  # midnight-manual + mnm
cargo build --release -p mn-server               # midnight-manual-server
```

### Database

```bash
# Run / inspect migrations
sqlx migrate run --source crates/mn-store/migrations
sqlx migrate info --source crates/mn-store/migrations

# Prepare offline query cache (so sqlx::query! works without a live DB at compile time)
cargo sqlx prepare --workspace -- --tests
```

### Running locally

```bash
# Cloud server (requires DATABASE_URL + secrets — see quickstart.md)
cargo run --release -p mn-server

# CLI debug helper hitting the local server
cargo run --release -p mn-cli -- --server http://localhost:8080 search "compile compact contract"

# MCP server over stdio (for AI client integration)
cargo run --release -p mn-cli -- mcp serve
```

### Release (maintainer only)

```bash
# Releases are driven by merging the release-please PR; manual invocation is rarely needed.
# Local rehearsal:
cargo dist build
cargo dist plan
```

### Canary tests (privacy invariants)

```bash
just test-canary                 # CI gate; SC-061 / FR-112
```

<!-- MANUAL ADDITIONS START -->
<!-- Anything between these markers is preserved across `/sdd:plan` updates. -->
<!-- MANUAL ADDITIONS END -->

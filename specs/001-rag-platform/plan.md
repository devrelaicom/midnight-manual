# Implementation Plan: midnight-manual v1 (rag-platform)

**Branch**: `001-rag-platform` | **Date**: 2026-05-13 | **Spec**: [`spec.md`](./spec.md)
**Input**: Feature specification from `/specs/001-rag-platform/spec.md`

## Summary

Greenfield Rust implementation of a RAG platform for the Midnight Network. One Cargo workspace producing two distinct binaries from shared crates: a user-facing CLI (`midnight-manual` / `mnm` aliases) that also exposes a `mcp serve` subcommand for local MCP-server operation, and a server-only binary (`midnight-manual-server`) deployed to Fly.io behind a managed Postgres+pgvector instance. Hybrid FTS + vector retrieval with Reciprocal Rank Fusion (RRF, k=60); cross-encoder reranking on the MCP-server side; trust × relevance confidence scoring with a TOML-tunable policy; per-source-version snapshots with retention of 5; Ed25519 challenge-response auth for admins (1h JWT) and GitHub OAuth for read-uplift (30d bearer); opt-out telemetry with canary-enforced privacy invariants; release-please-driven continuous release to crates.io, Homebrew tap, GitHub Releases, GHCR, and Fly.io.

## Technical Context

**Language/Version**: Rust stable; MSRV pinned to `1.83.0` (Cargo.toml `rust-version = "1.83"`). Re-evaluated each minor (Cargo convention: MSRV bumps are MINOR releases).
**Primary Dependencies** (resolved during Phase 0; see [`research.md`](./research.md)):
- `clap` v4 (CLI, derive macros, global flags struct)
- `tokio` (async runtime, single-threaded for the CLI, multi-threaded for the server)
- `axum` (HTTP server framework — chosen over actix-web for tower ecosystem and simpler async model)
- `tower` + `tower-http` (middleware: rate limit, request-id propagation, CORS, body limit)
- `sqlx` (Postgres driver + migrations, compile-time-checked queries, pgvector trait via `sqlx-postgres` + `pgvector` crate)
- `pgvector` (Rust bindings for pgvector encode/decode)
- `fastembed` (embedding + reranker via ONNX Runtime; D1, D2, D14)
- `tree-sitter` + grammars: `tree-sitter-rust`, `tree-sitter-typescript`, `tree-sitter-javascript` (no Compact grammar yet; hand-rolled scanner)
- `jsonwebtoken` (HS256 JWT mint/verify per D21)
- `ed25519-dalek` (challenge-response keys per D10)
- `oauth2` (GitHub OAuth web + device flows per FR-115)
- `serde` + `serde_json` + `toml` (config/auth/scoring-policy parsing)
- `jsonschema` (per-event-type telemetry validation at /v1/telemetry boundary)
- `tracing` + `tracing-subscriber` + `tracing-bunyan-formatter` or similar (structured JSON logging per FR-105)
- `prometheus` or `metrics` + `metrics-exporter-prometheus` (Prometheus exposition at /metrics)
- `tokio-util` (graceful shutdown, signal handling)
- `keyring` — **NOT used in v1** per D28
- `release-please-action` (GitHub Action), `cargo-dist` (release pipeline; FR-099)

**Storage**: PostgreSQL 16+ with the `pgvector` extension (Fly.io managed Postgres cluster). Schema versioning via `sqlx migrate` (D22). HNSW index on `chunk.embedding`; GIN index on `chunk.tsvector`. Two telemetry tables (raw + daily aggregate) with 7-day rolling retention on raw.

**Testing**:
- `cargo test` (unit) for pure logic.
- `cargo test --features integration` (integration) backed by `testcontainers` for ephemeral Postgres with pgvector pre-installed.
- A JSON-RPC test harness exercising the MCP server over stdio (FR-036).
- Property-based tests via `proptest` for scoring determinism (SC-047) and FFI byte-equivalence (SC-051).
- Canary integration tests (FR-112, SC-061) running real components and grepping captured logs + telemetry rows.

**Target Platform**:
- CLI / MCP server: macOS (x86_64 + aarch64), Linux glibc + musl (x86_64 + aarch64), Windows x86_64. Seven build targets per FR-099.
- Cloud server: linux/amd64 + linux/arm64 Docker images on `gcr.io/distroless/cc`, deployed to Fly.io.

**Project Type**: Single Cargo workspace with multiple library crates and two binary crates (`mn-cli` and `mn-server` per D26). Workspace pattern chosen over single-crate-with-features for clearer module boundaries (Constitution II).

**Performance Goals**:
- MCP handshake cold start < 500 ms (SC-019; Constitution IV).
- First-call search < 2.5 s including lazy model load (SC-020).
- Steady-state search p95 < 1 s end-to-end (SC-020; Constitution IV).
- Cloud `/v1/search` p95 < 500 ms on a 100k-chunk corpus (SC-013).
- Cloud server cold start (process → readyz=200) < 5 s on the production Docker image (SC-036, SC-059).
- Ingest of midnight-docs main branch (~300 pages) < 5 min on a developer laptop (SC-007).
- Reranker quality lift: nDCG@5 +0.05 absolute over un-reranked (SC-021); confidence-sorted lift: nDCG@5 +0.05 over RRF-sorted (SC-048); multi-query recall@10 lift ≥ 8 percentage points (SC-049); hybrid recall@10 lift ≥ 10 percentage points over single-mode (SC-014).

**Constraints**:
- Constitution VII privacy invariants enforced by canary tests in CI (SC-061).
- p95 retrieval < 1 s (Constitution IV) — non-negotiable.
- Zero query content, tokens, chunk content, or env values in logs/telemetry — non-negotiable.
- Two distinct token types (admin vs read-uplift, D28); MCP server never sees admin tokens.
- Stable MCP contract; breaking changes require MAJOR bump (Constitution I, FR-037).

**Scale/Scope**:
- **Corpus**: ~100k chunks at v1; ~10 source repos (midnight-docs + curated code examples).
- **CLI surface**: ~30 subcommands across 14 noun groups (Story 8).
- **HTTP surface**: 14 read endpoints (Story 4) + 13 write/admin endpoints (Story 9) + 1 telemetry endpoint + 2 health endpoints + 1 metrics endpoint = 31 endpoints total at v1.
- **MCP surface**: 7 tools (Story 5).
- **Telemetry surface**: 6 event types (Story 11).
- **Schema**: 7 corpus tables (source, source_version, embedding_model, node, document, chunk, package) + 3 admin/auth tables (rate_limit_override, plus `user` and `api_key` reserved) + 2 telemetry tables = 12 tables at v1.

## Constitution Check

*GATE: All Constitution principles must be honored or have justified exceptions. No violations exist in this plan.*

| # | Principle | Plan compliance |
|---|---|---|
| I | API First Design (NON-NEGOTIABLE) | MCP tool schemas and HTTP endpoint contracts are spec-defined and locked. Breaking changes require MAJOR bump (FR-037, FR-095, FR-098). |
| II | Modularity with Clean Boundaries | Workspace split into ≥5 bounded crates (`mn-core`, `mn-store`, `mn-retrieval`, `mn-content`, `mn-embedding`, `mn-auth`, `mn-telemetry`, `mn-mcp`, plus two bin crates `mn-cli` and `mn-server`). No circular dependencies — enforced by Cargo. |
| III | Integration Tests Against Real Components | `testcontainers` for Postgres+pgvector; real MCP-client harness for MCP integration; canary tests against real captured logs + telemetry rows. Mocks only at network boundaries that are themselves under test. |
| IV | Frictionless Setup & Speed Are Features | `cargo install midnight-manual` + `brew install midnight-network/tap/midnight-manual` + GitHub Releases. p95 < 1 s and cold start < 500 ms gated in CI (SC-013, SC-019, SC-020). |
| V | Errors Are Human-Readable and Actionable | Typed error envelope `{error: {code, message, remediation, context}, request_id}` on every 4xx/5xx (FR-030); every code has remediation text. MCP errors mirror the envelope. |
| VI | Graceful Degradation, Fail Fast on Programmer Errors | 503+Retry-After for transient cloud-store failures (FR-035); fail-fast at startup for config / user-store / scoring-policy errors (EC-56, EC-81); FIFO telemetry-queue drop with reporting on next flush (FR-113). |
| VII | Observability First, Telemetry with Consent | Structured JSON logs from day one (FR-105); request-id propagation (FR-029, FR-106); opt-out telemetry with three mechanisms (FR-107) plus canary CI tests (FR-112, SC-061). |
| VIII | Input Validation at Every Boundary | Typed Rust values at every HTTP/MCP/file boundary; schema validation on telemetry events (FR-109); TOML schema-version checks on config/auth/user-store/scoring-policy. |
| IX | Trunk-Based Development with Continuous Release | Release-please PR-driven release pipeline (FR-096); merging the release PR triggers tag + build matrix + crates.io publish + Homebrew tap update + GHCR push + Fly deploy in one workflow. |
| X | Conventional Commits & Semantic Versioning | Conventional-Commit lint blocks merge (FR-098); strict semver; MAJOR for MCP / CLI / HTTP contract breaks (Story 10 versioning rules). |
| XI | Documentation Lives With Code | README + cookbook + rustdoc; FR-114 enforces README "Telemetry & Privacy" section at release time. |

**Result**: ✅ All principles satisfied without exception. Complexity tracking section is intentionally empty (no violations).

## Project Structure

### Documentation (this feature)

```text
specs/001-rag-platform/
├── plan.md              # This file (/sdd:plan output)
├── spec.md              # Spec (bridged from discovery/SPEC.md)
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output — concrete DDL
├── quickstart.md        # Phase 1 output — developer onboarding
├── contracts/           # Phase 1 output — OpenAPI for /v1/*
│   ├── openapi.yaml
│   └── mcp-tools.json   # JSON Schema for MCP tool inputs/outputs
└── tasks.md             # Phase 2+ (/sdd:tasks — NOT produced by /sdd:plan)
```

### Source Code (repository root)

```text
midnight-manual/
├── Cargo.toml                         # workspace root: [workspace] + shared deps
├── rust-toolchain.toml                # pins stable + components (rustfmt, clippy, sqlx-cli)
├── rustfmt.toml                       # formatting policy
├── clippy.toml                        # lints policy
├── CONSTITUTION.md                    # canonical (mirrored to .sdd/memory/)
├── README.md                          # incl. "Telemetry & Privacy" section (FR-114)
├── CLAUDE.md                          # agent context (this file's sibling)
├── crates/
│   ├── mn-core/                       # types, errors, config, scoring policy, version constants
│   │   ├── src/lib.rs
│   │   ├── src/error.rs               # typed error envelope (FR-030)
│   │   ├── src/config.rs              # D17/D18 discovery
│   │   ├── src/auth_file.rs           # auth.toml read/write (D28)
│   │   ├── src/scoring_policy.rs      # TOML loader (D24)
│   │   └── src/model_id.rs            # {name}@{revision} wire format
│   ├── mn-store/                      # sqlx + migrations
│   │   ├── src/lib.rs
│   │   ├── src/entities/              # one module per entity
│   │   ├── src/queries/               # compile-time-checked queries
│   │   └── tests/                     # integration tests (testcontainers)
│   ├── migrations/                    # numbered SQL files (sqlx migrate)
│   ├── mn-retrieval/                  # RRF, hybrid query, scoring
│   │   ├── src/lib.rs
│   │   ├── src/rrf.rs                 # FR-026, FR-088
│   │   ├── src/scoring.rs             # trust × relevance, factors (FR-076..085)
│   │   └── src/filters.rs             # filter parsing + SQL construction
│   ├── mn-content/                    # chunking + package detection
│   │   ├── src/lib.rs
│   │   ├── src/markdown.rs            # heading-based chunking + frontmatter (FR-007, FR-017)
│   │   ├── src/code/                  # tree-sitter chunkers per language (FR-008, FR-046..049)
│   │   ├── src/compact.rs             # hand-rolled module scanner (FR-047, D9)
│   │   └── src/package.rs             # Cargo.toml / package.json detection (FR-050)
│   ├── mn-embedding/                  # fastembed wrapper for embedder + reranker
│   │   ├── src/lib.rs
│   │   ├── src/embedder.rs            # bge-base-en-v1.5 (D14)
│   │   ├── src/reranker.rs            # bge-reranker-base (D2)
│   │   └── src/cache.rs               # model file management + digest verify (FR-044)
│   ├── mn-auth/                       # keys, JWT, OAuth
│   │   ├── src/lib.rs
│   │   ├── src/ed25519.rs             # keygen, sign, verify
│   │   ├── src/jwt.rs                 # HS256 mint/verify (D21)
│   │   ├── src/oauth_github.rs        # web + device flow (FR-115)
│   │   └── src/user_store.rs          # TOML loader for server-side user file (D20)
│   ├── mn-telemetry/                  # event schemas + batching client
│   │   ├── src/lib.rs
│   │   ├── src/schemas/               # one module per event_type (Story 11)
│   │   ├── src/client.rs              # batched flusher with FIFO drop (FR-113)
│   │   └── src/opt_out.rs             # three-mechanism resolver (FR-107)
│   ├── mn-mcp/                        # MCP protocol + tool implementations
│   │   ├── src/lib.rs
│   │   ├── src/transport.rs           # stdio JSON-RPC framing (FR-036)
│   │   ├── src/server.rs              # lazy model load, request loop
│   │   ├── src/tools/                 # one module per tool (search, get_chunk, …)
│   │   └── src/cookbook.rs            # tool-description "Patterns" section (FR-091)
│   ├── mn-cli/                        # bin crate: midnight-manual + mnm
│   │   ├── src/main.rs
│   │   ├── src/commands/              # one module per noun (sources, versions, ingest, …)
│   │   └── src/admin_visibility.rs    # D23 hidden-by-default machinery
│   └── mn-server/                     # bin crate: midnight-manual-server
│       ├── src/main.rs
│       ├── src/routes/                # axum handlers per endpoint
│       ├── src/sweep.rs               # periodic retention + telemetry sweep (FR-063, FR-110)
│       ├── src/rate_limit.rs          # CIDR override + tier precedence (FR-031)
│       └── src/middleware/            # request-id, body limit, CORS, etc.
├── docs/
│   ├── cookbook/
│   │   └── query-enhancement.md       # Story 7 deliverable (FR-092)
│   └── README-deploy.md               # Fly.io operational notes
├── benches/                           # criterion benchmarks for scoring + RRF
├── tests/                             # cross-crate end-to-end tests
│   ├── canary/                        # FR-112 / SC-061
│   ├── load/                          # SC-013
│   └── recall/                        # SC-014, SC-021, SC-048, SC-049
├── .github/workflows/
│   ├── ci.yml                         # PR gates: fmt, clippy, test (MSRV + stable), audit
│   ├── release.yml                    # release-please + cargo-dist + flyctl
│   └── canary.yml                     # FR-112 canary tests as a release gate
├── .sdd/                              # sdd workflow (constitution, codebase docs)
├── discovery/                         # spec-writer artifacts (preserved for trace)
├── Dockerfile.server                  # multi-stage build for mn-server (distroless base)
└── fly.toml                           # Fly.io app config
```

**Structure Decision**: Cargo workspace with **10 crates** (8 lib + 2 bin). The 8 lib crates map roughly 1:1 to the modules Constitution II names plus the two clearly-separable concerns (`mn-content` and `mn-embedding`) that each carry heavy dependencies and benefit from independent compilation. The two bin crates (`mn-cli`, `mn-server`) are the only entrypoints; they orchestrate the libs but contain minimal logic of their own. Spec stories 1–11 map onto the crates as follows:

| Story | Primary crates |
|---|---|
| 1 (content model) | `mn-store`, `mn-core` |
| 2 (Markdown ingest) | `mn-cli`, `mn-content`, `mn-embedding`, `mn-store` |
| 3 (Code ingest) | `mn-cli`, `mn-content`, `mn-embedding`, `mn-store` |
| 4 (Cloud read API) | `mn-server`, `mn-retrieval`, `mn-store` |
| 5 (Local MCP server) | `mn-cli` (subcommand) → `mn-mcp`, `mn-embedding`, `mn-retrieval` |
| 6 (Confidence scoring) | `mn-retrieval`, `mn-core` (policy loader) |
| 7 (Query enhancement) | `mn-retrieval`, `mn-mcp` (tool description), `docs/cookbook/` |
| 8 (CLI admin lifecycle) | `mn-cli`, `mn-auth` |
| 9 (Cloud auth + ops) | `mn-server`, `mn-auth`, `mn-store` |
| 10 (Distribution) | workspace `Cargo.toml`, `Dockerfile.server`, `.github/workflows/`, `fly.toml` |
| 11 (Observability) | `mn-telemetry`, `mn-server` (logging + /metrics), `mn-cli` (logging) |

## Complexity Tracking

*No constitutional violations to justify.*

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| *None* | *None* | *None* |

# Phase 0: Research — midnight-manual v1

**Feature**: 001-rag-platform | **Date**: 2026-05-13

Most of the strategic research for this feature was performed during spec authoring and is captured in `discovery/archive/RESEARCH.md` (R1–R4) and `discovery/archive/DECISIONS.md` (D1–D28). This document records the **implementation-layer** decisions that follow once strategic direction is fixed — primarily crate selection and version pins for dependencies whose ecosystems shift faster than the spec.

## Inherited research (spec-level)

| Ref | Topic | Outcome |
|---|---|---|
| R1 / D1 / D14 | Local embedding library | `fastembed-rs` with `bge-base-en-v1.5` (768 dims). Family-aligned with the reranker; ~70 ms CPU embed on a laptop; ~450 MB RSS loaded. |
| R2 / D2 | Cross-encoder reranking | `bge-reranker-base` via `fastembed-rs` (same crate as the embedder). Runs in the local MCP server only. |
| R3 / D3 | Query rewriting | Caller-delegated. Server accepts `queries: string[]`; no LLM dependency on the server side. |
| R4 / D4 | Hybrid retrieval | Parallel Postgres FTS + pgvector, RRF (k=60) merged in app code. |
| D10 / D21 / D28 | Auth | Ed25519 challenge-response for admin (1h HS256 JWT); GitHub OAuth for read uplift (30d bearer); file-based `auth.toml` (chmod 0600), no keychain in v1. |
| D11 / D25 | Read auth tiering | Anonymous + SSO + CIDR override; multi-query costs `max(1, N)` tokens. |
| D24 | Confidence scoring | Trust × relevance weighted geometric-mean blend with TOML policy. |
| D27 | Telemetry | Self-hosted on the cloud server; 7-day raw + indefinite aggregates. |

No NEEDS CLARIFICATION items remain from spec discovery.

## Phase-0 (implementation-layer) decisions

### R-1. HTTP server framework

**Decision**: `axum` v0.7+.

**Rationale**: Tower ecosystem (rate-limit middleware, request-id propagation, body-size limit, CORS — all already-built tower layers); large active maintainer base; integrates cleanly with `sqlx` (both shared-runtime-friendly); typed error responses via custom `IntoResponse` impls. Stable since axum 0.6, breaking changes have been measured.

**Alternatives considered**:
- `actix-web` — own runtime model (actor system) doesn't compose with `tokio::sync::OnceCell` for lazy model loading as cleanly; smaller middleware ecosystem.
- `poem` — younger; OpenAPI integration (`poem-openapi`) is nice but `utoipa` works with axum and is more mature.
- `rocket` — slow async story; less idiomatic for service-shaped code.

### R-2. MCP transport / protocol implementation

**Decision**: Hand-roll a thin JSON-RPC framing layer in `mn-mcp` over `tokio::io::stdin`/`stdout`. The MCP protocol surface is small (initialize, list_tools, call_tool, list_resources, notification). Adopt the MCP spec types from `serde_json::Value`-typed structs that mirror the official schema.

**Rationale**: Rust MCP SDKs in May 2026 are still moving fast (rmcp, mcp-rs, others) and contracts churn between minor versions. Hand-rolled JSON-RPC framing for our 7-tool surface is ~300 LOC; pinning to an unstable external SDK would be a larger maintenance liability than writing the framing ourselves.

**Alternatives considered**:
- `rmcp` — official-ish Anthropic-published Rust MCP crate; latest minor releases still show breaking changes; revisit at v0.5+.
- `mcp-rs` — community crate; less active.

**Revisit**: Once an official Rust SDK reaches 1.0 and stabilizes, migrate to it (PATCH or MINOR release since the MCP wire format is stable; our `mn-mcp` crate is the internal boundary).

### R-3. JSON Schema validation (telemetry boundary, FR-109)

**Decision**: `jsonschema` crate (v0.17+).

**Rationale**: Pure-Rust, no native deps, supports JSON Schema Draft 2020-12. We need it only at one boundary (`POST /v1/telemetry` payload validation) — light usage, no perf concerns.

**Alternatives considered**:
- Hand-rolled validators per event_type — fewer deps but rejects the FR-109 promise of "schema is the source of truth," because schema files would be Rust-code adjacent rather than schema-language adjacent.
- `valico` — older, less maintained.

### R-4. Structured logging

**Decision**: `tracing` + `tracing-subscriber` configured with `tracing-subscriber::fmt::layer().json()` (built-in JSON formatter).

**Rationale**: Built-in JSON output is sufficient for our log line shape (FR-105). Avoiding `tracing-bunyan-formatter` keeps the dependency surface small. `tracing` is the de facto standard for Rust async services and integrates with `axum::middleware::from_fn` for request-id propagation.

**Alternatives considered**:
- `slog` — equally capable but smaller ecosystem in 2026.
- `bunyan-formatter` — extra dep with no functional gain over the built-in JSON layer.

### R-5. Prometheus metrics

**Decision**: `metrics` + `metrics-exporter-prometheus`.

**Rationale**: Decoupled metrics-API surface (every crate can emit via `metrics::counter!` without depending on Prometheus); exporter is swappable; supports the seven series enumerated in FR-111.

**Alternatives considered**:
- `prometheus` crate — direct, but couples emitter sites to Prometheus types.

### R-6. GitHub OAuth

**Decision**: `oauth2` crate (v4+) for the protocol, server-driven flow with `octocrab` to verify the resulting GitHub token's org membership.

**Rationale**: `oauth2` is the standard for the protocol layer; `octocrab` provides typed GitHub API access for the org-membership check (FR-062). Both crates are well-maintained.

**Alternatives considered**:
- Hand-rolled OAuth — error-prone PKCE handling; reject.

### R-7. OpenAPI generation

**Decision**: `utoipa` for axum integration; OpenAPI YAML emitted to `specs/001-rag-platform/contracts/openapi.yaml` as part of the build (or `cargo run --bin gen-openapi`).

**Rationale**: Annotations on the axum handlers produce a single source of truth for the API contract, matching FR-030's typed error envelope. Aligns with Constitution I (API First Design).

**Alternatives considered**:
- Hand-maintained OpenAPI YAML — drifts from code.
- `aide` — comparable; less momentum than utoipa.

### R-8. Markdown frontmatter

**Decision**: `serde_yaml` for frontmatter parsing (yaml-rust2 backend).

**Rationale**: Markdown frontmatter is by convention YAML-formatted; `serde_yaml` integrates with our existing `serde` typing. The deprecated `yaml-rust` crate is not used; `serde_yaml`'s current backend is `yaml-rust2`.

**Alternatives considered**:
- TOML frontmatter — uncommon in docs; not used in the midnight-docs corpus.

### R-9. Markdown chunking

**Decision**: `pulldown-cmark` for parsing; chunk emission walks the event stream and splits on heading boundaries (FR-007).

**Rationale**: Standard CommonMark parser, fast, no dependencies on markdown rendering. We don't need rendering, only structural parsing.

### R-10. Tree-sitter integration

**Decision**: `tree-sitter` runtime crate + per-language grammar crates: `tree-sitter-rust`, `tree-sitter-typescript`, `tree-sitter-javascript`. Compact handled by hand-rolled module scanner (FR-047) until a grammar crate exists.

**Rationale**: Standard pattern. Each grammar crate vendors C source; built once at compile time via `cc-rs`.

**Watching list note**: a tree-sitter-compact grammar is on the spec watch list (Story 3 revision risk). When available, swap in.

### R-11. Postgres + pgvector client

**Decision**: `sqlx` (postgres feature) with the `pgvector` crate's `sqlx-postgres` integration.

**Rationale**: Compile-time-checked queries (`sqlx::query_as!`); first-class async; the `pgvector` crate provides the `Vector` newtype with `Encode`/`Decode` for `sqlx`. Migration tooling via `sqlx migrate` (D22).

**Alternatives considered**:
- `diesel-async` — better ORM ergonomics but no compile-time query check that catches schema drift at build.
- `tokio-postgres` direct — loses migrations and the compile-time query check.

### R-12. Testcontainers for integration tests

**Decision**: `testcontainers` crate (Rust client) with a custom image built from `pgvector/pgvector:pg16`.

**Rationale**: Constitution III demands real components. `testcontainers` provides Docker-managed Postgres+pgvector during `cargo test --features integration`. The custom image avoids the cold-start cost of installing pgvector inside the test container.

### R-13. CLI framework

**Decision**: `clap` v4 with derive macros; a shared `GlobalOpts` struct via `#[command(flatten)]` to satisfy D17 (every command honors `--json`, `--quiet`, `--server`, `--config`, `--log-level`, `--no-color`).

**Rationale**: Standard. Derive macros keep command definitions close to handler code; `clap`'s subcommand support handles the noun-first tree (D19); `hide(true)` attribute drives D23 admin-visibility toggling at runtime via a custom completer.

### R-14. Release tooling

**Decision**:
- **release-please** GitHub Action — Conventional-Commit → version bump + CHANGELOG (FR-096).
- **cargo-dist** — cross-compiled binary matrix, GitHub Release upload with SHA256SUMS (FR-099, FR-100).
- **homebrew-releaser** Action — push formula update to the tap repo.
- **docker buildx** — multi-arch server image to GHCR (FR-102).
- **flyctl** — deploy step, gated on image push success.

**Rationale**: Each tool is the de facto standard for its niche in the Rust ecosystem as of May 2026. None requires custom plumbing.

**Alternatives considered**:
- `cargo-release` — overlaps with `cargo-dist` for the publish step; `cargo-dist` does the binary matrix that `cargo-release` does not.

### R-15. MSRV

**Decision**: `1.83.0`, pinned in `Cargo.toml` `rust-version = "1.83"`.

**Rationale**: Stable since November 2024; carries the `async fn in traits` features we rely on (sqlx + tracing) without requiring nightly; one minor behind the latest stable at time of writing (1.84 expected Jan 2026), giving downstream consumers a small grace window.

**Bump policy**: MSRV bumps are MINOR releases (Cargo convention; FR-097).

## Patterns and conventions from previous projects

*No `specs/*/retro/*.md` exists yet (this is the first feature). Future planning runs will pull from the retro store automatically.*

## Open implementation questions

| # | Question | Resolution timing |
|---|---|---|
| OQ-1 | Should the auth file path be configurable beyond `$XDG_CONFIG_HOME`, or do we accept a single default plus `--config`? | Pre-MVP: ship `--config` only; revisit if user feedback warrants. |
| OQ-2 | Postgres extension availability on Fly Postgres for `pg_trgm` (trigram fuzzy match, future enhancement)? | Out of scope for v1; native tsvector + pgvector is sufficient. |
| OQ-3 | Multi-region deploy posture? | Out of scope for v1 per Story 9; document the path in Story 10's release notes. |

## Output

This research informs:

- **Technical Context** in `plan.md` (already populated).
- **Crate dependencies** in `Cargo.toml` workspace manifest (produced in Phase 2).
- **CI tooling configuration** in `.github/workflows/` (produced in Phase 2).

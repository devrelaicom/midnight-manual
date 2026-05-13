---
description: "Task list for midnight-manual v1 (001-rag-platform)"
---

# Tasks: midnight-manual v1 (rag-platform)

**Input**: Design documents from `/specs/001-rag-platform/`
**Prerequisites**: plan.md ✓, spec.md ✓, research.md ✓, data-model.md ✓, contracts/ ✓

**Tests**: Test tasks ARE included — the spec mandates unit + integration (`testcontainers`) + property + canary suites, with CI gates on every PR (Constitution III, FR-112, SC-061).

**Organization**: Tasks are grouped by user story. P1 stories deliver the core retrieval loop (US1 schema → US2 md ingest → US4 read API → US5 MCP server → US3 code ingest). P2 stories add auth, admin, scoring, and multi-query. P3 stories add distribution and telemetry.

## Format

`- [ ] [TaskID] [P?] [Story?|GIT] Description with file path`

- **[P]** — parallelizable (different files, no incomplete-task dependencies)
- **[USn]** — maps the task to user story n (only present in story phases)
- **[GIT]** — git workflow task (no story label)
- All Rust implementation tasks use `devs:rust-dev` agent unless otherwise noted

## Stack / agent routing

| Tech | Agent |
|---|---|
| Rust source under `crates/**` | `devs:rust-dev` |
| `*.sql` migrations under `crates/mn-store/migrations/` | `devs:rust-dev` (writes raw SQL alongside Rust) |
| `.github/workflows/*.yml` | `dev-specialisms:init-local-tooling` |
| `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml` | `dev-specialisms:init-local-tooling` |
| `Dockerfile.server`, `fly.toml` | `dev-specialisms:fly-deploy` |
| `.npmrc`, generic shell/config | `dev-specialisms:init-local-tooling` |

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Cargo workspace + lints + base CI scaffolding. Greenfield commit `b3464c2` already contains a partial scaffold; this phase fills in the gaps and lands a clean check.

### Phase 1 git entry

- [ ] T001 [GIT] Verify on branch `001-rag-platform` and working tree is clean (`git branch --show-current` and `git status --porcelain`)
- [ ] T002 [GIT] Confirm `main` is reachable and current branch tracks no stale remote (`git fetch origin`)

### Phase 1 implementation

- [ ] T003 [P] Verify / write workspace root `Cargo.toml` with `[workspace]` listing all 10 member crates and pinned `rust-version = "1.83"` (use dev-specialisms:init-local-tooling skill)
- [ ] T004 [P] Verify / write `rust-toolchain.toml` pinning channel `stable` with components `rustfmt`, `clippy`, `rust-src` (use dev-specialisms:init-local-tooling skill)
- [ ] T005 [P] Write `rustfmt.toml` and `clippy.toml` per Constitution II / repo style (use dev-specialisms:init-local-tooling skill)
- [ ] T006 [P] Create empty crate skeletons under `crates/` for `mn-core`, `mn-store`, `mn-retrieval`, `mn-content`, `mn-embedding`, `mn-auth`, `mn-telemetry`, `mn-mcp`, `mn-cli`, `mn-server` — each with a stub `src/lib.rs` (or `src/main.rs` for bin crates) and a 1-line `README.md` (use devs:rust-dev agent)
- [ ] T007 [P] Add workspace-shared deps to root `Cargo.toml` `[workspace.dependencies]`: axum, tokio, tower, tower-http, sqlx, pgvector, fastembed, tree-sitter, tree-sitter-rust, tree-sitter-typescript, tree-sitter-javascript, jsonwebtoken, ed25519-dalek, oauth2, octocrab, clap, serde, serde_json, toml, jsonschema, tracing, tracing-subscriber, metrics, metrics-exporter-prometheus, prometheus, pulldown-cmark, tokio-util, anyhow, thiserror, uuid, time, reqwest, hyper, http, utoipa, testcontainers, proptest, criterion — version-pinned per `research.md` (use devs:rust-dev agent)
- [ ] T008 Add `[workspace.metadata.dist]` skeleton (cargo-dist v0 config) so later release work has a place to plug in (use dev-specialisms:init-local-tooling skill)
- [ ] T009 Add `.cargo/config.toml` with `[build]` defaults and `[target.<host>]` cfg for native CPU build at release time (use dev-specialisms:init-local-tooling skill)
- [ ] T010 [P] Write `.github/workflows/ci.yml` with jobs: `fmt`, `clippy` (`-D warnings`), `test` (stable + MSRV), `audit` (cargo-audit), `sqlx-prepare check`. Per-job concurrency and OS matrix (ubuntu-latest at minimum) (use dev-specialisms:init-local-tooling skill)
- [ ] T011 [P] Write `.github/workflows/canary.yml` placeholder pointing at `tests/canary` (filled in by Phase 12) (use dev-specialisms:init-local-tooling skill)
- [ ] T012 [P] Write `.github/workflows/release.yml` placeholder for `release-please` + `cargo-dist` (filled in by Phase 11) (use dev-specialisms:init-local-tooling skill)
- [ ] T013 [P] Write top-level `README.md` skeleton with required `"Telemetry & Privacy"` H2 (FR-114) — content fleshed out in Phase 12, but the section header must already exist so CI can grep for it
- [ ] T014 [P] Write `justfile` (or `Makefile`) with targets `check`, `check-msrv`, `test`, `test-integration`, `test-canary`, `bench` matching CLAUDE.md (use dev-specialisms:init-local-tooling skill)
- [ ] T015 [P] Add `.gitignore` lines for `target/`, `*.db`, `.env`, `models/`, `.sqlx/`, `result/` (use dev-specialisms:init-local-tooling skill)
- [ ] T016 [P] Add an `.npmrc` at repo root (Supply-chain hardening per session-start guidance) — declines custom registries explicitly to keep `@midnight-ntwrk/*` on public npm. NOTE: there is no JS in this project, so this file mainly serves as a hardening signal for downstream contributors
- [ ] T017 Run `cargo check --workspace` and `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` and confirm clean exit
- [ ] T018 [GIT] Commit: scaffold workspace, lints, CI placeholders

**Checkpoint**: Workspace builds clean; CI runs (most jobs no-op against empty crates).

---

## Phase 2: Foundational + User Story 1 — Content model & metadata schema (Priority: P1) 🎯 MVP foundation

**Goal**: Land the v1 database schema, the typed error envelope, config discovery, model-id wire format, and the `mn-core` + `mn-store` library surface so every later story has unambiguous shapes to build against. Also covers US1's acceptance scenarios end-to-end.

**Independent Test**: `cargo test --workspace --features integration` boots an ephemeral Postgres+pgvector via testcontainers, runs every migration, and round-trips `source`, `source_version`, `embedding_model`, `node`, `document`, `package`, `chunk` rows including JSONB provenance and the active-version partial unique index.

**⚠️ CRITICAL**: No US2..US11 work can begin until this phase is complete.

### Phase 2 git entry

- [ ] T019 [GIT] Verify working tree is clean before starting Phase 2
- [ ] T020 [US1] Create `retro/P2.md` from the retro template

### mn-core (types, errors, config, model-id, scoring-policy loader stubs)

- [ ] T021 [P] [US1] Define typed error envelope `Error { code, message, remediation, context }` and `ApiError` -> `IntoResponse` impl in `crates/mn-core/src/error.rs` (FR-030, Constitution V) (use devs:rust-dev agent)
- [ ] T022 [P] [US1] Implement config-file discovery (XDG + `--config` override) per D17/D18 in `crates/mn-core/src/config.rs` (use devs:rust-dev agent)
- [ ] T023 [P] [US1] Define `EmbeddingModelId` (`{name}@{revision}` wire format) with serde + Display + FromStr in `crates/mn-core/src/model_id.rs` (use devs:rust-dev agent)
- [ ] T024 [P] [US1] Define the canonical chunk/document/source/node/package/source_version Rust types in `crates/mn-core/src/types.rs` (these are the wire shapes returned by US4's API and used by US2/US3 ingest) (use devs:rust-dev agent)
- [ ] T025 [P] [US1] Define `Provenance` struct (`attribution`, `verified`, `verified_by`, `verified_at`, `deprecation`, `language_targets`, etc.) with serde (round-trips JSONB column from US1 schema) in `crates/mn-core/src/provenance.rs` (use devs:rust-dev agent)
- [ ] T026 [P] [US1] Stub `crates/mn-core/src/auth_file.rs` (auth.toml schema_version=1 with `[admin]` and `[read_uplift]` sections, chmod 0600 writer) — read path only in this phase; `write`/`mutate` come in Phase 7 (use devs:rust-dev agent)
- [ ] T027 [P] [US1] Stub `crates/mn-core/src/scoring_policy.rs` with the TOML loader struct (full validation lands in Phase 9 / US6); for now expose compiled-in defaults (use devs:rust-dev agent)
- [ ] T028 [US1] Unit tests for `mn-core` types: provenance round-trip, model-id parse/display, config discovery precedence (env > flag > file), error envelope shape stability in `crates/mn-core/tests/` (use devs:rust-dev agent)
- [ ] T029 [GIT] Commit: mn-core foundational types, error envelope, config discovery

### mn-store schema + migrations

- [ ] T030 [P] [US1] Migration `0001_extensions.sql` — `CREATE EXTENSION IF NOT EXISTS vector;` in `crates/mn-store/migrations/0001_extensions.sql`
- [ ] T031 [US1] Migration `0002_corpus_schema.sql` — exact DDL from `data-model.md` §0002: source, embedding_model, source_version (+partial unique active index + check), node, package, document, chunk (+`status` enum + `embedding vector(768)` + tsvector generated column) (use devs:rust-dev agent)
- [ ] T032 [US1] Migration `0003_corpus_indexes.sql` — HNSW on chunk.embedding (per data-model.md), GIN on chunk.tsvector, btree on FKs, `(source_version_id, status)` composite for sweep (use devs:rust-dev agent)
- [ ] T033 [US1] Migration `0004_admin_schema.sql` — rate_limit_override + reserved `user`, `api_key` tables (use devs:rust-dev agent)
- [ ] T034 [US1] Migration `0005_telemetry_schema.sql` — telemetry_event_raw, telemetry_aggregate_daily (use devs:rust-dev agent)
- [ ] T035 [US1] Migration `0006_seed_embedding_model.sql` — INSERT (idempotent) `bge-base-en-v1.5@1` row (use devs:rust-dev agent)
- [ ] T036 [GIT] Commit: numbered migrations 0001–0006 with corpus + admin + telemetry schema

### mn-store entity modules + queries

- [ ] T037 [P] [US1] `crates/mn-store/src/entities/source.rs` — typed insert/get/list/retire helpers with `sqlx::query!` macros (use devs:rust-dev agent)
- [ ] T038 [P] [US1] `crates/mn-store/src/entities/source_version.rs` — including atomic `finalize` (start a tx, mark new active, demote old) per EC-04 (use devs:rust-dev agent)
- [ ] T039 [P] [US1] `crates/mn-store/src/entities/embedding_model.rs` — get/create/find_active (use devs:rust-dev agent)
- [ ] T040 [P] [US1] `crates/mn-store/src/entities/node.rs` — insert, recursive `parent_chain(node_id)` CTE (powers US4's `parent_chain` response field) (use devs:rust-dev agent)
- [ ] T041 [P] [US1] `crates/mn-store/src/entities/document.rs` — insert (with provenance JSONB), get-by-id, siblings query (use devs:rust-dev agent)
- [ ] T042 [P] [US1] `crates/mn-store/src/entities/chunk.rs` — insert (incl. embedding bytes), get-by-id, siblings, parents, with `status` filter for embed_failed exclusion (EC-03) (use devs:rust-dev agent)
- [ ] T043 [P] [US1] `crates/mn-store/src/entities/package.rs` — insert / list by source_version (use devs:rust-dev agent)
- [ ] T044 [US1] Pool builder in `crates/mn-store/src/pool.rs` (PgPool, statement-cache, sqlx-migrate runner gated on env var) (use devs:rust-dev agent)
- [ ] T045 [US1] `cargo sqlx prepare --workspace` integration: add `.sqlx/` to CI; ensure offline queries pass without a live DB (use devs:rust-dev agent)

### Integration tests (Constitution III)

- [ ] T046 [P] [US1] testcontainers harness in `crates/mn-store/tests/integration/common.rs` — boots Postgres 16 with pgvector image, runs migrations on every test (use devs:rust-dev agent)
- [ ] T047 [P] [US1] Integration test in `crates/mn-store/tests/integration/source_version_lifecycle.rs` — covers EC-04 (cannot have two active versions), retention=5 promotion/demotion, building→active flip (use devs:rust-dev agent)
- [ ] T048 [P] [US1] Integration test `parent_chain.rs` — multi-level node tree, query returns ordered chain from chunk node to source root (US1 acceptance #2) (use devs:rust-dev agent)
- [ ] T049 [P] [US1] Integration test `embed_failed_exclusion.rs` — chunk with `status='embed_failed'` is excluded from read queries but listable by admin (US1 acceptance #9, EC-03) (use devs:rust-dev agent)
- [ ] T050 [P] [US1] Integration test `cross_version_embedding_model.rs` — same source can have versions encoded with different `embedding_model_id` (EC-10) (use devs:rust-dev agent)
- [ ] T051 [P] [US1] Property test in `crates/mn-core/tests/proptest_provenance.rs` (`proptest`) — provenance JSON round-trips for arbitrary valid shapes (SC-051 family) (use devs:rust-dev agent)
- [ ] T052 [US1] Wire `--features integration` to gate the testcontainers tests; document in `justfile`
- [ ] T053 [GIT] Commit: mn-store entity modules, queries, and integration tests

### Phase 2 close-out

- [ ] T054 [US1] Run `/sdd:map incremental` to update `.sdd/codebase/STACK.md`, `STRUCTURE.md`, `ARCHITECTURE.md`, `TESTING.md` with the Phase 2 surface
- [ ] T055 [US1] Review `retro/P2.md` and conservatively extract any project-wide learnings to `CLAUDE.md`
- [ ] T056 [GIT] Commit: codebase mapping + retro for Phase 2
- [ ] T057 [GIT] Push branch to origin (`git push -u origin 001-rag-platform`) ensuring pre-push hooks pass
- [ ] T058 [GIT] Create PR to `main` titled `feat(rag-platform): Phase 2 foundational schema + mn-core/mn-store` with phase summary
- [ ] T059 [GIT] Verify all CI checks pass (`gh pr checks <pr-number>`)
- [ ] T060 [GIT] Report PR ready status — output the `**PR #<n> READY FOR MERGE. AWAITING LGTM**` block and STOP

**Checkpoint**: Schema + types are stable. US2..US11 may now begin.

---

## Phase 3: User Story 2 — Admin ingestion of Markdown content via CLI (Priority: P1)

**Goal**: `mnm ingest md <slug> <path>` chunks Markdown locally, embeds with `bge-base-en-v1.5`, uploads to the cloud over the implicit write protocol, and atomically promotes a new `source_version`. Also lands the minimum server-side write endpoints (anonymous-mode; full auth in Phase 7) needed to make this story end-to-end testable.

**Independent Test**: Against an integration-test cloud server (anonymous-write mode for tests), `mnm ingest md midnight-docs ./fixtures/md-tree` creates a new source_version, populates documents/chunks, promotes it active, and a follow-up `mnm sources show midnight-docs` reports the new active revision.

### Phase 3 git entry

- [ ] T061 [GIT] Verify working tree is clean before starting Phase 3
- [ ] T062 [US2] Create `retro/P3.md` from retro template

### mn-content — Markdown chunker + frontmatter

- [ ] T063 [P] [US2] Markdown parser using `pulldown-cmark` in `crates/mn-content/src/markdown.rs` — heading-based chunking (per-heading is one chunk; configurable max tokens) (use devs:rust-dev agent)
- [ ] T064 [P] [US2] Frontmatter parser (YAML) extracting `verified`, `verified_by`, `verified_at`, `language_targets`, `deprecation` into `Provenance` (FR-017, US1 acceptance #8) (use devs:rust-dev agent)
- [ ] T065 [P] [US2] Fixed-window fallback chunker for heading-less Markdown (default 800 tokens, 100-token overlap, EC-07) in `crates/mn-content/src/window.rs` (use devs:rust-dev agent)
- [ ] T066 [P] [US2] Manifest (`hierarchy.yaml`) loader in `crates/mn-content/src/manifest.rs` — schema validation, file existence preflight (EC-13), duplicate-parent detection (EC-14) (use devs:rust-dev agent)
- [ ] T067 [P] [US2] `content_hash` computation (SHA-256 of normalized content) per document (FR-014) (use devs:rust-dev agent)
- [ ] T068 [US2] Unit tests for Markdown chunking (heading boundaries, code-block preservation, frontmatter strip), fallback chunker, manifest validator
- [ ] T069 [GIT] Commit: mn-content Markdown chunker, frontmatter parser, manifest loader

### mn-embedding — fastembed wrapper (embedder only — reranker comes in Phase 5)

- [ ] T070 [P] [US2] `crates/mn-embedding/src/embedder.rs` wrapping `fastembed::TextEmbedding` for `bge-base-en-v1.5` (768 dims, batch encode, returns `Vec<f32>` per input) (use devs:rust-dev agent)
- [ ] T071 [P] [US2] Model cache directory resolver (`$XDG_DATA_HOME/midnight-manual/models/`) and digest verifier (FR-044) in `crates/mn-embedding/src/cache.rs` (use devs:rust-dev agent)
- [ ] T072 [P] [US2] Lazy single-shot load guard (`tokio::sync::OnceCell`) so the CLI ingest and the MCP server share the same pattern (use devs:rust-dev agent)
- [ ] T073 [US2] Integration test `crates/mn-embedding/tests/embed_smoke.rs` — encodes 5 strings and asserts 768-dim outputs (gated on `--features integration` to avoid network in unit CI) (use devs:rust-dev agent)
- [ ] T074 [GIT] Commit: mn-embedding embedder + model cache

### mn-server — minimum write API for ingest (anonymous mode; full auth in Phase 7)

- [ ] T075 [P] [US2] Set up the `mn-server` binary skeleton: `axum` Router, `tracing` JSON log layer, `tower_http::trace`, `X-Request-Id` middleware, graceful shutdown on SIGTERM in `crates/mn-server/src/main.rs` + `crates/mn-server/src/app.rs` (use devs:rust-dev agent)
- [ ] T076 [P] [US2] Route `POST /v1/sources/{slug}/ingest-runs` — creates source_version row in `building` state, returns `{ingest_run_id, source_version_id, source_version_revision}` (use devs:rust-dev agent)
- [ ] T077 [P] [US2] Route `PUT /v1/sources/{slug}/ingest-runs/{id}/documents` — idempotent batch upload of `{document, chunks}` keyed on content hashes (use devs:rust-dev agent)
- [ ] T078 [P] [US2] Route `POST /v1/sources/{slug}/ingest-runs/{id}/finalize` — atomic flip to active in a single tx, demotes prior active (US2 acceptance #1; reuses mn-store::source_version::finalize from T038) (use devs:rust-dev agent)
- [ ] T079 [P] [US2] Route `POST /v1/sources/{slug}/ingest-runs/{id}/abort` — marks source_version `aborted` (use devs:rust-dev agent)
- [ ] T080 [P] [US2] Route `POST /v1/sources` — create a source (anonymous-mode for now; auth wrapper added in Phase 7) (use devs:rust-dev agent)
- [ ] T081 [US2] Route `GET /v1/sources` and `GET /v1/sources/{slug}` — list + show (use devs:rust-dev agent)
- [ ] T082 [US2] Route `GET /v1/models/active` — returns active embedding model identifier per FR-039 / Story 4 acceptance #12 (use devs:rust-dev agent)
- [ ] T083 [US2] Route `GET /healthz` (liveness) and `GET /readyz` (DB ping + pgvector extension check) (use devs:rust-dev agent)
- [ ] T084 [US2] Wire the `mn-server` to load `DATABASE_URL`, run `sqlx migrate` (unless `MIDNIGHT_MANUAL_AUTO_MIGRATE=false`, D22), seed `embedding_model` if absent (use devs:rust-dev agent)
- [ ] T085 [US2] Integration test in `crates/mn-server/tests/ingest_lifecycle.rs` — creates a source, starts a run, uploads two documents (one new, one unchanged-hash so embedding bytes are carried forward per FR-014 / US2 acceptance #4), finalizes, asserts active flip (use devs:rust-dev agent)
- [ ] T086 [GIT] Commit: mn-server skeleton + ingest write endpoints

### mn-cli — `mnm` binary skeleton + ingest md command

- [ ] T087 [P] [US2] CLI skeleton: clap v4 derive with global flags `--config`, `--server`, `--token`, `--json`, `--log-level`, `--no-telemetry`; aliases `midnight-manual` and `mnm` per D16 in `crates/mn-cli/src/main.rs` and `crates/mn-cli/src/cli.rs` (use devs:rust-dev agent)
- [ ] T088 [P] [US2] Admin-visibility machinery (D23): commands tagged `Visibility::Hidden` are filtered from `--help` unless `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` or `cli.show_admin_cmds=true` in `crates/mn-cli/src/admin_visibility.rs`. Invocation is never gated. (use devs:rust-dev agent)
- [ ] T089 [P] [US2] `mnm sources add` (hidden) / `list` / `show` commands in `crates/mn-cli/src/commands/sources.rs` (use devs:rust-dev agent)
- [ ] T090 [P] [US2] `mnm versions list` / `show` commands in `crates/mn-cli/src/commands/versions.rs` (use devs:rust-dev agent)
- [ ] T091 [US2] `mnm ingest md <slug> <path>` command in `crates/mn-cli/src/commands/ingest_md.rs`: walks the path, applies optional manifest, chunks, embeds locally with mn-embedding, calls server's ingest-runs endpoints with idempotent retries, prints summary or NDJSON per `--json` (use devs:rust-dev agent)
- [ ] T092 [US2] Flags on `ingest md`: `--manifest`, `--strict-manifest`, `--source-url-prefix`, `--published-url-prefix`, `--max-file-size`, `--on-frontmatter-error {continue,skip}`, `--strict`, `--dry-run`, `--force-new`, `--embedding-model`, `--batch-size` (use devs:rust-dev agent)
- [ ] T093 [US2] Resumable upload semantics — on rerun, the CLI fetches the in-progress source_version state from the server and uploads only missing chunks (US2 acceptance #6) (use devs:rust-dev agent)
- [ ] T094 [US2] Model-mismatch preflight: the CLI inspects the source's active embedding model via `GET /v1/models/active` and refuses to start (with remediation pointing at `mnm models pull`) when the local model doesn't match (US2 acceptance #8) (use devs:rust-dev agent)
- [ ] T095 [US2] CLI integration test in `crates/mn-cli/tests/ingest_md_e2e.rs` — boots a testcontainers Postgres + an in-process axum app, invokes `mnm ingest md` against a small fixture tree (`crates/mn-cli/tests/fixtures/md-small/`), asserts active version + chunk counts (use devs:rust-dev agent)
- [ ] T096 [GIT] Commit: `mnm ingest md` command + flags + resumable upload + tests

### Phase 3 close-out

- [ ] T097 [US2] Run `/sdd:map incremental` to update codebase docs with mn-content / mn-embedding / mn-server / mn-cli surface
- [ ] T098 [US2] Review `retro/P3.md` and extract any project-wide learnings to `CLAUDE.md`
- [ ] T099 [GIT] Commit: codebase mapping + retro for Phase 3
- [ ] T100 [GIT] Push branch and update PR body with Phase 3 summary
- [ ] T101 [GIT] Verify all CI checks pass
- [ ] T102 [GIT] Report PR ready status — STOP and await LGTM

**Checkpoint**: A maintainer can ingest a directory of Markdown files into the corpus. No read API yet.

---

## Phase 4: User Story 4 — Cloud read API: hybrid (FTS + vector) search (Priority: P1)

**Goal**: `POST /v1/search` returns ranked chunks via parallel FTS + pgvector with RRF (k=60), plus all the chunk/document/node read endpoints, model-mismatch 409 handling, per-IP/per-user rate limits with CIDR overrides, and the typed error envelope on every status code.

**Independent Test**: With an ingested fixture corpus, `POST /v1/search` with a known-good query returns chunks in RRF order with full metadata. A request carrying `client_embedding_model = "bge-small-en-v1.5@1"` gets 409 + remediation. Per-IP rate limit returns 429 + `Retry-After` after the configured burst.

### Phase 4 git entry

- [ ] T103 [GIT] Verify working tree is clean before starting Phase 4
- [ ] T104 [US4] Create `retro/P4.md` from retro template

### mn-retrieval — hybrid query construction, RRF, filters

- [ ] T105 [P] [US4] FTS query builder in `crates/mn-retrieval/src/fts.rs` — Postgres `tsvector` + `ts_rank_cd`, language config (`english`) (use devs:rust-dev agent)
- [ ] T106 [P] [US4] Vector query builder in `crates/mn-retrieval/src/vector.rs` — pgvector ANN with HNSW, cosine distance, `LIMIT` capped at `top_k_per_mode` (default 100) (use devs:rust-dev agent)
- [ ] T107 [P] [US4] RRF merger in `crates/mn-retrieval/src/rrf.rs` — k=60, merges across modes AND across multi-query pairs in one pass per Story 7 (FR-026, FR-088) (use devs:rust-dev agent)
- [ ] T108 [P] [US4] Filter parser + SQL construction in `crates/mn-retrieval/src/filters.rs` — `attribution`, `verified`, `content_type`, `source_slug`, `language_target`, `sdk_dependency`, `package` (US4 acceptance #11) (use devs:rust-dev agent)
- [ ] T109 [P] [US4] Property test `crates/mn-retrieval/tests/proptest_rrf.rs` — RRF is order-independent within ties, monotonic in rank, deterministic given the same inputs (SC-047) (use devs:rust-dev agent)
- [ ] T110 [US4] Integration test `crates/mn-retrieval/tests/hybrid_recall.rs` against testcontainers Postgres — asserts hybrid recall@10 lift ≥ 10pp over single-mode on a small labelled fixture (SC-014) (use devs:rust-dev agent)
- [ ] T111 [GIT] Commit: mn-retrieval FTS + vector + RRF + filters

### mn-server — read endpoints

- [ ] T112 [P] [US4] Route `POST /v1/search` accepting `{queries:[{text, vector}], client_embedding_model, filters, limit, sort_by, min_confidence, source_version_revision}` — returns chunks with full chunk/document/source/parent_chain/navigation/scores (use devs:rust-dev agent)
- [ ] T113 [P] [US4] Route `GET /v1/chunks/{id}` — returns chunk + full metadata (use devs:rust-dev agent)
- [ ] T114 [P] [US4] Route `GET /v1/chunks/{id}/siblings` — chunks of the same document in `chunk_index` order (US4 acceptance #4) (use devs:rust-dev agent)
- [ ] T115 [P] [US4] Route `GET /v1/chunks/{id}/parents` — parent chain from chunk node to source-version root (uses CTE from T040) (use devs:rust-dev agent)
- [ ] T116 [P] [US4] Route `GET /v1/documents/{id}` and `GET /v1/documents/{id}/chunks` (use devs:rust-dev agent)
- [ ] T117 [P] [US4] Route `GET /v1/nodes/{id}` and `GET /v1/nodes/{id}/children` (use devs:rust-dev agent)
- [ ] T118 [US4] Model-mismatch enforcement: every search-or-chunk endpoint inspects `client_embedding_model`; mismatch returns HTTP 409 with `{error: {code: "embedding_model_mismatch", remediation, context: {corpus_model, client_model}}}` (US4 acceptance #6, FR-038) (use devs:rust-dev agent)
- [ ] T119 [US4] Active-version-only default; `?source_version_revision=N` overrides for historical reads (US4 acceptance #7) (use devs:rust-dev agent)
- [ ] T120 [GIT] Commit: read API routes + model-mismatch enforcement

### mn-server — rate limiting + request-id + error envelope wiring

- [ ] T121 [P] [US4] Request-ID middleware in `crates/mn-server/src/middleware/request_id.rs` — generate or accept `X-Request-Id`, propagate to logs (FR-029, FR-106) (use devs:rust-dev agent)
- [ ] T122 [P] [US4] Rate-limit middleware in `crates/mn-server/src/rate_limit.rs` — token bucket with tier resolution: CIDR override > GitHub SSO bearer > anonymous per-IP (D11, FR-031). Adds `X-RateLimit-Limit/Remaining/Reset` headers (use devs:rust-dev agent)
- [ ] T123 [US4] 503 + `Retry-After` on transient DB unavailability for all read endpoints (US4 acceptance #15, FR-035) (use devs:rust-dev agent)
- [ ] T124 [US4] 400 invalid_request on empty queries / vector-dim mismatch (US4 acceptance #14) (use devs:rust-dev agent)
- [ ] T125 [US4] OpenAPI generation: register all routes with `utoipa`, expose `/v1/openapi.json` (matches `contracts/openapi.yaml`) (use devs:rust-dev agent)
- [ ] T126 [GIT] Commit: rate limiting + request id + error envelope plumbing

### Tests

- [ ] T127 [P] [US4] Integration test `crates/mn-server/tests/search_hybrid.rs` — ingests fixture, queries, asserts RRF ordering and per-result scores (use devs:rust-dev agent)
- [ ] T128 [P] [US4] Integration test `crates/mn-server/tests/model_mismatch_409.rs` — asserts 409 envelope shape (use devs:rust-dev agent)
- [ ] T129 [P] [US4] Integration test `crates/mn-server/tests/rate_limit_tiers.rs` — anonymous, SSO-style stub, CIDR override (use devs:rust-dev agent)
- [ ] T130 [P] [US4] Integration test `crates/mn-server/tests/historical_version_read.rs` — asserts default active-only and explicit historical-revision read (US4 acceptance #7) (use devs:rust-dev agent)
- [ ] T131 [P] [US4] Load test scaffolding under `tests/load/` (criterion or k6 wrapper) — exercises `/v1/search` against a 10k-chunk seeded corpus and asserts p95 < 500 ms (SC-013). Wired to a CI job (gated by label) (use devs:rust-dev agent)
- [ ] T132 [P] [US4] Recall benchmark under `tests/recall/hybrid_vs_single.rs` — 50 labelled query/relevant-chunk pairs, asserts ≥10pp recall@10 lift (SC-014) (use devs:rust-dev agent)
- [ ] T133 [GIT] Commit: read API integration, load, and recall tests

### Phase 4 close-out

- [ ] T134 [US4] Run `/sdd:map incremental`
- [ ] T135 [US4] Review `retro/P4.md` and extract conservative learnings to `CLAUDE.md`
- [ ] T136 [GIT] Commit: codebase mapping + retro for Phase 4
- [ ] T137 [GIT] Push branch and update PR body with Phase 4 summary
- [ ] T138 [GIT] Verify all CI checks pass
- [ ] T139 [GIT] Report PR ready status — STOP and await LGTM

**Checkpoint**: Cloud read API is fully usable by any HTTP client. MCP server can now be wired against it.

---

## Phase 5: User Story 5 — Local MCP server: retrieval tools (Priority: P1)

**Goal**: `mnm mcp serve` exposes 7 MCP tools over stdio: `search`, `get_chunk`, `get_chunk_siblings`, `get_chunk_parents`, `list_sources`, `pull_models`, `status`. Models load lazily on first retrieval. Reranker is `bge-reranker-base` (cross-encoder). Cold start < 500 ms (SC-019).

**Independent Test**: An MCP test harness (JSON-RPC over stdio) drives the binary through `initialize` → `list_tools` → `call_tool search` and asserts (a) handshake < 500 ms, (b) first search loads models once and completes < 2.5 s (SC-020), (c) `rerank=false` returns cloud's RRF order, (d) model-mismatch returns typed MCP error pointing at `pull_models`.

### Phase 5 git entry

- [ ] T140 [GIT] Verify working tree is clean before starting Phase 5
- [ ] T141 [US5] Create `retro/P5.md` from retro template

### mn-embedding — reranker

- [ ] T142 [P] [US5] `crates/mn-embedding/src/reranker.rs` — wraps fastembed's cross-encoder for `bge-reranker-base`; lazy single-shot load shared with the embedder (use devs:rust-dev agent)
- [ ] T143 [US5] Integration test in `crates/mn-embedding/tests/reranker_smoke.rs` — reranks 20 candidates against a known query, asserts top-1 changes vs RRF baseline (use devs:rust-dev agent)
- [ ] T144 [GIT] Commit: mn-embedding reranker

### mn-mcp — transport, server loop, tools

- [ ] T145 [P] [US5] Stdio JSON-RPC framing in `crates/mn-mcp/src/transport.rs` per FR-036 (LSP-style framing or newline-delimited per MCP spec) (use devs:rust-dev agent)
- [ ] T146 [P] [US5] MCP server loop with static tool/resource manifest in `crates/mn-mcp/src/server.rs` — handshake completes immediately, model load gated behind a `OnceCell` triggered by first retrieval (US5 acceptance #1, #2, #13) (use devs:rust-dev agent)
- [ ] T147 [P] [US5] Tool `search` in `crates/mn-mcp/src/tools/search.rs` — embeds query/queries locally, POSTs to `/v1/search`, reranks top-K, returns top `limit` (US5 acceptance #3, #4, #5) (use devs:rust-dev agent)
- [ ] T148 [P] [US5] Tool `get_chunk`, `get_chunk_siblings`, `get_chunk_parents` — pass-through proxies to corresponding cloud endpoints (use devs:rust-dev agent)
- [ ] T149 [P] [US5] Tool `list_sources` — proxies to `GET /v1/sources` (use devs:rust-dev agent)
- [ ] T150 [P] [US5] Tool `pull_models` — downloads embedder + reranker to model cache, emits MCP progress notifications, returns `{embedding_model, reranker_model, total_bytes, took_ms}` (US5 acceptance #8) (use devs:rust-dev agent)
- [ ] T151 [P] [US5] Tool `status` — returns server_version, cloud_reachable, corpus model, local model state, rate_limit_tier WITHOUT requiring models to be loaded (US5 acceptance #9) (use devs:rust-dev agent)
- [ ] T152 [US5] MCP error mapping: cloud 409 → `embedding_model_mismatch` with remediation referencing `pull_models`; network failure → `service_unavailable` with `Retry-After` echoed (US5 acceptance #6, #11) (use devs:rust-dev agent)
- [ ] T153 [US5] Tool-description "Patterns" subsection on `search` tool description naming `hyde`, `multi_query`, `step_back` per FR-091 (full cookbook ships in Phase 10) (use devs:rust-dev agent)
- [ ] T154 [US5] SIGTERM handling: cancel in-flight cloud calls, flush pending telemetry, exit within 1 s (US5 acceptance #14) (use devs:rust-dev agent)
- [ ] T155 [GIT] Commit: mn-mcp transport, server, 7 tools

### mn-cli — `mcp serve` subcommand + `models` + `mcp install` + `doctor`

- [ ] T156 [P] [US5] `mnm mcp serve` subcommand wires `mn-mcp::server::run` over stdio in `crates/mn-cli/src/commands/mcp_serve.rs` (use devs:rust-dev agent)
- [ ] T157 [P] [US5] `mnm models pull` — uses mn-embedding cache and digest verification; respects `--name` flag (use devs:rust-dev agent)
- [ ] T158 [P] [US5] `mnm models list` and `mnm models prune` (`--keep`) in `crates/mn-cli/src/commands/models.rs` (use devs:rust-dev agent)
- [ ] T159 [P] [US5] `mnm mcp install [--agent claude-code|cursor|...] [--config-path]` and `mnm mcp status` in `crates/mn-cli/src/commands/mcp_install.rs` (US8 acceptance #13 first cut — refined in Phase 8) (use devs:rust-dev agent)
- [ ] T160 [P] [US5] `mnm doctor` — first cut: reports CLI version, model presence, MCP installation status, cloud reachability, corpus model match, admin-visibility flag, config file location, with `--json` (full diagnostic surface refined in Phase 8) (use devs:rust-dev agent)
- [ ] T161 [GIT] Commit: `mcp serve`, `models`, `mcp install`, `doctor` CLI commands

### Tests

- [ ] T162 [P] [US5] JSON-RPC test harness in `crates/mn-mcp/tests/jsonrpc_harness.rs` that drives the server through `initialize → list_tools → call_tool` and asserts response shapes (use devs:rust-dev agent)
- [ ] T163 [P] [US5] Cold-start latency test `mn-mcp/tests/cold_start.rs` — boots the binary, measures handshake completion, asserts < 500 ms (SC-019) (use devs:rust-dev agent)
- [ ] T164 [P] [US5] Lazy-load contention test `mn-mcp/tests/concurrent_first_search.rs` — fires two `search` calls in parallel before models load, asserts only one load occurs (US5 acceptance #13) (use devs:rust-dev agent)
- [ ] T165 [P] [US5] Steady-state p95 test `mn-mcp/tests/steady_p95.rs` — 1000 sequential `search` calls, asserts p95 < 1 s end-to-end (SC-020) (use devs:rust-dev agent)
- [ ] T166 [P] [US5] Reranker quality test under `tests/recall/rerank_lift.rs` — nDCG@5 lift +0.05 absolute over un-reranked (SC-021) (use devs:rust-dev agent)
- [ ] T167 [GIT] Commit: MCP test harness + cold-start + concurrency + p95 + rerank-lift tests

### Phase 5 close-out

- [ ] T168 [US5] Run `/sdd:map incremental`
- [ ] T169 [US5] Review `retro/P5.md` and extract conservative learnings to `CLAUDE.md`
- [ ] T170 [GIT] Commit: codebase mapping + retro for Phase 5
- [ ] T171 [GIT] Push and update PR body with Phase 5 summary
- [ ] T172 [GIT] Verify all CI checks pass
- [ ] T173 [GIT] Report PR ready status — STOP and await LGTM

**Checkpoint**: End-to-end loop works: maintainer ingests md → MCP server retrieves and reranks → AI agent consumes results. This is the MVP.

---

## Phase 6: User Story 3 — Admin ingestion of source-code repos via CLI (Priority: P1)

**Goal**: `mnm ingest code <slug> <path>` chunks source files via tree-sitter (Rust / TS / JS) and a hand-rolled Compact module scanner, detects package membership, supports `--git <url>` clone-and-ingest, respects gitignore and default excludes, and reuses all of US2's upload/auth/lifecycle plumbing.

**Independent Test**: `mnm ingest code compact-examples ./fixtures/code-tree` ingests Rust, TS, and Compact files; every chunk has the right `package` tag; `node_modules/` and `target/` are skipped; tree-sitter parse errors fall back per-file to line-window with a warning.

### Phase 6 git entry

- [ ] T174 [GIT] Verify working tree is clean before starting Phase 6
- [ ] T175 [US3] Create `retro/P6.md` from retro template

### mn-content — code chunkers + package detection

- [ ] T176 [P] [US3] tree-sitter loader + grammar registration in `crates/mn-content/src/code/mod.rs` (Rust, TypeScript, JavaScript; one Parser cached per language) (use devs:rust-dev agent)
- [ ] T177 [P] [US3] Rust chunker in `crates/mn-content/src/code/rust.rs` — splits at `mod`/`impl`/`struct`/`enum`/`fn` boundaries, records `symbol_path` (use devs:rust-dev agent)
- [ ] T178 [P] [US3] TS/JS chunker in `crates/mn-content/src/code/ts_js.rs` — `namespace`/`class`/`interface`/`function`/`method` boundaries (use devs:rust-dev agent)
- [ ] T179 [P] [US3] Compact module scanner in `crates/mn-content/src/compact.rs` — hand-rolled top-level `module <Name> { ... }` lexer per D9, FR-047. Inside modules, falls back to line-window. Records module byte ranges for package detection (use devs:rust-dev agent)
- [ ] T180 [P] [US3] Line-window fallback in `crates/mn-content/src/code/window.rs` (default 60 lines, 20-line overlap; configurable) (use devs:rust-dev agent)
- [ ] T181 [P] [US3] Tree-sitter syntax-error fallback: per-file fallback to line-window with a structured warning (US3 acceptance #11) (use devs:rust-dev agent)
- [ ] T182 [P] [US3] Package detection in `crates/mn-content/src/package.rs` — Rust (walk to nearest `[package]` Cargo.toml, skip virtual `[workspace]` roots — US3 acceptance #6, FR-050.a), TS/JS (walk to nearest package.json with `"name"`), Compact (in-source module declarations, multiple per file possible, US3 acceptance #5) (use devs:rust-dev agent)
- [ ] T183 [P] [US3] Default excludes: `node_modules/`, `target/`, `vendor/`, `dist/`, `build/`, `out/`, `coverage/`, `.git/`, lockfiles, generated patterns (US3 fixed list) (use devs:rust-dev agent)
- [ ] T184 [P] [US3] gitignore honor (default on) + binary-file sniff (US3 acceptance #8, #12) (use devs:rust-dev agent)
- [ ] T185 [US3] Unit tests for each chunker (golden trees) and package detector (multi-package Cargo workspace, scoped npm name, multi-module Compact file)
- [ ] T186 [GIT] Commit: mn-content code chunkers + package detection

### mn-cli — `ingest code` command

- [ ] T187 [US3] `mnm ingest code <slug> <path>` command in `crates/mn-cli/src/commands/ingest_code.rs` reusing the upload/finalize plumbing from US2 (use devs:rust-dev agent)
- [ ] T188 [US3] Flags: `--git`, `--ref`, `--language <ext>=<grammar>`, `--include`, `--exclude`, `--no-respect-gitignore`, `--include-submodules`, `--code-chunk-lines`, `--code-chunk-overlap`, `--max-file-size`, plus the US2-shared `--strict/--dry-run/--force-new/--embedding-model/--batch-size` (use devs:rust-dev agent)
- [ ] T189 [US3] `--git` clone path: shallow clone into a tempdir, ingest, remove tempdir on exit (success or failure, US3 acceptance #9) (use devs:rust-dev agent)
- [ ] T190 [US3] Integration test `crates/mn-cli/tests/ingest_code_e2e.rs` covers a 3-language fixture and verifies package tagging end-to-end against testcontainers Postgres
- [ ] T191 [GIT] Commit: `mnm ingest code` command + flags + tests

### Phase 6 close-out

- [ ] T192 [US3] Run `/sdd:map incremental`
- [ ] T193 [US3] Review `retro/P6.md` and extract conservative learnings to `CLAUDE.md`
- [ ] T194 [GIT] Commit: codebase mapping + retro for Phase 6
- [ ] T195 [GIT] Push and update PR body with Phase 6 summary
- [ ] T196 [GIT] Verify all CI checks pass
- [ ] T197 [GIT] Report PR ready status — STOP and await LGTM

**Checkpoint**: Maintainers can ingest both docs and code repos. P1 stories complete.

---

## Phase 7: User Story 9 — Cloud server: auth, write API, deploy, ops (Priority: P2)

**Goal**: Materialize the full write protocol with Ed25519 challenge-response admin auth (1h HS256 JWT, D10/D21), GitHub OAuth read-uplift (30d bearer, D11/D28), CIDR rate-limit overrides, sweep job (retention=5 + 24h grace), readiness/health checks, structured startup, and a Fly.io deploy.

**Independent Test**: An integration test runs the full admin flow: `mnm keys generate` → register pubkey in user-store TOML → `mnm login` → admin JWT issued → `mnm ingest md` succeeds → request with no JWT receives 401 → expired JWT receives 401 with remediation. A separate test runs the GitHub OAuth flow against a mocked GitHub.

### Phase 7 git entry

- [ ] T198 [GIT] Verify working tree is clean before starting Phase 7
- [ ] T199 [US9] Create `retro/P7.md` from retro template

### mn-auth — keys, JWT, OAuth, user store

- [ ] T200 [P] [US9] `crates/mn-auth/src/ed25519.rs` — keygen, sign, verify using `ed25519-dalek` (use devs:rust-dev agent)
- [ ] T201 [P] [US9] `crates/mn-auth/src/jwt.rs` — HS256 mint/verify with `jsonwebtoken`, 1h TTL per D21, claims `{sub, role, exp, iat, jti}` (use devs:rust-dev agent)
- [ ] T202 [P] [US9] `crates/mn-auth/src/oauth_github.rs` — web flow + device flow (`--no-browser` per FR-115) using `oauth2`; verify org membership with `octocrab` (FR-062) (use devs:rust-dev agent)
- [ ] T203 [P] [US9] `crates/mn-auth/src/user_store.rs` — TOML loader (schema_version=1, fail-fast on unknown fields per Constitution VIII / EC-56) (use devs:rust-dev agent)
- [ ] T204 [P] [US9] Bearer-token store for read-uplift tokens (server-side hashed-token table or signed token; per D28 we use a signed bearer with `MIDNIGHT_MANUAL_READ_TOKEN_TTL_DAYS`) in `crates/mn-auth/src/read_uplift.rs` (use devs:rust-dev agent)
- [ ] T205 [US9] Unit tests for keypair gen + sign/verify, JWT mint/verify (incl. expired-token rejection), org-membership check (mocked octocrab)
- [ ] T206 [GIT] Commit: mn-auth Ed25519, JWT, OAuth, user-store

### mn-server — auth routes + admin routes + sweep

- [ ] T207 [P] [US9] Route `POST /v1/auth/challenge` (body `{user_id}` → `{nonce, expires_at}`); nonce store with TTL in `crates/mn-server/src/routes/auth_challenge.rs` (use devs:rust-dev agent)
- [ ] T208 [P] [US9] Route `POST /v1/auth/verify` (body `{user_id, signature, nonce}` → `{jwt, expires_at}`); ed25519 verify + JWT mint (use devs:rust-dev agent)
- [ ] T209 [P] [US9] Routes `GET /v1/auth/github/start` and `GET /v1/auth/github/callback` (use devs:rust-dev agent)
- [ ] T210 [P] [US9] Auth middleware in `crates/mn-server/src/middleware/auth.rs` — extracts admin JWT or read-uplift bearer; populates request extensions; enforces role on admin-tagged routes (use devs:rust-dev agent)
- [ ] T211 [P] [US9] Wrap previously-anonymous write endpoints (T076–T080) with admin JWT requirement; refusal returns 401/403 with typed error (use devs:rust-dev agent)
- [ ] T212 [P] [US9] Route `PATCH /v1/sources/{slug}` and `POST /v1/sources/{slug}/retire` (admin) (use devs:rust-dev agent)
- [ ] T213 [P] [US9] Routes `POST /v1/sources/{slug}/versions/{rev}/promote` and `POST /v1/sources/{slug}/versions/{rev}/retire` (admin) (use devs:rust-dev agent)
- [ ] T214 [P] [US9] Admin CIDR rate-limit endpoints: `POST/GET/PATCH/DELETE /v1/admin/ratelimits` (use devs:rust-dev agent)
- [ ] T215 [P] [US9] Sweep job in `crates/mn-server/src/sweep.rs` — periodic tokio task: deletes source_versions older than retention_count past grace window in one tx; sweeps aborted ingest_runs older than `MIDNIGHT_MANUAL_ABORT_GRACE` (US9 acceptance #10, FR-063) (use devs:rust-dev agent)
- [ ] T216 [US9] Structured startup sequence per US9 acceptance #9 (user store load → JWT secret load → DB connect → run migrations → seed embedding_model → start listener; any failure exits with structured stderr error) (use devs:rust-dev agent)
- [ ] T217 [US9] `/readyz` reports 503 with last DB error if DB has been unreachable for > configurable grace (US9 acceptance #11) (use devs:rust-dev agent)
- [ ] T218 [GIT] Commit: auth middleware + admin routes + sweep

### Tests

- [ ] T219 [P] [US9] Integration test `crates/mn-server/tests/auth_challenge_verify.rs` (use devs:rust-dev agent)
- [ ] T220 [P] [US9] Integration test `crates/mn-server/tests/admin_jwt_required.rs` — 401 without JWT, 403 wrong role, 200 with admin JWT (use devs:rust-dev agent)
- [ ] T221 [P] [US9] Integration test `crates/mn-server/tests/cidr_override.rs` — CIDR override beats SSO tier beats anonymous (use devs:rust-dev agent)
- [ ] T222 [P] [US9] Integration test `crates/mn-server/tests/sweep_retention.rs` — 6 versions + 24h-aged-out version is deleted by sweep tick (use devs:rust-dev agent)
- [ ] T223 [GIT] Commit: auth + admin + sweep integration tests

### Fly.io deploy artifacts

- [ ] T224 [P] [US9] `Dockerfile.server` — multi-stage `cargo-chef` build, distroless `gcr.io/distroless/cc` runtime, only `midnight-manual-server` in the final image (use dev-specialisms:fly-deploy skill)
- [ ] T225 [P] [US9] `fly.toml` — single region (`lhr` or `iad`), required secrets, autoscale config (use dev-specialisms:fly-deploy skill)
- [ ] T226 [P] [US9] `docs/README-deploy.md` — Fly operational notes (secrets, migrations, rollback procedure, region change) (use midnight-docs-writer skill)
- [ ] T227 [GIT] Commit: Dockerfile.server + fly.toml + deploy doc

### Phase 7 close-out

- [ ] T228 [US9] Run `/sdd:map incremental`
- [ ] T229 [US9] Review `retro/P7.md` and extract conservative learnings to `CLAUDE.md`
- [ ] T230 [GIT] Commit: codebase mapping + retro for Phase 7
- [ ] T231 [GIT] Push and update PR body with Phase 7 summary
- [ ] T232 [GIT] Verify all CI checks pass
- [ ] T233 [GIT] Report PR ready status — STOP and await LGTM

**Checkpoint**: Cloud server is fully authenticated, swept on schedule, and deployable to Fly.io.

---

## Phase 8: User Story 8 — CLI admin lifecycle (Priority: P2)

**Goal**: Complete the admin command surface: `keys`, `login/logout`, `users`, `versions promote/rollback/retire`, `ratelimits`, `db migrate/status`, `telemetry`, `auth github/status/logout`, plus a hardened `doctor` and full `--json` parity. Admin commands hidden from default help per D23.

**Independent Test**: `mnm --help` shows only developer commands; `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1 mnm --help` shows everything. `mnm keys generate` → `mnm login` → `mnm versions promote` → `mnm versions rollback` → `mnm logout` exercise the full admin path against a Phase-7-server in tests.

### Phase 8 git entry

- [ ] T234 [GIT] Verify working tree is clean before starting Phase 8
- [ ] T235 [US8] Create `retro/P8.md` from retro template

### mn-cli — admin commands

- [ ] T236 [P] [US8] `mnm keys generate` and `mnm keys import` in `crates/mn-cli/src/commands/keys.rs` — Ed25519 keypair to `$XDG_CONFIG_HOME/midnight-manual/keys/<user_id>.{public,private}` (chmod 0600 on private), echoes pubkey in TOML user-store row format (use devs:rust-dev agent)
- [ ] T237 [P] [US8] `mnm login --user-id` in `crates/mn-cli/src/commands/login.rs` — challenge-response against Phase-7 server, writes JWT to `auth.toml[admin]` with chmod 0600 (use devs:rust-dev agent)
- [ ] T238 [P] [US8] `mnm logout` — clears `auth.toml[admin]` (use devs:rust-dev agent)
- [ ] T239 [P] [US8] `mnm auth github [--no-browser]`, `mnm auth status`, `mnm auth logout` in `crates/mn-cli/src/commands/auth_github.rs` — writes `auth.toml[read_uplift]` (use devs:rust-dev agent)
- [ ] T240 [P] [US8] `mnm users add/list/show/update/remove` in `crates/mn-cli/src/commands/users.rs` — edits local user-store TOML; refuses on schema_version mismatch; emits deploy-required warning after every mutation (use devs:rust-dev agent)
- [ ] T241 [P] [US8] `mnm versions promote/rollback/retire` in `crates/mn-cli/src/commands/versions_admin.rs` — wraps the Phase-7 admin endpoints (use devs:rust-dev agent)
- [ ] T242 [P] [US8] `mnm sources add/update/retire` (hidden) in `crates/mn-cli/src/commands/sources_admin.rs` (use devs:rust-dev agent)
- [ ] T243 [P] [US8] `mnm ratelimits add/list/extend/remove` in `crates/mn-cli/src/commands/ratelimits.rs` (use devs:rust-dev agent)
- [ ] T244 [P] [US8] `mnm db migrate` and `mnm db status` (preflight runner against `DATABASE_URL`) in `crates/mn-cli/src/commands/db.rs` (use devs:rust-dev agent)
- [ ] T245 [P] [US8] `mnm telemetry status/enable/disable` in `crates/mn-cli/src/commands/telemetry.rs` (writes user config; full telemetry plumbing lands in Phase 12 — this story finalizes the CLI surface) (use devs:rust-dev agent)
- [ ] T246 [P] [US8] `mnm config show/get/set` in `crates/mn-cli/src/commands/config.rs` — `--effective` flag resolves env + flag overrides (use devs:rust-dev agent)
- [ ] T247 [P] [US8] Hardened `mnm doctor` (replaces Phase-5 stub) — reports CLI version, embedding/reranker presence + version, MCP installation across known agents, cloud reachability, corpus model match, local keypair presence, login state, admin-visibility flag, config file location; `--json` emits single JSON object (use devs:rust-dev agent)
- [ ] T248 [P] [US8] `mnm version` and `--version` alias on every subcommand (use devs:rust-dev agent)
- [ ] T249 [US8] `--json` parity audit: every command emits either a single JSON document or NDJSON to stdout, never human-formatted text mixed in (FR-021) (use devs:rust-dev agent)
- [ ] T250 [GIT] Commit: admin CLI command surface + hardened doctor + json parity

### Tests

- [ ] T251 [P] [US8] CLI integration test `crates/mn-cli/tests/admin_lifecycle.rs` — keys → login → users add → versions promote → versions rollback → logout (use devs:rust-dev agent)
- [ ] T252 [P] [US8] Help-visibility test `crates/mn-cli/tests/help_visibility.rs` — admin commands hidden by default; visible under env flag; invokable regardless (D23) (use devs:rust-dev agent)
- [ ] T253 [P] [US8] `mnm doctor --json` schema test — output validates against documented shape (use devs:rust-dev agent)
- [ ] T254 [GIT] Commit: admin CLI integration tests

### Phase 8 close-out

- [ ] T255 [US8] Run `/sdd:map incremental`
- [ ] T256 [US8] Review `retro/P8.md` and extract conservative learnings to `CLAUDE.md`
- [ ] T257 [GIT] Commit: codebase mapping + retro for Phase 8
- [ ] T258 [GIT] Push and update PR body with Phase 8 summary
- [ ] T259 [GIT] Verify all CI checks pass
- [ ] T260 [GIT] Report PR ready status — STOP and await LGTM

---

## Phase 9: User Story 6 — Confidence scoring & result ranking (Priority: P2)

**Goal**: Add `trust_score`, `confidence`, `confidence_factors` to every read result. Trust scoring is policy-driven (TOML at `MIDNIGHT_MANUAL_SCORING_POLICY`, compiled defaults fallback). Sorting modes: `confidence`/`trust`/`relevance`/`score`. `min_confidence` filter. All additive — no contract breaks (Constitution I).

**Independent Test**: A search of an ingested corpus with mixed attributions returns results sorted by `confidence` descending; `sort_by=trust` reorders; `min_confidence=0.5` drops sub-threshold results and `search_metadata.filtered_by_confidence` reports the count.

### Phase 9 git entry

- [ ] T261 [GIT] Verify working tree is clean before starting Phase 9
- [ ] T262 [US6] Create `retro/P9.md` from retro template

### mn-core — full scoring policy loader

- [ ] T263 [P] [US6] Promote `scoring_policy.rs` from stub to full loader: parse the TOML, validate finite non-negative weights, reject unknown keys (fail-fast at startup per Constitution VIII), compiled-in defaults when env unset (US6 acceptance #11) (use devs:rust-dev agent)
- [ ] T264 [P] [US6] Property test in `crates/mn-core/tests/proptest_scoring.rs` — trust_score and confidence are deterministic and clamped to [0,1] for arbitrary policies + provenance inputs (SC-047) (use devs:rust-dev agent)
- [ ] T265 [GIT] Commit: mn-core scoring policy loader + property tests

### mn-retrieval — scoring computation

- [ ] T266 [P] [US6] `crates/mn-retrieval/src/scoring.rs` — `trust_score = base * ver * fresh * dep * vmatch` and `confidence = trust^w_t * relevance^w_r` per spec.md §"Trust score computation" (use devs:rust-dev agent)
- [ ] T267 [P] [US6] Relevance normalization: `relevance_rrf = 1 - 1/(1 + raw)` and `relevance_rerank = sigmoid(raw_logit)`; both compiled-in (non-configurable) (use devs:rust-dev agent)
- [ ] T268 [P] [US6] `confidence_factors` builder — emits the full breakdown object documented in spec.md (US6 acceptance #12) (use devs:rust-dev agent)
- [ ] T269 [US6] Wire scoring into the cloud `POST /v1/search` response (additive: existing callers ignore the new fields) and the MCP `search` tool result (which recomputes confidence using the reranker score when `rerank=true`) (use devs:rust-dev agent)
- [ ] T270 [US6] Add request fields `sort_by ∈ {confidence,trust,relevance,score}` (default `confidence`) and `min_confidence ∈ [0,1]` to the read API and MCP `search` tool — both ignore missing/unknown values to remain additive (use devs:rust-dev agent)
- [ ] T271 [US6] `search_metadata.filtered_by_confidence` count when `min_confidence` excludes results (US6 acceptance #10) (use devs:rust-dev agent)
- [ ] T272 [GIT] Commit: scoring computation + sort_by + min_confidence wiring

### Tests

- [ ] T273 [P] [US6] Integration test `crates/mn-server/tests/scoring_ordering.rs` — foundation > partner > third_party > community > unknown for identical-otherwise results; verified > unverified; fresh > stale; deprecation penalty applied (use devs:rust-dev agent)
- [ ] T274 [P] [US6] Integration test `crates/mn-mcp/tests/rerank_relevance_substitution.rs` — `rerank=true` sets `confidence_factors.relevance_source="rerank"` and recomputes confidence (use devs:rust-dev agent)
- [ ] T275 [P] [US6] Recall benchmark under `tests/recall/confidence_lift.rs` — confidence-sorted nDCG@5 +0.05 over RRF-sorted (SC-048) (use devs:rust-dev agent)
- [ ] T276 [P] [US6] Criterion bench `benches/scoring.rs` — confidence computation < 1µs per result (use devs:rust-dev agent)
- [ ] T277 [GIT] Commit: scoring tests + benches

### Phase 9 close-out

- [ ] T278 [US6] Run `/sdd:map incremental`
- [ ] T279 [US6] Review `retro/P9.md` and extract conservative learnings to `CLAUDE.md`
- [ ] T280 [GIT] Commit: codebase mapping + retro for Phase 9
- [ ] T281 [GIT] Push and update PR body
- [ ] T282 [GIT] Verify all CI checks pass
- [ ] T283 [GIT] Report PR ready status — STOP and await LGTM

---

## Phase 10: User Story 7 — Query enhancement: multi-query support and cookbook (Priority: P2)

**Goal**: Lift the multi-query contract to first class — `queries: [{text, vector}]` with 1 ≤ N ≤ 10, RRF across modes AND across pairs, `scores.matched_queries`, `search_metadata.per_query`, per-query rate-limit cost (D25). Ship `docs/cookbook/query-enhancement.md` with worked HyDE / multi-query / step-back examples.

**Independent Test**: A 3-query request returns merged results with `matched_queries` set per result; rate-limit consumes `max(1, N)=3` tokens; recall@10 lift ≥ 8pp vs single-query on the labelled benchmark (SC-049). Cookbook examples are runnable.

### Phase 10 git entry

- [ ] T284 [GIT] Verify working tree is clean before starting Phase 10
- [ ] T285 [US7] Create `retro/P10.md` from retro template

### Implementation

- [ ] T286 [P] [US7] Extend RRF merger in `mn-retrieval` to merge across both modes AND across query-pair axis in a single pass (already partially landed in Phase 4 — confirm and document) (use devs:rust-dev agent)
- [ ] T287 [P] [US7] `search_metadata.per_query` builder — one record per input query: `{query_index, fts_candidates, vector_candidates, fts_latency_ms, vector_latency_ms}` (use devs:rust-dev agent)
- [ ] T288 [P] [US7] Per-result `scores.matched_queries` — 0-based input indices that contributed (use devs:rust-dev agent)
- [ ] T289 [P] [US7] Multi-query rate-limit cost: charge `max(1, N)` tokens per request, surface in `X-RateLimit-Remaining` (D25, US7 acceptance #5) (use devs:rust-dev agent)
- [ ] T290 [P] [US7] Max-queries enforcement: `queries.length > MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST` (default 10) returns 400 before consuming rate-limit tokens (US7 acceptance #4) (use devs:rust-dev agent)
- [ ] T291 [P] [US7] Single-query convenience form equivalence: `{query, vector}` must produce byte-identical response to `{queries: [{text:query, vector}]}` — gated by a property test (use devs:rust-dev agent)
- [ ] T292 [P] [US7] CLI `mnm search` multi-query support: repeated `--query` flags or `--queries-stdin` JSON shape; emits per-query diagnostics from `search_metadata` (use devs:rust-dev agent)
- [ ] T293 [P] [US7] MCP `search` tool description gains the "Patterns" subsection citing `hyde`, `multi_query`, `step_back` (was stubbed in Phase 5 T153; fleshed out here with examples) (use devs:rust-dev agent)
- [ ] T294 [P] [US7] `docs/cookbook/query-enhancement.md` — runnable cookbook with worked examples for HyDE, multi-query, step-back per FR-092 (use midnight-docs-writer skill)
- [ ] T295 [GIT] Commit: multi-query support + cookbook

### Tests

- [ ] T296 [P] [US7] Integration test `crates/mn-server/tests/multi_query_rrf.rs` — N pairs merge correctly; matched_queries populated (use devs:rust-dev agent)
- [ ] T297 [P] [US7] Property test `mn-server/tests/proptest_single_vs_multi_equiv.rs` — byte-identical responses for the two shapes (US7 acceptance #6) (use devs:rust-dev agent)
- [ ] T298 [P] [US7] Recall benchmark `tests/recall/multi_query_lift.rs` — ≥ 8pp recall@10 lift (SC-049) (use devs:rust-dev agent)
- [ ] T299 [P] [US7] Rate-limit cost test `tests/load/multi_query_cost.rs` — confirms post-charge balance (use devs:rust-dev agent)
- [ ] T300 [GIT] Commit: multi-query tests + benchmarks

### Phase 10 close-out

- [ ] T301 [US7] Run `/sdd:map incremental`
- [ ] T302 [US7] Review `retro/P10.md` and extract conservative learnings to `CLAUDE.md`
- [ ] T303 [GIT] Commit: codebase mapping + retro for Phase 10
- [ ] T304 [GIT] Push and update PR body
- [ ] T305 [GIT] Verify all CI checks pass
- [ ] T306 [GIT] Report PR ready status — STOP and await LGTM

---

## Phase 11: User Story 10 — Distribution (Priority: P3)

**Goal**: One continuous release pipeline driven by `release-please`. Every merge to `main` opens/updates a release PR; merging it triggers a 7-target binary matrix via `cargo-dist`, publishes the crate, updates the Homebrew tap, pushes a multi-arch Docker image to GHCR, and deploys Fly.io — all from one CI run.

**Independent Test**: A dry-run of the release workflow on a tag-candidate SHA emits all artifacts locally (`cargo dist build && cargo dist plan`) and the canary `mnm version` reports the right version + commit + build_date.

### Phase 11 git entry

- [ ] T307 [GIT] Verify working tree is clean before starting Phase 11
- [ ] T308 [US10] Create `retro/P11.md` from retro template

### Implementation

- [ ] T309 [P] [US10] Configure `release-please-action` in `.github/workflows/release-please.yml` (or fold into `release.yml`) with `release-please-config.json` and `.release-please-manifest.json` — Conventional-Commit-driven version bumps + CHANGELOG (use dev-specialisms:init-local-tooling skill)
- [ ] T310 [P] [US10] Configure `cargo-dist` in workspace `Cargo.toml` (`[workspace.metadata.dist]`) for the 7-target matrix (`x86_64-unknown-linux-{gnu,musl}`, `aarch64-unknown-linux-{gnu,musl}`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`); publish-jobs include cargo-publish, GitHub Release, GHCR (use dev-specialisms:init-local-tooling skill)
- [ ] T311 [P] [US10] Homebrew tap update step in the release workflow — pushes to `midnight-network/homebrew-tap` via `homebrew-releaser` (or hand-rolled formula bump) (use dev-specialisms:init-local-tooling skill)
- [ ] T312 [P] [US10] Docker buildx step — multi-arch (linux/amd64 + linux/arm64) image of `midnight-manual-server` pushed to `ghcr.io/midnight-network/midnight-manual:v<X.Y.Z>` and `:latest` (use dev-specialisms:fly-deploy skill)
- [ ] T313 [P] [US10] `flyctl deploy` step gated on Docker image push success (use dev-specialisms:fly-deploy skill)
- [ ] T314 [P] [US10] `mnm version` reports `{version, commit, build_date}` populated at build time via `vergen` or `built` (use devs:rust-dev agent)
- [ ] T315 [P] [US10] CI matrix exercises MSRV toolchain + `stable` on every PR (US10 acceptance #7) (use dev-specialisms:init-local-tooling skill)
- [ ] T316 [P] [US10] `cargo-audit` or `cargo-vet` runs on every PR as supply-chain guard (use devs:rust-dev agent)
- [ ] T317 [US10] Conventional-Commit lint job (commitlint or `cog`) blocks merges that don't comply (FR-098, Constitution X) (use dev-specialisms:init-local-tooling skill)
- [ ] T318 [GIT] Commit: release pipeline + cargo-dist + MSRV + audit + commitlint

### Tests

- [ ] T319 [P] [US10] Smoke test in CI: `cargo dist plan` succeeds and lists all 7 targets (use devs:rust-dev agent)
- [ ] T320 [P] [US10] Test `crates/mn-cli/tests/version_stamp.rs` — `mnm version --json` reports populated fields and matches `Cargo.toml::package.version` (use devs:rust-dev agent)
- [ ] T321 [P] [US10] `mnm doctor --json` post-install test on each OS+arch reports `cli_version` populated, `models.state="missing"`, `mcp.installation="not installed"`, exits 0 (US10 acceptance #11) (use devs:rust-dev agent)
- [ ] T322 [GIT] Commit: release smoke tests

### Phase 11 close-out

- [ ] T323 [US10] Run `/sdd:map incremental`
- [ ] T324 [US10] Review `retro/P11.md` and extract conservative learnings to `CLAUDE.md`
- [ ] T325 [GIT] Commit: codebase mapping + retro for Phase 11
- [ ] T326 [GIT] Push and update PR body
- [ ] T327 [GIT] Verify all CI checks pass
- [ ] T328 [GIT] Report PR ready status — STOP and await LGTM

---

## Phase 12: User Story 11 — Observability & telemetry (Priority: P3)

**Goal**: Structured JSON logs everywhere, end-to-end `request_id` propagation, opt-out telemetry pipeline (three mechanisms per FR-107), `/v1/telemetry` ingest + retention sweep, Prometheus `/metrics`, README "Telemetry & Privacy" section, and **canary CI tests that prove the privacy invariants mechanically**.

**Independent Test**: With `CANARY_zzz_xyz` embedded in a search query and a fabricated bearer token, run the full corpus → query → response → telemetry path; CI greps every captured log file and every telemetry row, asserts zero occurrences of the canary strings.

### Phase 12 git entry

- [ ] T329 [GIT] Verify working tree is clean before starting Phase 12
- [ ] T330 [US11] Create `retro/P12.md` from retro template

### mn-telemetry — event schemas + opt-out + client

- [ ] T331 [P] [US11] Per-event-type schemas in `crates/mn-telemetry/src/schemas/` (`mcp_tool_call`, `cli_command`, `ingest_complete`, `pull_models`, `mcp_startup`, `mcp_shutdown`) — strict allow-list of fields, types in spec.md table (use devs:rust-dev agent)
- [ ] T332 [P] [US11] Opt-out resolver in `crates/mn-telemetry/src/opt_out.rs` — three mechanisms (env `MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1`, `telemetry.enabled=false` in config, `mnm telemetry disable` write) with documented precedence (FR-107) (use devs:rust-dev agent)
- [ ] T333 [P] [US11] Batching telemetry client in `crates/mn-telemetry/src/client.rs` — in-memory queue, flush every 30s or every 100 events, FIFO drop above 1000-event buffer, local `telemetry_events_dropped` counter reported on next flush (FR-113, US11 acceptance #11) (use devs:rust-dev agent)
- [ ] T334 [US11] Wire telemetry emit sites: every MCP tool call (`mcp_tool_call`), every CLI command (`cli_command`), ingest completion, pull_models, MCP startup/shutdown (use devs:rust-dev agent)
- [ ] T335 [GIT] Commit: mn-telemetry schemas + opt-out + client + emit sites

### mn-server — /v1/telemetry + retention + metrics

- [ ] T336 [P] [US11] Route `POST /v1/telemetry` — NDJSON body, validates each event against its per-`event_type` JSON Schema via `jsonschema`, drops unknown/invalid events with a structured warning, inserts valid events into `telemetry_event_raw` (FR-109, US11 acceptance #7) (use devs:rust-dev agent)
- [ ] T337 [P] [US11] Retention sweep — extend Phase-7 sweep to also delete `telemetry_event_raw` rows older than `MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS` (default 7); roll into `telemetry_aggregate_daily` before deletion (FR-110) (use devs:rust-dev agent)
- [ ] T338 [P] [US11] Route `GET /metrics` — Prometheus exposition with at minimum: `requests_total{route,status,tier}`, `request_duration_seconds_bucket{route,le}`, `source_versions_active`, `embedding_models_in_corpus`, `telemetry_events_received_total{event_type,component}`, `telemetry_events_dropped_total{reason}`, `sweep_runs_total{outcome}` (FR-111) (use devs:rust-dev agent)
- [ ] T339 [GIT] Commit: /v1/telemetry + retention + /metrics

### Logging discipline

- [ ] T340 [P] [US11] All cloud-server log lines emit JSON with required fields per US11 acceptance #1; canary test asserts no human-formatted text mixes in (use devs:rust-dev agent)
- [ ] T341 [P] [US11] MCP server logs to stderr only (stdout is JSON-RPC); CLI diagnostics to stderr; `--json` payloads to stdout. Confirm via canary tests (US11 acceptance #2, #3) (use devs:rust-dev agent)
- [ ] T342 [P] [US11] `request_id` end-to-end correlation — client generates or echoes `X-Request-Id`, cloud propagates through every log line touching the request (US11 acceptance #4) (use devs:rust-dev agent)
- [ ] T343 [GIT] Commit: log discipline + request-id correlation audit

### Canary suite (FR-112, SC-061) — release gate

- [ ] T344 [P] [US11] Canary fixture under `tests/canary/fixtures/` with embedded markers (e.g., `CANARY_zzz_xyz_query`, `CANARY_token_marker`, `CANARY_chunk_content_xyz`, `/Users/cAnArY/path/marker`) (use devs:rust-dev agent)
- [ ] T345 [P] [US11] Canary integration test `tests/canary/query_content_never_logged.rs` — runs a search containing `CANARY_zzz_xyz_query`, captures all server + MCP + CLI log files, greps for the marker, fails on any match (use devs:rust-dev agent)
- [ ] T346 [P] [US11] Canary integration test `tests/canary/tokens_never_logged.rs` — uses `Authorization: Bearer CANARY_token_marker_xyz`, asserts marker absent from logs and telemetry rows (use devs:rust-dev agent)
- [ ] T347 [P] [US11] Canary integration test `tests/canary/chunk_content_never_in_telemetry.rs` — search returns a chunk whose content contains `CANARY_chunk_content_xyz`, asserts the marker is in the HTTP response but NEVER in any captured telemetry row (use devs:rust-dev agent)
- [ ] T348 [P] [US11] Canary integration test `tests/canary/env_values_never_logged.rs` — sets `MIDNIGHT_MANUAL_SECRET=CANARY_env_value_xyz`, exercises code paths, asserts marker absent everywhere (use devs:rust-dev agent)
- [ ] T349 [P] [US11] Canary integration test `tests/canary/paths_never_logged.rs` — asserts no filesystem path from the test machine appears in logs/telemetry (use devs:rust-dev agent)
- [ ] T350 [US11] Wire `.github/workflows/canary.yml` (placeholder from Phase 1) to run the canary suite as a required release gate (use dev-specialisms:init-local-tooling skill)
- [ ] T351 [GIT] Commit: canary fixtures + 5 canary integration tests + canary.yml workflow

### CLI telemetry plumbing

- [ ] T352 [P] [US11] `mnm telemetry status [--json]` — reports `{enabled, endpoint, queue_depth, last_flushed_at, last_drop_count, opt_out_resolved_from}` (US11 acceptance #13) (use devs:rust-dev agent)
- [ ] T353 [P] [US11] `mnm telemetry enable/disable` writes the toggle to user config; emits structured warning event in NDJSON mode (US11 acceptance #14) (use devs:rust-dev agent)
- [ ] T354 [GIT] Commit: CLI telemetry plumbing wired to mn-telemetry

### README discoverability

- [ ] T355 [US11] Flesh out README "Telemetry & Privacy" section (header existed since Phase 1) — what is collected, what is NOT, endpoint URL, three opt-out mechanisms, retention policy, canary-test note (FR-114) (use midnight-docs-writer skill)
- [ ] T356 [GIT] Commit: README Telemetry & Privacy section

### Phase 12 close-out

- [ ] T357 [US11] Run `/sdd:map incremental`
- [ ] T358 [US11] Review `retro/P12.md` and extract conservative learnings to `CLAUDE.md`
- [ ] T359 [GIT] Commit: codebase mapping + retro for Phase 12
- [ ] T360 [GIT] Push and update PR body
- [ ] T361 [GIT] Verify all CI checks pass (incl. canary release gate)
- [ ] T362 [GIT] Report PR ready status — STOP and await LGTM

**Checkpoint**: Privacy invariants are mechanically enforced. All 11 user stories complete.

---

## Phase 13: Polish & Cross-Cutting Concerns

**Purpose**: Cross-story refactoring, performance tuning to the constitutional p95/cold-start budgets, rustdoc + cookbook polish, security hardening, final SC verification.

### Phase 13 git entry

- [ ] T363 [GIT] Verify working tree is clean before starting Phase 13
- [ ] T364 Create `retro/P13.md` from retro template

### Polish

- [ ] T365 [P] Run `cargo bench --workspace` and verify scoring + RRF benches hold to documented budgets (use devs:rust-dev agent)
- [ ] T366 [P] Verify p95 retrieval < 1 s end-to-end (Constitution IV / SC-020) under load via `tests/load/p95_steady_state.rs` (use devs:rust-dev agent)
- [ ] T367 [P] Verify cloud `/v1/search` p95 < 500 ms on a 100k-chunk corpus (SC-013) (use devs:rust-dev agent)
- [ ] T368 [P] Verify MCP cold start < 500 ms (SC-019) and first-call < 2.5 s (SC-020) on each CI runner OS (use devs:rust-dev agent)
- [ ] T369 [P] Verify cloud server cold start < 5 s (SC-036, SC-059) (use dev-specialisms:fly-deploy skill)
- [ ] T370 [P] Rustdoc pass — every public type/function in every crate has a one-line doc; `cargo doc --workspace --no-deps -D warnings` is clean (use devs:rust-dev agent)
- [ ] T371 [P] `cargo deny` configuration in `deny.toml` covering license + advisory + bans + sources; run in CI (use devs:rust-dev agent)
- [ ] T372 [P] `cargo udeps` (nightly) audit — remove unused dependencies (use devs:rust-dev agent)
- [ ] T373 [P] Final supply-chain review with `/supply-chain-defence:review` (use supply-chain-defence:review skill)
- [ ] T374 Run `/midnight-verify:verify` over any Midnight-network claims in the user-facing docs (none expected, but verify) (use midnight-verify:verify skill)
- [ ] T375 [P] Final canary release gate — full run-through with synthetic forbidden strings across every component (use devs:rust-dev agent)
- [ ] T376 [P] `tests/load/concurrent_clients.rs` — N concurrent MCP clients all hit the same cloud server; assert no degraded behavior (use devs:rust-dev agent)
- [ ] T377 [GIT] Commit: polish — perf budgets, rustdoc, deny/udeps, final canary

### Final verification

- [ ] T378 Walk `quickstart.md` end-to-end on a fresh machine (or container) — every command described works as written (SC quickstart-validation)
- [ ] T379 Sanity-check every Success Criteria (SC-001 … SC-066) — open issues for any that fail
- [ ] T380 Sanity-check that the constitution gates in CI all evaluate green (privacy canaries, p95, cold start, recall lifts, MSRV)

### Phase 13 close-out

- [ ] T381 Run `/sdd:map incremental` for final codebase mapping pass
- [ ] T382 Review `retro/P13.md` and extract conservative learnings to `CLAUDE.md`
- [ ] T383 [GIT] Commit: final codebase mapping + retro
- [ ] T384 [GIT] Push and update PR body with v1 completion summary
- [ ] T385 [GIT] Verify all CI checks pass
- [ ] T386 [GIT] Report PR ready status — STOP and await LGTM for v1.0.0 release

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 (Setup)** — no deps
- **Phase 2 (Foundational + US1)** — depends on Phase 1; **BLOCKS** all later phases
- **Phase 3 (US2)** — depends on Phase 2
- **Phase 4 (US4)** — depends on Phase 2 (and benefits from Phase 3's server skeleton being in place, but technically independent)
- **Phase 5 (US5)** — depends on Phase 4 (calls the cloud read API)
- **Phase 6 (US3)** — depends on Phase 3 (reuses ingest plumbing)
- **Phase 7 (US9)** — depends on Phase 3 + Phase 4 (adds auth to existing endpoints + new admin endpoints)
- **Phase 8 (US8)** — depends on Phase 7 (CLI talks to admin endpoints)
- **Phase 9 (US6)** — depends on Phase 4 + Phase 5 (additive to read API + MCP)
- **Phase 10 (US7)** — depends on Phase 4 + Phase 5 + Phase 9 (multi-query lifts work most naturally after scoring exists, but technically independent of Phase 9)
- **Phase 11 (US10)** — depends on all P1/P2 stories existing (release matrix builds all binaries)
- **Phase 12 (US11)** — depends on Phase 5 + Phase 7 (telemetry emit sites + cloud ingest endpoint)
- **Phase 13 (Polish)** — depends on Phases 1–12

### Within-Phase Order

- Migrations before entity modules before queries before integration tests
- mn-core types before everything that uses them
- Crate-level lib code before bin-crate wiring
- Tests for a component land in the same phase commit-block as the component

### Parallel Opportunities

- **Within Phase 2**: T030–T035 (migrations 0001–0006) are independent files; T037–T043 (entity modules) are independent files; T046–T051 (integration tests) all parallel
- **Within Phase 3**: T063–T067 (Markdown/manifest/hash) and T070–T072 (embedding wrapper) and T087–T090 (CLI skeleton commands) are all independent file sets
- **Within Phase 4**: T105–T108 (mn-retrieval modules) and T112–T117 (read routes) and T121–T122 (middleware) are all independent file sets
- **Within Phase 5**: T142 (reranker) and T145–T151 (MCP tools) and T156–T160 (CLI commands) are independent
- **Within Phase 7**: T200–T204 (mn-auth modules), T207–T215 (server routes), T224–T226 (deploy artifacts) all parallel
- **Within Phase 12**: T331–T334 (mn-telemetry), T336–T338 (server endpoints), T344–T349 (canary tests) all parallel

---

## Parallel Example: Phase 2 mn-store entity modules

```bash
# Five independent files — launch in parallel:
Task: "Create source.rs entity module in crates/mn-store/src/entities/source.rs (use devs:rust-dev agent)"
Task: "Create source_version.rs entity module in crates/mn-store/src/entities/source_version.rs (use devs:rust-dev agent)"
Task: "Create embedding_model.rs entity module in crates/mn-store/src/entities/embedding_model.rs (use devs:rust-dev agent)"
Task: "Create node.rs entity module in crates/mn-store/src/entities/node.rs (use devs:rust-dev agent)"
Task: "Create chunk.rs entity module in crates/mn-store/src/entities/chunk.rs (use devs:rust-dev agent)"
```

## Parallel Example: Phase 12 canary tests

```bash
# Five independent canary tests — launch in parallel:
Task: "Canary test: query content never logged in tests/canary/query_content_never_logged.rs (use devs:rust-dev agent)"
Task: "Canary test: tokens never logged in tests/canary/tokens_never_logged.rs (use devs:rust-dev agent)"
Task: "Canary test: chunk content never in telemetry in tests/canary/chunk_content_never_in_telemetry.rs (use devs:rust-dev agent)"
Task: "Canary test: env values never logged in tests/canary/env_values_never_logged.rs (use devs:rust-dev agent)"
Task: "Canary test: paths never logged in tests/canary/paths_never_logged.rs (use devs:rust-dev agent)"
```

---

## Implementation Strategy

### MVP First (Phases 1–5: US1 + US2 + US4 + US5)

1. Phase 1: Setup → workspace builds
2. Phase 2: Foundational + US1 → schema + types stable
3. Phase 3: US2 → maintainers can ingest Markdown
4. Phase 4: US4 → anyone can query the corpus over HTTP
5. Phase 5: US5 → AI agents can retrieve via MCP

After Phase 5 the MVP is live and shippable as a `0.1.x` release. Stop, validate, demo.

### Incremental Delivery

- After Phase 5 → MVP release (`0.1.x`)
- After Phase 6 → code ingest added (`0.2.x`)
- After Phase 7 → authenticated + deployable (`0.3.x`)
- After Phase 8 → full admin CLI (`0.4.x`)
- After Phase 9 → confidence scoring (`0.5.x`)
- After Phase 10 → multi-query + cookbook (`0.6.x`)
- After Phase 11 → continuous-release pipeline (`0.7.x`)
- After Phase 12 → opt-out telemetry + canary CI (`0.8.x`)
- After Phase 13 → v1.0.0

### Parallel Team Strategy

After Phase 2 ships, two streams can run in parallel:
- **Stream A (read path)**: Phase 4 (US4) → Phase 5 (US5) → Phase 9 (US6) → Phase 10 (US7)
- **Stream B (write path)**: Phase 3 (US2) → Phase 6 (US3) → Phase 7 (US9) → Phase 8 (US8)
- Phase 11 (US10) and Phase 12 (US11) can run in parallel after both streams complete
- Phase 13 (Polish) is the join point before v1.0.0

---

## Notes

- `[P]` tasks operate on different files with no incomplete-task dependencies and are safe to dispatch in parallel.
- `[USn]` traces tasks back to the user story that justifies them; setup, foundational close-out tasks, and polish tasks carry no story label.
- `[GIT]` tasks gate the workflow — commits after every logical group, push + PR update + CI verification + ready report at every phase close.
- Tests are not optional in this project: Constitution III mandates integration tests against real components, and the canary suite is a release gate (FR-112, SC-061).
- Commit cadence: after every implementation block in the phase. Pre-commit and pre-push hooks must pass; never `--no-verify`.
- PR strategy: one feature PR (`001-rag-platform`) updated at every phase close. Each phase's "PR READY FOR MERGE. AWAITING LGTM" report is a checkpoint — the human decides whether to actually merge or continue accumulating phases on the branch.
- Total task count: **386 tasks** across **13 phases**.
- Stories addressed: **all 11** (US1–US11).
- Avoid: vague tasks, cross-story dependencies that break independence, premature abstractions, dependencies on future phases' files.

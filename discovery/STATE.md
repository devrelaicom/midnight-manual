# Discovery State: rag-platform

**Updated**: 2026-05-13
**Iteration**: 1
**Phase**: Problem Exploration → Story Crystallization

---

## Problem Understanding

### Problem Statement

Developers building on the Midnight Network need accurate, current, and well-attributed knowledge about Midnight's rapidly-changing surface area (Compact language, SDKs, tooling, protocols) at the point of use — inside their AI-assisted coding workflow. The existing MCP server that attempted this is not fit for purpose; a refactor turned into a near-rewrite, so the project is being restarted as a greenfield Rust implementation. Without a high-quality retrieval surface, developers (and the AI agents they work with) fall back on stale training data, leading to confidently-wrong Compact code, broken SDK calls, and lost time. The Midnight Network is under rapid development with frequent breaking changes — so retrieval must surface not just relevant content but the *right version*, with provenance and verification metadata strong enough to support a confidence score.

### Personas

| Persona | Description | Primary Goals |
|---------|-------------|---------------|
| **DApp Developer (Dev)** | External developer (or agent) building on Midnight. Uses an MCP-capable AI assistant locally. May know nothing about the platform internals. | Get accurate, version-correct answers and code examples inline while coding. Avoid being misled by stale or unverified content. |
| **Content Maintainer (Maintainer)** | Midnight Network staff. Authors and curates content: docs, examples, tutorials. Runs the CLI to ingest, update, and retire content. Has admin credentials to the cloud DB. | Keep the corpus accurate and current. Tag content with attribution and verification status. Make changes scriptable so updates can run in CI. |
| **Operator** | Midnight Network ops/infra. Runs the cloud server, monitors health, manages secrets and rollouts. May overlap with Maintainer. | Reliable hosted service. Clear logs/metrics. Safe deploys. Bounded costs. |
| **Ecosystem Reader** | Partner projects, community contributors, third-party tools. Read-only consumers of the corpus via the MCP server (or potentially direct API later). | Programmatic access to a trustworthy Midnight knowledge base without running their own ingestion. |

### Current State vs. Desired State

**Today (without feature)**:
- Existing MCP server exists but is unfit for purpose (carrying enough technical debt that a refactor became a rewrite).
- Developers and agents fall back on training data that this very session is configured to mistrust — Compact syntax errors, wrong SDK signatures, deprecated imports.
- No structured way to express provenance ("written by the Foundation", "verified on language version 0.23", "uses midnight-js@1.4.0") so agents can't weight results by confidence.
- No reliable way to ask "find me a working counter contract that compiles against current Compact" and get a *current* answer.

**Tomorrow (with feature)**:
- A single Rust codebase produces three deliverables: a `mn-manual` CLI (admin tool), a local `mn-manual-mcp` MCP server (developer-facing), and a cloud HTTP API server (queries + auth for ingestion).
- A hosted Postgres + pgvector database on Fly.io holds chunks with rich hierarchical, navigational, and provenance metadata.
- Hybrid retrieval (FTS + vector) plus reranking returns relevant chunks alongside enough context that callers can reconstruct the surrounding page/guide/repo.
- Confidence scoring derived from metadata (verification status, source attribution, language/SDK version match) ranks results and lets agents explain why they trust an answer.
- Ingestion is fully scriptable (`--json`, no interactive prompts) so the corpus can be refreshed by CI on a schedule when upstream docs/repos change.

### Constraints

**From the user**:
- Rust for all three components (CLI, local MCP server, cloud server).
- Fly.io for hosting; Fly.io managed Postgres with vector extension.
- DB is read-write for Midnight Network staff only; read-only for everyone else.
- Must support both FTS and semantic/vector search.
- Embedding pipeline preferred local (subject to research).
- Chunks must carry hierarchical metadata up to repo/site root and sibling-order metadata within a page/file.
- Code chunks must reconstruct back to source (whitespace tolerance OK).
- Package/module detection minimum: Compact, TypeScript/JavaScript, Rust. Unknown languages fall back to file-path-only metadata.
- CLI: human-readable stdout default + `--json` everywhere + no required interactive prompts (scriptable for CI).
- Re-chunking and re-embedding entire pages on change is acceptable.

**From the Constitution** (`CONSTITUTION.md`):
- p95 retrieval latency < 1 second under nominal cloud-store conditions.
- Cold start < 500ms (process launch → MCP handshake complete).
- MCP contract surface is the product; semver applies; breaking it requires a major bump.
- Trunk-based development, continuous release, Conventional Commits.
- Telemetry opt-out, anonymized, never includes query content / file paths / PII.
- Errors at the MCP boundary must be human-readable and actionable; the server never crashes the client.
- Integration tests against real components preferred over mocks.
- Distribution: `cargo install`, GitHub Releases, Homebrew tap.

**Derived / inferred**:
- Cloud server must front the DB (the DB itself is not exposed publicly — wraps writes behind auth, reads behind a public read API).
- Local MCP server is a thin client to the cloud read API + (likely) a local embedder for query vectors.

---

## Story Landscape

### Story Status Overview

| # | Story | Priority | Status | Confidence | Blocked By |
|---|-------|----------|--------|------------|------------|
| 1 | Content model & metadata schema | P1 | ✅ In SPEC | 100% | Decisions on chunking heuristics, parent inference, package detection rules |
| 2 | Admin ingestion of Markdown content via CLI | P1 | ✅ In SPEC | 50% | Story 1 |
| 3 | Admin ingestion of source-code repos via CLI | P1 | ✅ In SPEC | 40% | Story 1; package-detection rules |
| 4 | Cloud read API: hybrid (FTS + vector) search | P1 | ✅ In SPEC | 50% | Story 1 |
| 5 | Local MCP server: retrieval tools | P1 | ✅ In SPEC | 50% | Story 4; MCP tool contract |
| 6 | Confidence scoring and result ranking | P2 | ✅ In SPEC | 30% | Story 1, 4; scoring policy |
| 7 | Query enhancement: rewriting + reranking | P2 | ✅ In SPEC | 20% | Research outcomes; latency budget |
| 8 | CLI admin lifecycle: update / delete / list / audit | P2 | ✅ In SPEC | 40% | Stories 1–3 |
| 9 | Cloud server auth, deploy, ops | P2 | ✅ In SPEC | 30% | Story 4 |
| 10 | Distribution (cargo / Homebrew / GitHub Releases) | P3 | ✅ In SPEC | 50% | All build artifacts shaped |
| 11 | Observability and telemetry (opt-out, privacy-safe) | P3 | ✅ In SPEC | 40% | All components shaped |

### Story Dependencies

```
                  ┌──────────────────────────┐
                  │ S1: Content model        │
                  │   & metadata schema      │
                  └─────────────┬────────────┘
                                │
              ┌─────────────────┼────────────────────┐
              │                 │                    │
              ▼                 ▼                    ▼
        ┌──────────┐      ┌──────────┐         ┌──────────┐
        │ S2: MD   │      │ S3: Code │         │ S4: Read │
        │ ingest   │      │ ingest   │         │ API      │
        └────┬─────┘      └────┬─────┘         └────┬─────┘
             │                 │                    │
             └────────┬────────┘                    │
                      ▼                             ▼
                ┌──────────┐                  ┌──────────┐
                │ S8: CLI  │                  │ S5: MCP  │
                │ lifecycle│                  │ server   │
                └──────────┘                  └────┬─────┘
                                                   │
                                  ┌────────────────┼────────────────┐
                                  ▼                ▼                ▼
                            ┌──────────┐    ┌──────────┐      ┌──────────┐
                            │ S6:      │    │ S7: Query│      │ S9: Cloud│
                            │ Confidence│    │ enhance  │      │ auth/ops │
                            └──────────┘    └──────────┘      └──────────┘

      S10 (distribution) and S11 (observability) cut across all stories.
```

### Proto-Stories / Emerging Themes
*(All have been promoted into the table above; this section is preserved for trace.)*

---

## Completed Stories Summary

| # | Story | Priority | Completed | Key Decisions | Revision Risk |
|---|-------|----------|-----------|---------------|---------------|
| 1 | Content model & metadata schema | P1 | 2026-05-13 | D1, D4, D6, D7, D8, D9, D12, D13, D14, D15 | **Medium** — likely revisits: (a) cross-version chunk-reuse mechanism if simpler than expected, (b) `package` schema once we ingest the first real code repo, (c) `provenance.language_targets` shape once Story 6 (confidence scoring) builds against it |
| 2 | Admin ingestion of Markdown content via CLI | P1 | 2026-05-13 | D1, D6, D7, D8, D13, D14, D16, D17, D18, D19 | **Low–Medium** — likely revisits: (a) the implicit cloud write protocol once Story 9 designs the concrete HTTP endpoints, (b) manifest schema if Story 3 (code ingest) needs the same hierarchy concepts |
| 4 | Cloud read API — hybrid (FTS + vector) search | P1 | 2026-05-13 | D4, D11, D12, D13, D14 | **Low** — likely revisits: (a) filter syntax once Story 6 (confidence scoring) wants additional ranking signals, (b) endpoint shape once Story 5 (MCP server) consumes the API in anger |
| 5 | Local MCP server — retrieval tools | P1 | 2026-05-13 | D1, D2, D3, D12, D14, D17, D18 | **Low–Medium** — likely revisits: (a) cold-start budget if model load measurements show real-world numbers exceeding the lazy-load amortization estimate, (b) MCP tool schemas once Story 6 (confidence scoring) wants additional return fields |
| 3 | Admin ingestion of source-code repos via CLI | P1 | 2026-05-13 | D1, D6, D8, D9, D13, D14, D16, D17, D18, D19 | **Medium** — likely revisits: (a) the Compact hand-rolled scanner once a tree-sitter Compact grammar lands, (b) default exclusion patterns as we ingest more repos and discover edge cases, (c) the symbol_path schema once Story 6 (confidence scoring) builds against it |
| 9 | Cloud server — auth, write API, deploy, ops | P2 | 2026-05-13 | D10, D11, D15, D20, D21, D22 | **Low–Medium** — likely revisits: (a) rate-limit numeric defaults once real traffic data exists, (b) the role enum if Story 6 (confidence scoring) wants a reviewer role for verification metadata, (c) multi-region deploy posture as traffic grows |
| 8 | CLI admin lifecycle | P2 | 2026-05-13 | D10, D11, D12, D16, D17, D18, D19, D20, D21, D22, D23 | **Low–Medium** — likely revisits: (a) mcp install registry as new agents emerge (cursor, continue, others), (b) doctor report fields as observability requirements grow |
| 6 | Confidence scoring & result ranking | P2 | 2026-05-13 | D2, D4, D14, D24 | **Low** — likely revisits: (a) scoring policy default weights after measurement on real traffic, (b) freshness half-life if user studies show staleness perception differs from 180d, (c) relevance-score normalization if RRF→sigmoid mapping is suboptimal |
| 7 | Query enhancement — multi-query support and cookbook | P2 | 2026-05-13 | D3, D4, D25 | **Low** — likely revisits: (a) max-queries cap if production traffic shows different ergonomics, (b) cookbook patterns as new techniques emerge (e.g. RAG-Fusion, MMR), (c) dedup hash strategy if it produces unexpected collisions |
| 10 | Distribution | P3 | 2026-05-13 | D16, D26 | **Low** — likely revisits: (a) MSRV bump cadence after Cargo ecosystem behavior settles, (b) signing posture once Sigstore is mature enough to justify the operational cost, (c) target matrix if a Linux distro emerges with a meaningful userbase not yet covered |
| 11 | Observability & telemetry | P3 | 2026-05-13 | D27 | **Low** — likely revisits: (a) retention window once real usage shows debugging needs, (b) Prometheus series list as ops requirements grow, (c) potential addition of opt-in detailed-error reporting (currently scoped out) |

*Full stories in SPEC.md*

---

## In-Progress Story Detail

### Story 11: Observability & telemetry (Priority: P3)

**As** an operator running the cloud server (debugging production), a maintainer (understanding which retrieval patterns serve users), and a privacy-conscious developer (deciding whether to trust the telemetry posture),
**I need** structured JSON logging from day one in every component, end-to-end request-id propagation, an opt-out telemetry pipeline that demonstrably never sees query content / tokens / PII, a Prometheus `/metrics` endpoint with documented series, and canary tests in CI that prove the privacy invariants —
**So that** Constitution VII goes from policy to mechanical guarantee, and the project earns the trust it asks ecosystem users to extend.

**Draft Acceptance Scenarios** (graduated manually):

1. **Given** the cloud server processes any HTTP request, **When** it emits log output, **Then** every line is structured JSON with fields `{ts, level, request_id, route, status, latency_ms, tier, error_code (nullable)}`; logs go to stdout (Fly captures); no human-formatted text mixes in.
2. **Given** the MCP server processes any tool call, **When** it emits log output, **Then** every line is structured JSON with fields `{ts, level, request_id, tool_name, latency_ms, model_state, rerank_on, result_count, error_code (nullable)}`; logs go to stderr (stdout is reserved for MCP JSON-RPC).
3. **Given** the CLI runs any command, **When** it emits diagnostics, **Then** structured JSON goes to stderr; with `--json` the command's payload goes to stdout, never mixing diagnostic output into the payload stream.
4. **Given** a CLI/MCP request reaches the cloud, **When** the cloud handles it, **Then** the cloud's response carries `X-Request-Id` and every cloud log line touching that request includes the same `request_id`; the originating client's log line for that request includes the same id (allowing end-to-end correlation across components by `request_id` alone).
5. **Given** telemetry is enabled by default, **When** the MCP server processes tool calls, **Then** events are queued in memory and flushed to the cloud's `POST /v1/telemetry` every 30 seconds (configurable) or when 100 events accumulate, whichever comes first.
6. **Given** `MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1`, or `telemetry.enabled = false` in config, or after `mnm telemetry disable`, **When** any component (CLI/MCP) runs, **Then** zero telemetry events are sent; pending in-memory events are discarded; the client never connects to `/v1/telemetry`.
7. **Given** an anonymous client POSTs an NDJSON batch to `/v1/telemetry`, **When** the cloud validates each event, **Then** events conforming to the per-`event_type` schema are written to the `telemetry_event_raw` table; events with unknown fields or unknown event types are dropped and a structured warning is logged with the offending event_type (a programmer-error signal).
8. **Given** the canary test suite in CI, **When** any forbidden string (query content like `CANARY_zzz_xyz`, fabricated tokens, chunk content samples, file paths) is fed through any code path, **Then** post-run grep against every captured log file and every received telemetry event finds zero occurrences; any match fails the build.
9. **Given** the cloud server is running, **When** `GET /metrics` is hit, **Then** Prometheus exposition format is returned containing: `requests_total{route, status, tier}`, `request_duration_seconds_bucket{route, le}`, `source_versions_active`, `embedding_models_in_corpus`, `telemetry_events_received_total{event_type, component}`, `telemetry_events_dropped_total{reason}`, `sweep_runs_total{outcome}`.
10. **Given** the sweep job (Story 9 FR-063) ticks, **When** it processes telemetry rows, **Then** it deletes `telemetry_event_raw` rows older than `MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS` (default 7); the corresponding aggregate counters (`telemetry_aggregate_daily`) are unaffected.
11. **Given** the cloud's `/v1/telemetry` is unreachable from a client (network error, 5xx), **When** events accumulate beyond the in-memory buffer (default 1000), **Then** the client drops the oldest events (FIFO) and increments a local `telemetry_events_dropped` counter that is itself reported on the next successful flush.
12. **Given** the README's first-run section, **When** a user reads it, **Then** they find a paragraph naming the telemetry endpoint URL, the categories of data collected, the three opt-out mechanisms, and the retention policy — discoverable in plain language per Constitution VII.
13. **Given** `mnm telemetry status [--json]`, **When** run, **Then** it reports `{enabled: bool, endpoint, queue_depth, last_flushed_at, last_drop_count, opt_out_resolved_from}` so an operator can verify both the policy and current runtime state at a glance.
14. **Given** `mnm telemetry disable`, **When** run, **Then** `telemetry.enabled = false` is written to the user config file and a structured warning records the change. `mnm telemetry enable` reverses it.

**Telemetry event schemas** (per `event_type`):

| event_type | component | fields |
|---|---|---|
| `mcp_tool_call` | mcp | tool_name (enum), latency_ms (int), result_count (int), model_state (enum), rerank_on (bool), error_code (nullable enum) |
| `cli_command` | cli | command (enum like "sources.list"), latency_ms, exit_code (int), error_code (nullable enum) |
| `ingest_complete` | cli | source_slug_hash (sha256 of slug), kind (md/code), files_total, chunks_total, embeds_skipped, duration_ms, outcome (enum) |
| `pull_models` | cli | model_name (enum), total_bytes, duration_ms, outcome (enum) |
| `mcp_startup` | mcp | cold_start_ms, model_state |
| `mcp_shutdown` | mcp | uptime_s, tool_calls_served, model_state_at_shutdown |

`source_slug_hash` is intentionally a hash of the slug rather than the slug itself — even source identifiers (which could carry organizational meaning) are not stored on the telemetry side.

Unknown fields cause the event to be dropped; unknown event_types are dropped; events failing schema are dropped with a structured warning.

**Schema for stored telemetry**:

```
telemetry_event_raw
  id            uuid PK
  received_at   timestamptz  (server-side; client-supplied timestamp is informational only)
  event_type    text
  component     text
  version       text         (component version, e.g. "1.2.3")
  fields        jsonb        (validated against per-event-type schema before insert)
  request_id    text NULL    (when present, allows cross-component join; never user-identifying)
  -- auto-deletes after MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS (default 7)

telemetry_aggregate_daily
  day           date
  event_type    text
  component     text
  count         bigint
  PRIMARY KEY (day, event_type, component)
  -- retained indefinitely
```

**README discoverability** (Constitution VII):

The repo's README MUST include a top-level "Telemetry & Privacy" section answering:
- What is collected (linked event-type table)
- What is NOT collected (query content, tokens, PII, paths, env values, secrets)
- Where it goes (the cloud endpoint URL of the deployed instance)
- How to opt out (three mechanisms; same as `mnm --help` mentions)
- How long records are kept (7 days raw, aggregates kept)
- Privacy canary tests (a paragraph reassuring the reader that CI enforces this)

**Forbidden in logs / telemetry** (canary set):

- The verbatim text of any user query
- The verbatim text of any returned chunk
- Bearer tokens, JWTs, API keys, signing secrets
- Filesystem paths from a user's machine
- IP addresses (in event rows; IPs are used transiently for rate-limit accounting and not stored on event rows)
- Email addresses or user identifiers in event rows

**As** a developer (installing the CLI/MCP server on a fresh laptop), as a Midnight Network operator (shipping the cloud server to Fly.io), and as a maintainer (running the release pipeline),
**I need** every deployable artifact to ship from one continuous-release pipeline on every commit to main — with `cargo install`, Homebrew tap, GitHub Releases, and a Docker image all built from the same SHA —
**So that** install is one command for any user (Constitution IV), versions are traceable across channels, and the team merges and releases at a sustainable cadence (Constitution IX).

**Draft Acceptance Scenarios** (graduated manually):

1. **Given** the repo is at version vX.Y.Z, **When** I run `cargo install midnight-manual --version vX.Y.Z`, **Then** two binaries are installed in `$CARGO_HOME/bin/`: `midnight-manual` and `mnm`; running `mnm version --json` reports `{version: "X.Y.Z", commit: "<sha>", build_date: "<date>"}`.
2. **Given** the same release, **When** I run `brew install midnight-network/tap/midnight-manual`, **Then** the same two binaries are installed under the Homebrew prefix; `mnm version` reports the same `version` and `commit` as the cargo install would.
3. **Given** the same release, **When** I download `midnight-manual-vX.Y.Z-<target>.tar.gz` from GitHub Releases, **Then** I extract a directory containing both binaries plus a `SHA256SUMS` file and a `LICENSE`; verifying `sha256sum -c SHA256SUMS` succeeds.
4. **Given** a PR is merged to `main` whose Conventional-Commit messages include any `feat:`, `fix:`, or `BREAKING CHANGE`, **When** release-please runs on the merge, **Then** it opens (or updates) a release PR containing the version bump, CHANGELOG.md additions, and the manifest updates for cargo/brew.
5. **Given** the release PR is merged, **When** the release workflow fires, **Then** in one CI run: (a) a git tag is created at the merge SHA, (b) cross-compiled binaries are built for all targets in the matrix, (c) artifacts are uploaded to a GitHub Release with checksums, (d) the crate is published to crates.io, (e) the Homebrew formula in `midnight-network/homebrew-tap` is updated with the new URL and SHA, (f) a multi-arch Docker image is pushed to ghcr.io tagged with both the version and `:latest`, (g) the Fly.io app is deployed from the same image.
6. **Given** a target in `{linux-x86_64-gnu, linux-x86_64-musl, linux-aarch64-gnu, linux-aarch64-musl, darwin-x86_64, darwin-aarch64, windows-x86_64-msvc}`, **When** the release pipeline builds, **Then** every target produces a tarball (or zip on Windows) containing both binaries; the build matrix runs in parallel.
7. **Given** the Cargo.toml `rust-version = "1.NN"` field (MSRV pin), **When** CI runs on every PR, **Then** the test matrix exercises both the MSRV toolchain and `stable`; a build failure on either fails the PR.
8. **Given** the released crate, **When** I run `mnm version` after a manual install, **Then** the version reported matches both `Cargo.toml` `package.version` and the git tag at the released SHA.
9. **Given** a contributor opens a PR that breaks the MCP tool contract (per Constitution I), **When** release-please tries to generate the next version bump, **Then** the commit's `!` or `BREAKING CHANGE:` footer forces a MAJOR bump and the release PR explicitly calls out the breaking change in the CHANGELOG.
10. **Given** the GitHub Release artifacts are published, **When** any of them is downloaded over the next 90 days, **Then** GitHub serves the exact same bytes (immutability invariant — releases are never edited in place; corrections ship a patch release).
11. **Given** an installer (cargo, brew, GitHub binary) is used on each supported OS+arch, **When** the user runs `mnm doctor --json` immediately after install with no further setup, **Then** the report shows: `cli_version` populated, `models.state = "missing"`, `mcp.installation = "not installed"`, and exits 0 (post-install state is healthy-but-uninitialized).
12. **Given** the Docker image at `ghcr.io/midnight-network/midnight-manual:vX.Y.Z`, **When** Fly.io deploys it, **Then** the container starts the `midnight-manual-server` binary; the user-facing CLI binaries (`midnight-manual`, `mnm`) are not present in the server image (smaller image, fewer attack surface).

**Distribution channel matrix**:

| Channel | Artifacts | Built from | Updated by |
|---|---|---|---|
| crates.io | `midnight-manual` crate (publishes `midnight-manual`, `mnm`, NOT `midnight-manual-server`) | Release tag | release pipeline |
| Homebrew tap (`midnight-network/homebrew-tap`) | Formula referencing GitHub Release tarballs (mac + linux) | Release tag | release pipeline (PR to tap repo) |
| GitHub Releases | Prebuilt `midnight-manual-vX.Y.Z-<target>.tar.gz` for every target; `SHA256SUMS` | Release tag | release pipeline |
| GHCR (`ghcr.io/midnight-network/midnight-manual`) | Multi-arch Docker image of `midnight-manual-server` | Release tag | release pipeline |
| Fly.io | Deploy of the GHCR image | Release tag | release pipeline (deploy step) |

**Build target matrix**:

- `x86_64-unknown-linux-gnu` (glibc) and `x86_64-unknown-linux-musl` (static)
- `aarch64-unknown-linux-gnu` and `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin` and `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

(7 user-facing targets. Docker server image: `linux/amd64` + `linux/arm64`.)

**Release pipeline tooling**:

- **release-please** (or equivalent: googleapis/release-please-action) for Conventional-Commit-driven version bumps and CHANGELOG generation.
- **cargo-dist** for the binary cross-compile matrix + checksum generation + GitHub Release upload. Fallback: hand-rolled `cargo build --target` matrix in GitHub Actions if cargo-dist proves limiting.
- **homebrew-releaser** action (or hand-rolled formula update) to push the formula change to the tap repo.
- **docker buildx** for the multi-arch server image.
- **flyctl deploy** for the Fly.io deploy step, gated on Docker image push success.

**Versioning**:

- Strict semver per Constitution X.
- MAJOR: MCP tool contract break, CLI flag break, or cloud HTTP endpoint shape break.
- MINOR: additive capabilities (new tool, new flag, new endpoint, new optional response field).
- PATCH: bug fixes, internal refactors with no contract impact.
- MSRV bumps are MINOR (Cargo ecosystem convention).
- Version stamped into the binary at build time and reported by `mnm version`.

**Signing / supply-chain (deferred to a follow-up release)**:

- Sigstore / cosign signing of GitHub Release artifacts is documented as v1.next, not v1.
- The release pipeline still emits SHA-256 checksums for every artifact.
- `cargo-vet` or `cargo-audit` runs on every PR as a basic supply-chain guard.

**As** an AI agent calling the MCP server (and the Midnight Network team supporting agent developers),
**I need** the multi-query input shape from D3 to be explicitly specified, fairly rate-limited, well-instrumented, and well-documented with worked examples in a shipped cookbook —
**So that** sophisticated callers can apply HyDE, multi-query expansion, and step-back prompting to lift recall, while casual callers continue to work unchanged and abusive inputs are bounded.

**Draft Acceptance Scenarios** (graduated manually):

1. **Given** `POST /v1/search` with `queries: [{text, vector}]` (N pairs, 1 ≤ N ≤ 10), **When** the server processes the request, **Then** hybrid retrieval runs once per query pair (FTS + pgvector), and RRF (k=60) merges across both retrieval modes and across query pairs in one pass.
2. **Given** the merged candidate set, **When** a result is returned, **Then** `scores.matched_queries` lists the input query indices (0-based) that contributed at least one of FTS/vector rank to the result.
3. **Given** any multi-query response, **When** the response is built, **Then** `search_metadata.per_query` carries one record per input query: `{query_index, fts_candidates, vector_candidates, fts_latency_ms, vector_latency_ms}`.
4. **Given** a request with `queries.length > MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST` (default 10), **When** the request is validated, **Then** the server returns 400 `invalid_request` naming the cap, before consuming any rate-limit tokens.
5. **Given** a multi-query request with `queries.length = N`, **When** the rate-limiter accounts for it, **Then** the request consumes `max(1, N)` tokens from the caller's bucket per D25; the `X-RateLimit-Remaining` header reflects the post-charge balance.
6. **Given** a single-query convenience form (`{query: "text", vector: [...]}`), **When** the server processes it, **Then** behavior is identical to passing `queries: [{text: "text", vector: [...]}]`; this is verified by an internal test that produces byte-identical responses for the two shapes.
7. **Given** `queries: []` or every entry has an empty `text` field, **When** the request is validated, **Then** the server returns 400 `invalid_request`.
8. **Given** the MCP server exposes its `search` tool, **When** an agent inspects the tool description, **Then** the description includes a "Patterns" section documenting three named techniques — `hyde`, `multi_query`, `step_back` — each with a 1–3 line example.
9. **Given** the repo's `docs/cookbook/query-enhancement.md` file, **When** a contributor or third-party agent author opens it, **Then** they find a runnable cookbook with worked examples for HyDE, multi-query paraphrase, and step-back prompting, each showing the LLM prompt(s) the calling agent emits and the resulting `queries` array passed to the MCP `search` tool.
10. **Given** a benchmark consisting of 50 labelled query/relevant-chunk pairs, **When** retrieval is measured under (a) single-query and (b) 3-query multi-query (expansion paraphrases) at the same `limit=10`, **Then** the multi-query recall@10 exceeds single-query recall@10 by at least 8 percentage points absolute.
11. **Given** an admin-mode rate-limit override is active for the caller, **When** a multi-query request arrives, **Then** the override's `limit_rps` applies to the post-multiplied cost (so a 200-req/s override accommodates ~40 five-query requests per second).
12. **Given** `--json` mode on the CLI's `mnm search` (debug) command supports multi-query input via `--query` repeated or via a JSON-on-stdin form, **When** invoked, **Then** the CLI emits the same per-query and per-result diagnostics the cloud returns.

**Cookbook content shape** (`docs/cookbook/query-enhancement.md`, shipped in the repo):

1. **HyDE (Hypothetical Document Embeddings)** — agent prompts an LLM to write a hypothetical answer to the user's question; the answer becomes a second query. Improves recall when the user's question is short or jargon-light.
2. **Multi-query expansion** — agent prompts an LLM to paraphrase the question 2–3 ways (different vocabulary, broader and narrower phrasings); all variations are passed as `queries`. Improves recall when synonyms matter.
3. **Step-back prompting** — agent generates one more abstract version of the question (e.g. "how does Compact module declaration work?" → "what is Compact's module system?"); both queries go in. Improves recall when the user asked an over-specific question.
4. Each pattern has: when to use it, an example LLM prompt, the resulting `queries` array, and a note on rate-limit cost (D25).

**MCP `search` tool description** (an addition to the Story 5 schema, not a structural change):

The description block accompanying the `search` tool MUST end with a "Patterns" subsection listing the three named techniques with one-line examples, so an LLM reading the tool catalog discovers the pattern without external docs.

**CLI multi-query shape** (admin debug helper):

```
mnm search "primary text query" --query "alt 1" --query "alt 2" [--limit N]
# or
mnm search --queries-stdin    # reads JSON {queries: [...]} from stdin
```

Both forms support `--json` output containing per-query and per-result diagnostics from `search_metadata`.

**As** a developer (or AI agent) consuming search results, and as a Midnight Network maintainer tuning corpus quality,
**I need** every result to carry a confidence score that blends content trust (from provenance) with retrieval relevance (from hybrid + reranker), plus a per-factor breakdown that lets the consumer explain *why* a result is trustworthy or weak,
**So that** agents can prefer trusted answers over barely-relevant guesses, surface freshness and verification information in citations, and maintainers can tune the corpus by changing scoring policy rather than code.

**Draft Acceptance Scenarios** (graduated manually):

1. **Given** a search request, **When** the cloud computes results, **Then** every result carries `trust_score ∈ [0.0, 1.0]`, `confidence ∈ [0.0, 1.0]`, and `confidence_factors` (object naming the factor inputs); these fields are additive to the existing per-result schema (no contract break).
2. **Given** two results for the same query identical except for `provenance.attribution`, **When** scored, **Then** the foundation-attributed result has a higher `trust_score` than the partner result, which is higher than third_party, which is higher than community, which is higher than unknown.
3. **Given** two results identical except for `provenance.verified`, **When** scored, **Then** the verified result has a higher `trust_score` than the unverified one; the multiplier comes from the loaded scoring policy.
4. **Given** two results identical except for `source_modified_at` (one is 14 days old, one is 2 years old), **When** scored, **Then** the fresher result has a higher `trust_score`; freshness decays exponentially with a configurable half-life (default 180 days).
5. **Given** a result whose `provenance.deprecation.is_deprecated = true`, **When** scored, **Then** its `trust_score` is reduced by a significant deprecation penalty (default ×0.3); the result still appears unless filtered out by `min_confidence`.
6. **Given** a search request with `filters.language_target.version_constraint_satisfies = "0.31"`, **When** scored, **Then** results whose `provenance.language_targets` satisfy that constraint receive a version-match boost; results that miss the constraint receive a version-miss penalty; results with no language_targets are neutral.
7. **Given** `rerank=false` on the MCP search call, **When** the cloud composes its response, **Then** `confidence` is computed using the normalized RRF score as the relevance term; the MCP server passes the cloud's confidence through unchanged.
8. **Given** `rerank=true` on the MCP search call, **When** the MCP server processes results, **Then** it replaces the relevance term with the normalized reranker score and recomputes `confidence` using the same blend formula; `confidence_factors.relevance_source = "rerank"` records the substitution.
9. **Given** a search request with no explicit `sort_by`, **When** results are ranked, **Then** they are returned sorted by `confidence` descending. With `sort_by = "trust"` they are sorted by `trust_score`. With `sort_by = "relevance"` they are sorted by the relevance term used. With `sort_by = "score"` they are sorted by the underlying RRF score (existing behavior).
10. **Given** a search request with `min_confidence = 0.5`, **When** results are filtered, **Then** results below 0.5 confidence are excluded before the limit is applied; `search_metadata.filtered_by_confidence` reports the count dropped.
11. **Given** the cloud server starts up, **When** `MIDNIGHT_MANUAL_SCORING_POLICY` points at a valid TOML file, **Then** the policy is loaded and validated; absence falls back to compiled-in defaults; invalid policy fails startup (Constitution VI / VIII).
12. **Given** an MCP agent inspects `confidence_factors` for a returned chunk, **When** building an explanation for the user, **Then** the breakdown carries enough information to write a sentence like "this is from the Foundation, verified on 2026-04-01, last updated 14 days ago, targets Compact ≥ 0.31" without further API calls.
13. **Given** the scoring policy weights produce a value outside [0.0, 1.0], **When** the cloud finishes computing `confidence` or `trust_score`, **Then** the value is clamped to the range, and a structured warning is logged (Constitution VI — programmer-error class).

**Scoring policy TOML schema** (default loaded from compiled-in fallback if `MIDNIGHT_MANUAL_SCORING_POLICY` is unset):

```toml
schema_version = 1

[attribution]                 # multipliers in [0,1+]
foundation  = 1.00
partner     = 0.85
third_party = 0.60
community   = 0.40
unknown     = 0.30

[verification]
verified_by_foundation = 1.00
verified_by_partner    = 0.90
verified_by_other      = 0.80
unverified             = 0.70

[freshness]
half_life_days = 180.0        # exp(-age_days / half_life_days)
fallback_age_source = "ingested_at"  # when source_modified_at is null

[deprecation]
penalty_multiplier = 0.30     # multiplies trust_score by this when deprecated

[version_match]
satisfies   = 1.15            # boost when query constraint satisfied
neutral     = 1.00            # no constraint in query, or no language_targets in chunk
unsatisfied = 0.70            # query specified a constraint and chunk fails it

[blend]
# confidence = (trust_score ^ trust_weight) * (relevance ^ relevance_weight)
trust_weight     = 0.55
relevance_weight = 0.45
# Both axes are clamped to [0,1] before exponentiation.
```

The TOML is loaded once at startup; weights are validated for finite, non-negative values; unknown keys fail the load (Constitution VIII fail-fast).

**Trust score computation** (server-side, per result):

```
base = attribution_multiplier(provenance.attribution)
ver  = verification_multiplier(provenance.verified, provenance.verified_by)
fresh = exp(-age_days / half_life_days)             # age from source_modified_at, else ingested_at
dep  = deprecation_multiplier(provenance.deprecation.is_deprecated)
vmatch = version_match_multiplier(query_filters.language_target, provenance.language_targets)

trust_score = clamp(base * ver * fresh * dep * vmatch, 0.0, 1.0)
```

**Relevance term**:
- When the cloud returns the response: normalized RRF score (`1 - 1/(1+rrf_raw)` or similar bounded mapping, TBD in implementation).
- When the MCP server reranks: normalized cross-encoder score from `bge-reranker-base` (sigmoid-mapped to [0,1]).

**Confidence**:
```
confidence = clamp(trust_score ^ trust_weight * relevance ^ relevance_weight, 0.0, 1.0)
```

**Returned per result** (additive to Story 4 shape):

```json
{
  "trust_score": 0.91,
  "confidence": 0.87,
  "confidence_factors": {
    "attribution": "foundation",
    "attribution_multiplier": 1.00,
    "verified": true,
    "verified_by": "midnight-foundation",
    "verification_multiplier": 1.00,
    "age_days": 14,
    "freshness_multiplier": 0.948,
    "deprecation": false,
    "deprecation_multiplier": 1.00,
    "language_target_query": { "name": "compact", "version_constraint_satisfies": "0.31" },
    "language_targets_chunk": [{ "name": "compact", "version_constraint": ">=0.23" }],
    "version_match_multiplier": 1.15,
    "relevance_source": "rerank",
    "relevance_multiplier": 0.873
  }
}
```

**Revisions to existing graduated stories**:

This story produces additive revisions (no contract breaks) to:
- **Story 4 (Cloud read API)**: per-result response shape gains `trust_score`, `confidence`, `confidence_factors`; request body accepts `sort_by` and `min_confidence`.
- **Story 5 (MCP server)**: search result shape gains the same fields; MCP server recomputes `confidence` from the reranker score when rerank=true.

Both are forward-compatible: existing callers ignoring the new fields continue to work unchanged.

**As** a Midnight Network admin (running the CLI interactively or in CI), and as a DApp developer (running the same binary for model and MCP setup),
**I need** a complete admin-facing command surface — version lifecycle, user/key management, ratelimit overrides, model lifecycle, MCP installation, diagnostics, login, and migration preflight — with admin commands cleanly hidden from default help output so developers see a small, focused surface,
**So that** every operation in the system has a scriptable, audited CLI command and the two audiences don't pollute each other's --help.

**Draft Acceptance Scenarios** (graduated manually):

1. **Given** the default config (no admin mode), **When** I run `mnm --help`, **Then** the output lists only developer-facing commands: `search`, `sources list`, `sources show`, `versions list`, `versions show`, `models`, `mcp`, `doctor`, `config`. Admin commands (`users`, `keys`, `ratelimits`, `sources add/update/retire`, `versions promote/rollback/retire`, `ingest`, `login`, `db`) are NOT shown.
2. **Given** `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` or `cli.show_admin_cmds = true` in config, **When** I run `mnm --help`, **Then** every command is listed, including admin commands.
3. **Given** admin commands are hidden, **When** I run `mnm users list` directly, **Then** it executes normally — visibility never blocks invocation (D23).
4. **Given** I have not run `mnm keys generate`, **When** I run `mnm login --user-id aaron` against a server, **Then** the CLI emits an error pointing at `mnm keys generate` (no local keypair found). When a keypair exists, the CLI completes the challenge-response, caches the resulting JWT in the OS keychain (or `~/.config/midnight-manual/tokens.json` as fallback), and prints `logged in as aaron, token expires in 60m`.
5. **Given** I run `mnm keys generate`, **When** the command executes, **Then** an Ed25519 keypair is written to `$XDG_CONFIG_HOME/midnight-manual/keys/<user_id>.{public,private}` with permissions `0600` on the private half; the public half is also echoed to stdout in the TOML row format ready to paste into the user-store TOML.
6. **Given** I run `mnm users add --user-id ci-bot --role writer --public-key ed25519:abc... --note "CI pipeline"`, **When** processed, **Then** the local user-store TOML at the path resolved per D18 is updated; the file's `schema_version` is preserved; the CLI emits a warning that the change is local only and pointing at the deploy step needed (D20).
7. **Given** I run `mnm users list` (or `update`, `show`, `remove`), **When** processed, **Then** the local user-store TOML is read/edited accordingly; with `--json` the output is the user store as a JSON document.
8. **Given** I run `mnm versions promote midnight-docs --revision 12`, **When** processed against a logged-in admin context, **Then** the CLI calls `POST /v1/sources/midnight-docs/versions/12/promote` (Story 9 endpoint), and on success prints `promoted revision 12; demoted revision 13`.
9. **Given** I run `mnm versions rollback midnight-docs`, **When** processed, **Then** the CLI looks up the most recent prior active version (revision N-1) and calls the same promote endpoint with that revision; if no prior version exists, the command exits non-zero with a clear error.
10. **Given** I run `mnm versions retire midnight-docs --revision 9`, **When** processed, **Then** the CLI calls `POST /v1/sources/midnight-docs/versions/9/retire`; the version is marked retired and becomes eligible for sweep (Story 9 FR-063).
11. **Given** I run `mnm ratelimits add --cidr 169.155.237.15/25 --limit 200/s --ttl 48h --note "hackathon-london"`, **When** processed, **Then** the CLI calls `POST /v1/admin/ratelimits` (Story 9 endpoint). `mnm ratelimits list`, `mnm ratelimits extend <id> --ttl 24h`, and `mnm ratelimits remove <id>` use the corresponding endpoints.
12. **Given** I run `mnm models pull`, **When** processed, **Then** the CLI fetches `GET /v1/models/active` to learn the corpus model, downloads the embedding model and reranker model to `$XDG_DATA_HOME/midnight-manual/models/`, verifies digests, and prints a summary. `mnm models list` enumerates locally-installed models; `mnm models prune` removes models not matching the active corpus model (with `--keep <name>` to override).
13. **Given** I run `mnm mcp install [--agent claude-code|cursor|...] [--config-path <path>]`, **When** processed, **Then** the CLI updates the named agent's MCP config file (or prints the JSON snippet for manual install when the agent isn't recognized); on success, prints the agent's config path and the snippet that was applied.
14. **Given** I run `mnm doctor`, **When** processed, **Then** the CLI emits a structured diagnostic report covering: CLI version, embedding & reranker model presence + version, MCP server installation status across known agents, cloud server reachability (HEAD `/healthz`), corpus model match status, local keypair presence, login state, admin-mode visibility flag, and config file location. With `--json` the report is a single JSON object.
15. **Given** I run `mnm db migrate` (admin), **When** processed, **Then** the CLI executes pending migrations against the configured `DATABASE_URL`; intended for deploy-time preflight when `MIDNIGHT_MANUAL_AUTO_MIGRATE=false` (D22). `mnm db status` prints applied vs pending migrations.
16. **Given** I run any command with `--json`, **When** processed, **Then** all output goes to stdout as a single JSON document (single-record commands) or NDJSON (streaming/progressive commands); no human-formatted text touches stdout (FR-021).

**Complete CLI command tree** (this story finalizes; earlier stories introduced subsets):

```
mnm
├── search <query> [--limit N] [--rerank]        # developer; debug helper that hits cloud /v1/search
├── sources
│   ├── list                                      # developer
│   ├── show <slug>                               # developer
│   ├── add <slug> --kind ... [--origin-url ...] # admin
│   ├── update <slug> [...]                       # admin
│   └── retire <slug>                             # admin
├── versions
│   ├── list <slug>                               # developer
│   ├── show <slug> <revision>                    # developer
│   ├── promote <slug> --revision N               # admin
│   ├── rollback <slug>                           # admin (promotes most-recent prior active)
│   └── retire <slug> --revision N                # admin
├── ingest                                        # admin (entire subtree)
│   ├── md <slug> <path> [...]                    # Story 2
│   └── code <slug> <path> [...]                  # Story 3
├── models
│   ├── pull [--name <model>]                     # developer
│   ├── list                                      # developer
│   └── prune [--keep <name>]                     # developer
├── mcp
│   ├── install [--agent <name>] [--config-path <path>]  # developer
│   └── status                                    # developer
├── users                                         # admin (edits local user-store TOML; D20)
│   ├── add --user-id ... --role ... --public-key ...
│   ├── list
│   ├── show <user_id>
│   ├── update <user_id> [...]
│   └── remove <user_id>
├── keys                                          # admin
│   ├── generate [--user-id <id>]
│   └── import --user-id ... --public-key ...
├── ratelimits                                    # admin
│   ├── add --cidr ... --limit ... --ttl ... [--note ...]
│   ├── list
│   ├── extend <id> --ttl ...
│   └── remove <id>
├── login --user-id ...                           # admin (challenge-response per Story 9)
├── logout                                        # admin (clears local token cache)
├── db                                            # admin
│   ├── migrate                                   # preflight migration runner (D22)
│   └── status
├── config
│   ├── show [--effective]                        # developer; --effective resolves env+flag overrides
│   ├── get <key>                                 # developer
│   └── set <key> <value>                         # developer (writes the user config file)
├── doctor                                        # developer (universal diagnostic)
└── version                                       # developer
```

**Visibility rules (D23)**:

- Admin commands hidden by default: `sources add/update/retire`, `versions promote/rollback/retire`, the entire `ingest`, `users`, `keys`, `ratelimits`, `login`, `logout`, `db` subtrees.
- Visible by default: `search`, `sources list/show`, `versions list/show`, `models`, `mcp`, `config`, `doctor`, `version`.
- Toggle: `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` env or `cli.show_admin_cmds = true` in config (env wins per D18). Toggle affects help output only; invocation is never gated.
- `mnm doctor` always reports the active admin-visibility state.

**Local user-store editing (D20 contract)**:

The `mnm users` subtree edits a local TOML file. After every mutation, the CLI emits a warning reminding the admin that the change does not take effect until the file is deployed (Fly secret update + redeploy). With `--json`, the warning is a structured event in NDJSON output rather than human text on stderr. The CLI MUST refuse to overwrite a file whose `schema_version` doesn't match the binary's supported version.

**As** a Midnight Network operator (and the admins and developers consuming the read and write surfaces),
**I need** the cloud server to materialize the implicit write protocol from Stories 2 and 3, implement the auth flows from D10 and D11, expose admin operations for CIDR overrides, and ship as a deployable Fly.io artifact with sane sweep jobs, health endpoints, and migrations —
**So that** Stories 2/3 have a concrete endpoint to talk to, the read API has working rate-limiting tiers, the corpus self-maintains under retention rules, and the entire service can be deployed and rolled back by a single CI pipeline.

**Draft Acceptance Scenarios** (graduated manually):

1. **Given** a valid Ed25519 keypair registered in the deployed user store (D10/D20), **When** the CLI runs `mnm login`, **Then** the CLI calls `POST /v1/auth/challenge` (announces user_id, receives challenge nonce), signs the nonce, calls `POST /v1/auth/verify` (sends user_id + signature), receives a 1-hour HS256 JWT, and caches it in the OS keychain.
2. **Given** an admin holds a valid JWT (D21), **When** the CLI calls `POST /v1/sources/{slug}/ingest-runs`, **Then** the server creates a `source_version` in `building` state and returns `{ingest_run_id, source_version_id, source_version_revision}`; the response is rejected with 401 if the JWT is missing or invalid, and 403 if the user lacks the required role.
3. **Given** an admin has an active ingest_run, **When** they call `PUT /v1/sources/{slug}/ingest-runs/{id}/documents` with a batch of `{document, chunks}` pairs, **Then** the server inserts rows under the run's `source_version_id`; replaying the same batch (same content_hashes) is a 200 no-op (idempotent on hash).
4. **Given** an admin completes uploads, **When** they call `POST /v1/sources/{slug}/ingest-runs/{id}/finalize`, **Then** in one DB transaction the new source_version is set `is_active=true` and the prior active version is demoted; the response carries `{source_version_id, revision, is_active: true, demoted_revision}`.
5. **Given** an admin aborts an ingest_run, **When** they call `POST .../abort`, **Then** the server marks the source_version `aborted`; subsequent PUT/finalize calls on that run id return 409 with a typed `run_aborted` error.
6. **Given** an unauthenticated read request, **When** `POST /v1/search` is called, **Then** it succeeds at the anonymous rate-limit tier (per-IP, from D11) with appropriate `X-RateLimit-*` headers; rate-limit decisions consult CIDR overrides first, then SSO tier, then anonymous (per FR-031).
7. **Given** a user clicks "sign in with GitHub" in a future UI (or runs `mnm read-token github`), **When** `GET /v1/auth/github/start` is hit, **Then** the server redirects to GitHub OAuth; the `callback` exchanges the code, verifies the user is a member of the configured Midnight GitHub org, and mints a longer-TTL bearer token for the read uplift tier.
8. **Given** an admin holds a JWT and calls `POST /v1/admin/ratelimits` with `{cidr, limit_rps, expires_at, note}`, **When** processed, **Then** a `rate_limit_override` row is created and immediately effective; `GET /v1/admin/ratelimits` lists active overrides; `PATCH` extends one; `DELETE` removes one.
9. **Given** the server starts up, **When** it boots, **Then** it (a) loads the user store from the path resolved via `MIDNIGHT_MANUAL_USER_STORE` (mandatory env), (b) loads the JWT signing secret from `MIDNIGHT_MANUAL_JWT_SECRET` (mandatory env), (c) connects to the database, (d) runs pending migrations (unless `MIDNIGHT_MANUAL_AUTO_MIGRATE=false`), (e) seeds `embedding_model` with the active model row if absent, and (f) starts the HTTP listener; any of (a)–(d) failing exits the process non-zero with a structured error to stderr.
10. **Given** a source_version has been inactive for the configured grace window (default 24h after retention demoted it), **When** the periodic sweep job runs, **Then** it deletes the version's chunks, documents, nodes, and packages in dependency order in a single transaction, and removes the source_version row.
11. **Given** the server has been running for > 1h with no successful DB query, **When** the `/readyz` endpoint is hit, **Then** the server reports 503 with the most recent DB error in the typed error body; `/healthz` still reports 200 (process is alive).
12. **Given** a request body or query causes any of the documented error codes, **When** processed, **Then** the response follows the typed error envelope from Story 4 (`{error: {code, message, remediation, context}, request_id}`) with the appropriate HTTP status.
13. **Given** the JWT signing secret is rotated (e.g. via a Fly.io secret update + redeploy), **When** the new process starts, **Then** all previously-issued admin tokens fail verification; admins re-authenticate by re-running `mnm login`.
14. **Given** a request arrives with a JWT signed by a different secret (or expired, or with an invalid signature), **When** processed, **Then** the server returns 401 `unauthorized` with remediation `Run 'mnm login' to obtain a fresh token`.

**Endpoint surface introduced by this story** (additions to the surface in Story 4):

```
# Auth
POST /v1/auth/challenge                              # body {user_id} -> {nonce, expires_at}
POST /v1/auth/verify                                 # body {user_id, signature, nonce} -> {jwt, expires_at}
GET  /v1/auth/github/start                           # 302 to GitHub
GET  /v1/auth/github/callback                        # body {bearer_token, expires_at}

# Write API (admin JWT required)
POST   /v1/sources                                   # create a source
PATCH  /v1/sources/{slug}                            # update source metadata
POST   /v1/sources/{slug}/retire                     # retire a whole source
POST   /v1/sources/{slug}/ingest-runs                # start an ingest run
PUT    /v1/sources/{slug}/ingest-runs/{id}/documents # batch upload
POST   /v1/sources/{slug}/ingest-runs/{id}/finalize  # atomically promote
POST   /v1/sources/{slug}/ingest-runs/{id}/abort     # abandon
POST   /v1/sources/{slug}/versions/{rev}/promote     # rollback to a prior version
POST   /v1/sources/{slug}/versions/{rev}/retire      # retire a specific historical version

# Admin (admin JWT required)
POST   /v1/admin/ratelimits                          # CIDR override CRUD
GET    /v1/admin/ratelimits
PATCH  /v1/admin/ratelimits/{id}
DELETE /v1/admin/ratelimits/{id}

# Diagnostics
GET    /metrics                                      # Prometheus (Story 11 may extend)
```

**User store TOML schema** (loaded once at startup; D20):

```toml
schema_version = 1

[[users]]
user_id = "aaron"
role = "admin"          # admin | writer | (future roles)
public_key = "ed25519:base64..."
created_at = "2026-05-13"
note = "founding admin"

[[users]]
user_id = "ci-bot"
role = "writer"
public_key = "ed25519:base64..."
created_at = "2026-05-14"
```

The schema is versioned; unknown fields are rejected (fail-fast at startup).

**Fly.io deploy posture**:

- One Fly app: `midnight-manual` (single region at launch — `lhr` or `iad`).
- One managed Postgres cluster with pgvector enabled.
- Fly secrets (all required, validated at startup):
  - `DATABASE_URL` — Fly Postgres connection string
  - `MIDNIGHT_MANUAL_JWT_SECRET` — HS256 signing secret (32+ bytes random)
  - `MIDNIGHT_MANUAL_USER_STORE` — TOML body of the user store (loaded into a tmpfs path at boot via a Fly secret-to-file binding)
  - `MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_ID`, `MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET`, `MIDNIGHT_MANUAL_GITHUB_ORG`
- Image: Rust binary in a `gcr.io/distroless/cc` base; multi-stage build via `cargo-chef`.
- Continuous release: every merge to `main` triggers GitHub Actions → Fly deploy.
- Multi-region path: documented but out of scope for v1.

**Sweep job**:

A background tokio task runs every 5 minutes (configurable). For each source: list source_versions older than the source's `retention_count`-th most recent; for any such version older than `MIDNIGHT_MANUAL_SWEEP_GRACE` (default 24h) since being marked inactive, delete its chunks, documents, nodes, and packages in a single transaction. Aborted ingest_runs older than `MIDNIGHT_MANUAL_ABORT_GRACE` (default 1h) are also swept.

**Migrations**:

`sqlx migrate` invoked at startup unless `MIDNIGHT_MANUAL_AUTO_MIGRATE=false` (D22). Migrations are forward-only, idempotent, and shipped in `migrations/` as numbered SQL files.

**As** a Midnight Content Maintainer,
**I need** a CLI command that ingests a tree of source files (or a remote git repo) — chunking by AST-aware boundaries where possible, detecting package membership per language, and re-using all the upload, auth, and lifecycle plumbing from Markdown ingest —
**So that** the corpus carries Rust, TypeScript/JavaScript, and Compact code with the same hierarchical and provenance guarantees as Markdown content, and a partner-curated repo of examples can be re-ingested by CI on every upstream commit.

**Draft Acceptance Scenarios** (graduated manually):

1. **Given** a source `compact-examples` registered as `kind=code_repo` and a local directory containing Rust, TypeScript, and Compact files, **When** I run `mnm ingest code compact-examples ./path`, **Then** every recognized source file is chunked (tree-sitter for known languages, line-window fallback otherwise), package membership is assigned per language, and a new source_version is built and promoted.
2. **Given** a Rust file at `pkg/src/lib.rs` with a `Cargo.toml` at `pkg/Cargo.toml` declaring `name = "midnight-foo"`, **When** ingested, **Then** chunks emitted from that file carry `package = {kind: "rust", name: "midnight-foo", manifest_path: "pkg/Cargo.toml"}`.
3. **Given** a TypeScript file at `pkgs/web/src/index.ts` with a `package.json` at `pkgs/web/package.json` declaring `"name": "@midnight-ntwrk/web"`, **When** ingested, **Then** chunks carry `package = {kind: "npm", name: "@midnight-ntwrk/web", manifest_path: "pkgs/web/package.json"}`.
4. **Given** a Compact file `contracts/src/token/FungibleToken.compact` with `module FungibleToken { ... }` at top level, **When** ingested, **Then** every chunk inside the module's byte range carries `package = {kind: "compact", name: "FungibleToken", manifest_path: null}`; content outside any module declaration carries `package = null`.
5. **Given** a Compact file with two top-level modules (`module Foo { ... } module Bar { ... }`), **When** ingested, **Then** chunks in each module are tagged with their enclosing module name; multiple `package` rows exist for the same file (one per module).
6. **Given** a Cargo workspace with three member crates, **When** ingested, **Then** the workspace root's virtual `Cargo.toml` (no `[package]`) is ignored for package detection and each .rs file resolves to its member's `Cargo.toml`; the resulting source_version contains exactly three Rust packages.
7. **Given** a repo with `node_modules/`, `target/`, `vendor/`, `dist/`, and `.git/` directories, **When** ingested, **Then** these directories are skipped by default; configurable via `--include <glob>` / `--exclude <glob>`.
8. **Given** the repo's `.gitignore` matches certain files, **When** ingested, **Then** matched files are skipped by default; `--no-respect-gitignore` disables.
9. **Given** the `--git <url>` flag (with optional `--ref <branch|tag|sha>`), **When** ingest runs, **Then** the CLI clones the repo into a temp directory, ingests it, and removes the temp directory on exit (success or failure).
10. **Given** a file whose language has no tree-sitter grammar loaded (e.g. `.sol`, `.go` without explicit support), **When** ingested, **Then** the file falls back to a line-window chunker (default 60 lines, 20-line overlap, both configurable) and is indexed with `language = <ext>`, `symbol_path = []`.
11. **Given** a tree-sitter parser encounters a syntax error in an otherwise-supported file, **When** processing that file, **Then** the chunker falls back to a line-window for that file, emits a warning naming the file and parser error, and continues with subsequent files.
12. **Given** a binary file (detected by magic-number sniff) appears under the ingest path, **When** ingest runs, **Then** the file is skipped with a warning and counted in `summary.skipped_files`.
13. **Given** the `--dry-run`, `--json`, `--strict`, and `--force-new` flags, **When** ingest runs, **Then** they behave identically to `mnm ingest md` (re-used FRs from Story 2 — FR-018, FR-019, FR-020, FR-021).

**CLI surface introduced by this story**:

```
mnm ingest code <slug> <path>
    [--git <url>] [--ref <branch|tag|sha>]
    [--language <ext>=<grammar>]                # add or override language mapping
    [--include <glob>] [--exclude <glob>]
    [--no-respect-gitignore]
    [--include-submodules]
    [--code-chunk-lines <n>] [--code-chunk-overlap <n>]
    [--max-file-size <bytes>]
    [--strict] [--dry-run] [--force-new]
    [--embedding-model <name>] [--batch-size <n>]
```

(Source registry and version commands re-used from Story 2.)

**Default exclusions** (composable with `--include` / `--exclude`):

- `node_modules/`, `target/`, `vendor/`, `dist/`, `build/`, `out/`, `coverage/`, `.git/`
- Lockfiles by default: `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`, `Cargo.lock` (configurable)
- Common generated patterns: `*.min.js`, `*.bundle.js`, `*.generated.ts`, `*_pb.ts`, `*_pb.rs`

**Language → tree-sitter grammar mapping**:

| Extensions | Grammar | Symbol path source |
|---|---|---|
| `.rs` | tree-sitter-rust | mod / impl / struct / enum / fn |
| `.ts`, `.tsx` | tree-sitter-typescript | namespace / class / interface / function / method |
| `.js`, `.jsx`, `.mjs`, `.cjs` | tree-sitter-javascript | class / function / method |
| `.compact` | hand-rolled top-level scanner (until grammar exists); chunks by `module Foo { ... }` for package detection + line-window inside modules | module names |
| _other_ | line-window fallback | `[]` |

**Package detection rules**:

- **Rust**: walk up from each `.rs` file to the nearest `Cargo.toml` with a `[package]` section; skip workspace virtual roots (those with only `[workspace]`).
- **TypeScript / JavaScript**: walk up to the nearest `package.json` with a `"name"` field; if missing `"name"`, fall back to the manifest's directory name and warn.
- **Compact**: parse the file's top-level `module <Name> { ... }` blocks; tag chunks by enclosing module's byte range. No filesystem manifest. Files with no module declaration: `package = null`.
- **Other**: no package; chunks carry `package = null`.

**Cloud write protocol**: identical to Story 2 (`POST /v1/sources/{slug}/ingest-runs`, `PUT .../documents`, `POST .../finalize`, `POST .../abort`).

**As** a developer (or AI agent) using an MCP-capable assistant on a local machine,
**I need** an MCP server that exposes a small, stable set of retrieval tools — embedding queries locally, calling the cloud read API, reranking the top-K with a cross-encoder, and handling model-state errors with actionable remediation,
**So that** the agent gets fast, accurate, version-correct retrieval from the Midnight corpus without ever leaking query content off the machine and without the developer dropping back to a terminal when a model needs refreshing.

**Draft Acceptance Scenarios** (graduated manually):

1. **Given** the MCP server is launched by an AI client, **When** the MCP handshake completes, **Then** the server returns within 500ms cold start (Constitution IV), declares its tools and resources, and does not block on model loading.
2. **Given** the first `search` tool call arrives after handshake, **When** the server processes it, **Then** the embedding and reranker models are loaded lazily (once, behind a one-shot guard so concurrent calls don't double-load), and subsequent calls reuse the in-memory models.
3. **Given** a `search` call with a single `query: string`, **When** processed, **Then** the server embeds the query locally with bge-base-en-v1.5, POSTs `{queries: [{text, vector}], client_embedding_model, filters, limit}` to the cloud `/v1/search` endpoint, reranks the top-K returned candidates with bge-reranker-base, and returns the top `limit` results to the agent with the documented MCP result schema.
4. **Given** a `search` call with `queries: string[]` (multi-query for HyDE/expansion, per D3), **When** processed, **Then** each query is embedded locally and all pairs sent to `/v1/search` (which RRF-merges across queries); the merged candidate set is reranked locally and the top `limit` returned.
5. **Given** a `search` call with `rerank: false`, **When** processed, **Then** the server skips local reranking and returns the cloud's RRF-ordered top `limit`. (Useful for ultra-low-latency callers; default is `true`.)
6. **Given** the cloud responds with HTTP 409 `embedding_model_mismatch`, **When** any retrieval tool receives it, **Then** the tool returns a typed MCP error referencing `pull_models` with the corpus model name in the remediation message; the agent can call `pull_models` to self-heal.
7. **Given** the local embedding or reranker model is missing on first retrieval, **When** the tool runs, **Then** the server returns a typed `models_missing` error with the precise tool name to invoke (`pull_models`) and which model is needed.
8. **Given** the `pull_models` tool is called, **When** it runs, **Then** it downloads the embedding and reranker models to `$XDG_DATA_HOME/midnight-manual/models/`, emits progress notifications during download, and returns `{embedding_model, reranker_model, total_bytes, took_ms}` on success.
9. **Given** the `status` tool is called, **When** it runs, **Then** it returns `{server_version, cloud_reachable, corpus_embedding_model, local_embedding_model, local_reranker_model, model_state: ready|missing|stale|loading|corrupt, rate_limit_tier}` without requiring models to be loaded.
10. **Given** `get_chunk`, `get_chunk_siblings`, `get_chunk_parents`, or `list_sources` is called, **When** processed, **Then** the server proxies to the corresponding cloud endpoint and returns the raw JSON result; no embedding or reranking is involved.
11. **Given** the cloud is unreachable (network error / 503), **When** any retrieval tool is called, **Then** the tool returns a typed `service_unavailable` error including any `Retry-After` from the cloud response; the MCP server never crashes the AI client (Constitution V).
12. **Given** the config supplies a bearer token (per D17/D18) and the `MIDNIGHT_MANUAL_DISABLE_TELEMETRY` flag is unset, **When** any tool runs, **Then** the bearer is included in cloud requests via `Authorization: Bearer <token>` and an anonymized telemetry event (`tool_name`, `latency_ms`, `result_count`, `model_state`, `rerank_on`) is emitted — never query content, never the token, never chunk content (Constitution VII).
13. **Given** two concurrent `search` calls arrive while models are loading, **When** they are processed, **Then** both await the single in-flight load (no double-load, no double-download) and complete in order once models are ready.
14. **Given** the AI client kills the subprocess mid-request, **When** the SIGTERM lands, **Then** the server cancels in-flight cloud calls cleanly, flushes any pending telemetry, and exits within 1 second.

**MCP tool surface introduced by this story**:

```
search           — primary retrieval tool (text → reranked chunks)
get_chunk        — fetch one chunk by id with full metadata
get_chunk_siblings — fetch all chunks of the chunk's document, ordered
get_chunk_parents — walk parent chain to source root
list_sources     — enumerate available sources for filter-narrowing
pull_models      — download/update local embedding and reranker models
status           — health and model-state introspection
```

**`search` tool input schema**:

```json
{
  "type": "object",
  "properties": {
    "query":   { "type": "string", "description": "Single query string (convenience for casual callers)." },
    "queries": { "type": "array", "items": {"type": "string"}, "description": "Multi-query input for HyDE / expansion; sophisticated callers may pass several reformulations of the user's intent." },
    "limit":   { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
    "rerank":  { "type": "boolean", "default": true },
    "filters": { "type": "object", "description": "Same shape as the cloud /v1/search filters: source_slug, attribution, verified, content_type, language_target, sdk_dependency, package." }
  },
  "oneOf": [ {"required": ["query"]}, {"required": ["queries"]} ]
}
```

**`search` tool result shape**: array of result objects with the same shape as the cloud `/v1/search` response items (chunk, document, source, source_version, package, parent_chain, navigation, scores) — plus a top-level `rerank` field carrying the reranker score when rerank=true.

**Lazy model loading**:

The MCP handshake completes immediately by declaring tools and resources from a static manifest. The actual ONNX model load (≈600–700 MB combined RSS for embedder + reranker) is deferred to first retrieval call, behind a single `tokio::sync::OnceCell` guard so concurrent first-callers share one load. Cold start (process launch → handshake done) stays under the 500ms Constitution IV budget; first retrieval pays an additional ~1.5s amortized over a single tool call.

**Configuration** (per D17/D18; loaded once at startup):

```toml
# $XDG_CONFIG_HOME/midnight-manual/config.toml
[server]
url = "https://manual.midnight.network"

[auth]
# Optional. If absent, requests run as anonymous.
bearer_token_env = "MIDNIGHT_MANUAL_TOKEN"

[models]
embedding   = "bge-base-en-v1.5"
reranker    = "bge-reranker-base"
cache_dir   = "~/.local/share/midnight-manual/models"

[telemetry]
enabled = true
```

`MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1` overrides the file. `--config <path>` overrides discovery. Bearer token is resolved via the env var named in `bearer_token_env`, never from the file directly.

**As** a developer using the local MCP server (and any ecosystem reader),
**I need** an HTTP API on the cloud server that returns relevant chunks for a given query with full hierarchical and provenance metadata, runs hybrid FTS + vector retrieval with RRF merging, and detects model-mismatch cleanly,
**So that** the MCP server (Story 5) has a single, fast, well-typed contract to call — and so partner projects can build directly on the same surface.

**Draft Acceptance Scenarios** (graduated manually):

1. **Given** a single search query, **When** `POST /v1/search` is called with `{text, vector}`, **Then** the API runs FTS (Postgres tsvector / ts_rank_cd) and pgvector ANN in parallel, merges via RRF (k=60), and returns up to `limit` chunks (default 20, max 100) with full chunk + document + source + parent_chain + navigation + scores.
2. **Given** a multi-query request with N `{text, vector}` pairs, **When** `POST /v1/search` is called, **Then** retrieval runs hybrid per pair and RRF merges across both retrieval modes and across pairs; the response's `scores.matched_queries` lists which pairs contributed to each result.
3. **Given** a chunk id, **When** `GET /v1/chunks/{id}` is called, **Then** the API returns the chunk with full metadata, document, source, parent_chain, navigation, and the corpus embedding model identifier.
4. **Given** a chunk id, **When** `GET /v1/chunks/{id}/siblings` is called, **Then** the API returns every chunk from the same document ordered by `chunk_index`, suitable for full-page reconstruction.
5. **Given** a chunk id, **When** `GET /v1/chunks/{id}/parents` is called, **Then** the API returns the parent_chain from the chunk's node up to the source-version root.
6. **Given** a request with `client_embedding_model` that does not match the active corpus model, **When** any search-or-chunk endpoint is hit, **Then** the API responds with HTTP 409 and a typed body `{error: {code: "embedding_model_mismatch", message, remediation, context: {corpus_model, client_model}}}`.
7. **Given** any read endpoint, **When** any version of any source is not active, **Then** chunks/documents from that version are excluded by default; querying historical versions requires an explicit `?source_version_revision=N` parameter (admins-only on a future iteration; v1 allows public access to historical reads).
8. **Given** an anonymous request hits the per-IP rate limit, **When** the next request arrives, **Then** the API responds with HTTP 429, a `Retry-After` header, and a typed error body naming the limit and reset time.
9. **Given** a request with a valid GitHub-SSO bearer token, **When** the request arrives, **Then** the rate limit applies to the per-user (higher) tier from D11; standard `X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset` headers are returned on every response.
10. **Given** a request matching an active CIDR override entry, **When** the request arrives, **Then** the effective rate limit is the override's `limit_rps` until `expires_at`; matching is checked before the anonymous/SSO tier (D11).
11. **Given** filters in the search request body — `{attribution, verified, content_type, source_slug, language_target, sdk_dependency, package}` — **When** `POST /v1/search` runs, **Then** results are restricted to chunks whose document.provenance / source / package fields satisfy every filter (logical AND across keys, OR within a key's value array).
12. **Given** `GET /v1/models/active`, **When** called, **Then** the API returns the active embedding model identifier (`{name, revision, dim, provider}`) — used by clients to detect they need to pull a different model before issuing queries.
13. **Given** any request, **When** the response is sent, **Then** the response carries a stable `X-Request-Id` header for log correlation, the API version prefix is `/v1`, and the body is JSON content-type with a documented schema.
14. **Given** an empty queries array or vector dim mismatch (with `client_embedding_model` agreeing), **When** `POST /v1/search` is called, **Then** the API responds with HTTP 400 and a typed `invalid_request` error naming the offending field.
15. **Given** the Postgres backend is temporarily unavailable, **When** any read endpoint is called, **Then** the API responds with HTTP 503, a `Retry-After` header, and a typed `service_unavailable` body (Constitution VI graceful degradation); the server never crashes the request.

**Endpoint surface introduced by this story**:

```
POST /v1/search                              # hybrid search
GET  /v1/sources                             # list sources
GET  /v1/sources/{slug}                      # source detail
GET  /v1/sources/{slug}/versions             # list source_versions
GET  /v1/sources/{slug}/versions/{revision}  # version detail (active or historical)
GET  /v1/chunks/{id}                         # chunk detail
GET  /v1/chunks/{id}/siblings                # all chunks in same document
GET  /v1/chunks/{id}/parents                 # parent chain to source root
GET  /v1/documents/{id}                      # document detail
GET  /v1/documents/{id}/chunks               # all chunks of a document, ordered
GET  /v1/nodes/{id}                          # node detail
GET  /v1/nodes/{id}/children                 # node's direct children
GET  /v1/models/active                       # current corpus embedding model
GET  /healthz                                # liveness
GET  /readyz                                 # readiness (db reachable, model registry loaded)
```

**Search request shape**:

```json
{
  "queries": [
    { "text": "how do I compile a Compact contract", "vector": [0.123, ...] }
  ],
  "client_embedding_model": "bge-base-en-v1.5@1",
  "limit": 20,
  "filters": {
    "source_slug": ["midnight-docs"],
    "attribution": ["foundation", "partner"],
    "verified": true,
    "content_type": ["doc", "tutorial", "example"],
    "language_target": { "name": "compact", "version_constraint_satisfies": "0.23" },
    "sdk_dependency": { "kind": "npm", "name": "@midnight-ntwrk/midnight-js", "version_constraint_satisfies": "1.4.0" },
    "package": { "kind": "rust", "name": "midnight-foo" }
  },
  "include_scores": true
}
```

**Search response shape** (per result):

```json
{
  "chunk": { "id", "content", "chunk_index", "total_chunks", "heading_path", "symbol_path", "start_byte", "end_byte", "token_count", "status" },
  "document": { "id", "kind", "source_url", "published_url", "source_path", "language", "provenance" },
  "source": { "slug", "display_name", "kind" },
  "source_version": { "revision", "ingested_at" },
  "package": { "kind", "name", "version" } | null,
  "parent_chain": [ { "id", "kind", "name", "order_index" }, ... ],
  "navigation": { "prev_chunk_id" | null, "next_chunk_id" | null },
  "scores": { "rrf": 0.0312, "fts_rank": 3, "vector_distance": 0.247, "matched_queries": [0, 1] }
}
```

**Error envelope** (all 4xx and 5xx):

```json
{
  "error": {
    "code": "embedding_model_mismatch",
    "message": "Corpus is encoded with bge-base-en-v1.5@1 but client sent bge-small-en-v1.5@1.",
    "remediation": "Pull the matching model with: mnm models pull bge-base-en-v1.5",
    "context": { "corpus_model": "bge-base-en-v1.5@1", "client_model": "bge-small-en-v1.5@1" }
  },
  "request_id": "01HQ..."
}
```

Documented error codes: `embedding_model_mismatch` (409), `invalid_request` (400), `not_found` (404), `unauthorized` (401), `forbidden` (403), `rate_limited` (429), `service_unavailable` (503), `internal` (500).

**As** a Midnight Content Maintainer,
**I need** a CLI command that ingests a tree of Markdown files into the cloud corpus — chunking, embedding locally, attaching provenance metadata, and atomically promoting a new source_version,
**So that** I can keep the Midnight docs corpus accurate, current, and trustworthy with a single repeatable command, both interactively and from CI.

**Draft Acceptance Scenarios** (full graduation handled manually due to multi-clause format):

1. **Given** a source `midnight-docs` exists and a local directory of `.md`/`.mdx` files, **When** I run `mnm ingest md midnight-docs ./path`, **Then** a new source_version is created in the cloud, chunks are emitted for every Markdown file under `./path`, the new version is promoted to active, and the prior active version becomes inactive.
2. **Given** a Markdown file with frontmatter `verified: true, verified_by: "midnight-foundation"`, **When** ingested, **Then** the resulting `document.provenance` carries those fields and `document.frontmatter` holds the full frontmatter verbatim.
3. **Given** I pass `--manifest hierarchy.yaml`, **When** ingest runs, **Then** the `node` tree reflects the manifest; with no `--manifest`, the `node` tree reflects directory ancestry up to the ingest root.
4. **Given** a Markdown file whose `content_hash` matches its document in the prior active source_version, **When** the new version is built, **Then** new chunk rows are inserted carrying the previous version's embedding bytes (no re-embed) and the skip count is reported in the run summary.
5. **Given** `--dry-run`, **When** ingest runs, **Then** the CLI emits the full plan (chunks, expected re-embeds, target revision) without contacting any cloud write endpoint.
6. **Given** ingest is interrupted mid-upload, **When** I rerun the same command, **Then** the CLI resumes the in-progress source_version uploading only missing chunks and promotes only after a full successful upload.
7. **Given** `--json`, **When** ingest runs, **Then** stdout is NDJSON, no human-formatted text is written to stdout, and the last record is `{"type":"summary","result":"ok|partial|error",...}`.
8. **Given** the local embedding model is missing or mismatched against the source's active model, **When** ingest starts, **Then** the CLI surfaces an actionable error referencing `mnm models pull` and exits non-zero before contacting the cloud server.
9. **Given** `--source-url-prefix` and `--published-url-prefix` flags (or equivalents in the manifest), **When** ingest emits documents, **Then** `document.source_url` and `document.published_url` are constructed by appending the file's relative path; absolute URLs in the manifest override prefix construction.
10. **Given** a file present in the prior active version but absent from this ingest path, **When** the new version is built, **Then** the document is not carried into the new version; historical queries still see it.
11. **Given** malformed frontmatter, **When** ingest processes that file, **Then** the CLI emits a warning naming the file and YAML location, and either continues with `frontmatter = null` (default) or skips the file based on `--on-frontmatter-error {continue,skip}`.
12. **Given** a file larger than `--max-file-size` (default 10 MB), **When** ingest processes it, **Then** the file is skipped with a warning and listed in the summary's `skipped_files`; the run does not fail unless `--strict` is set.

**CLI surface introduced by this story**:

```
mnm sources add <slug> --kind <docs_site|code_repo|standalone|mixed> [--origin-url <url>] [--retention-count <n>]
mnm sources list
mnm sources show <slug>
mnm ingest md <slug> <path>
    [--manifest <path>]
    [--source-url-prefix <url>] [--published-url-prefix <url>]
    [--max-file-size <bytes>]
    [--on-frontmatter-error continue|skip]
    [--strict] [--strict-manifest]
    [--dry-run]
    [--force-new]
    [--embedding-model <name>]
    [--batch-size <n>]
mnm versions list <slug>
mnm versions show <slug> <revision>
```

(Promote / rollback / retire live in Story 8.)

**Manifest schema** (`hierarchy.yaml`):

```yaml
manifest_version: 1
root:
  name: docs
  children:
    - name: getting-started
      path: getting-started/                         # optional directory pin
      published_url: https://docs.midnight.network/getting-started/
      children:
        - file: getting-started/quickstart.mdx
          name: Quickstart
          published_url: https://docs.midnight.network/getting-started/quickstart
          provenance:
            attribution: foundation
            content_type: tutorial
```

Frontmatter merges on top of node-level `provenance:`. Files absent from the manifest fall back to directory-tree inference unless `--strict-manifest` is set (then unreferenced files are an error).

**Implicit cloud write protocol** (refined in Story 9):

- `POST /sources/{slug}/ingest-runs` → returns `ingest_run_id` + `source_version_id`, allocates a new `source_version` in `building` state
- `PUT /sources/{slug}/ingest-runs/{id}/documents` → batch upload of `{document, chunks}` pairs
- `POST /sources/{slug}/ingest-runs/{id}/finalize` → flips `is_active` atomically; demotes prior active
- `POST /sources/{slug}/ingest-runs/{id}/abort` → marks the in-progress version as abandoned, eligible for sweep

**As** a Midnight Network maintainer and a DApp developer (read consumer),
**I need** a stable, expressive content model that captures sources, versions, documents, chunks, hierarchy, packages, and provenance,
**So that** every downstream story has unambiguous shapes to build against, and every chunk returned to a caller carries enough metadata to walk its parent chain, cite a public URL, and be ranked by trustworthiness and freshness.

**Draft Acceptance Scenarios**:

1. **Given** an active source_version is being replaced by a new ingest, **When** a read query arrives mid-ingest, **Then** the query returns chunks from the previous active version only — never a mix.
2. **Given** a chunk is returned by the read API, **When** the caller inspects its metadata, **Then** the chunk carries a parent_chain array from immediate parent up to the source root, plus chunk_index, total_chunks, prev_chunk_id and next_chunk_id (each nullable at boundaries), plus the document's source_url and published_url.
3. **Given** a code file located at `pkg/src/lib.rs` with a `Cargo.toml` at `pkg/Cargo.toml` declaring `name = "midnight-foo"`, **When** the file is ingested, **Then** every chunk emitted from that file carries `package.name = "midnight-foo"`, `package.kind = "rust"`, and `package.manifest_path = "pkg/Cargo.toml"`.
4. **Given** a Compact file containing `module FungibleToken { ... }`, **When** ingested, **Then** every chunk emitted from that file carries `package.name = "FungibleToken"` and `package.kind = "compact"`; for files declaring multiple top-level modules the chunks are tagged with whichever module lexically contains them.
5. **Given** an ingest run provides `--manifest hierarchy.yaml`, **When** chunks are emitted, **Then** their parent_chain reflects the manifest tree and ignores on-disk directory structure; and **Given** no manifest is provided, **When** chunks are emitted, **Then** parent_chain reflects directory ancestry up to the ingest root.
6. **Given** the corpus is encoded with `bge-base-en-v1.5@1`, **When** a read query arrives carrying `client_embedding_model = "bge-small-en-v1.5@1"`, **Then** the API responds with HTTP 409 and a typed JSON body naming the corpus model, the client model, and the remediation tool to invoke.
7. **Given** a source has retention_count = 5 with 5 historical source_versions plus one active, **When** a new ingest promotes a sixth version to active, **Then** the previously-active version becomes inactive, the oldest inactive version becomes eligible for sweep, and the database enforces "at most one active source_version per source" as a partial unique constraint.
8. **Given** a Markdown page with frontmatter `verified: true`, `verified_by: "midnight-foundation"`, `verified_at: "2026-05-01"`, `language_targets: [{name: compact, version_constraint: ">=0.23"}]`, **When** ingested, **Then** these fields are stored once on the document row and inherited at read time by every chunk returned from that document.
9. **Given** a chunk fails to embed (model error, OOM, malformed content), **When** the ingest run completes, **Then** the chunk row exists with `status = "embed_failed"` and `embedding IS NULL`, the read API excludes it from results, and an admin query can still list it.
10. **Given** a Markdown page with no headings, **When** ingested, **Then** chunking falls back to a fixed-window strategy (default 800 tokens, 100-token overlap, both configurable) and the document is indexed without error.
11. **Given** a document is unchanged across two consecutive ingest runs (content_hash matches), **When** the new source_version is built, **Then** the schema permits the ingest CLI to insert a fresh chunk row carrying the previous version's embedding bytes (no re-embed required); ingest logic is owned by Story 2/3.
12. **Given** a chunk is returned to a caller, **When** the caller fetches `/chunks?document_id=...`, **Then** the API returns every chunk from that document in chunk_index order — enabling a caller to reconstruct the full page from any starting chunk.

**Entity model** (summary; full DDL deferred to Story 2 planning):

- `source` — logical source. (id, slug UNIQUE, display_name, kind enum {docs_site, code_repo, standalone, mixed}, origin_url, retention_count INT NOT NULL DEFAULT 5, created_at, retired_at NULL)
- `source_version` — immutable snapshot. (id, source_id FK, revision INT, is_active BOOL, ingested_at, ingest_cli_version, embedding_model_id FK, content_hash, notes, retired_at NULL). Partial unique index: `(source_id) WHERE is_active`.
- `embedding_model` — registry. (id, name, revision INT, dim INT, provider, created_at). Initial row: `(bge-base-en-v1.5, 1, 768, baai)`.
- `node` — hierarchy tree. (id, source_version_id FK, parent_node_id FK NULL, kind enum {root, group, document, chunk}, name, order_index INT, created_at). One root node per source_version.
- `document` — page or file. (id, source_version_id FK, node_id FK, kind enum {markdown, code, plaintext}, source_url NULL, published_url NULL, source_path, language NULL, content_hash, source_modified_at NULL, frontmatter JSONB NULL, provenance JSONB, package_id FK NULL, char_count, token_count, created_at).
- `chunk` — indexed unit. (id, source_version_id FK, document_id FK, node_id FK, chunk_index INT, total_chunks INT, content TEXT, content_hash, tsvector GENERATED STORED, embedding VECTOR(768) NULL, embedding_model_id FK, heading_path TEXT[], symbol_path TEXT[], start_byte INT, end_byte INT, token_count INT, status enum {ready, embed_failed, deprecated}, created_at).
- `package` — code grouping. (id, source_version_id FK, kind enum {rust, npm, compact, other}, name, version NULL, manifest_path NULL, metadata JSONB).

**Provenance JSONB schema** (on `document`):
- `attribution`: enum {foundation, partner, third_party, community, unknown}
- `verified`: bool
- `verified_by`: text NULL
- `verified_at`: date NULL
- `verification_notes`: text NULL
- `language_targets`: array of `{ name, version_constraint }` (e.g. `{name: "compact", version_constraint: ">=0.23"}`)
- `sdk_dependencies`: array of `{ kind, name, version_constraint }` (kind ∈ npm, cargo, compact)
- `deprecation`: `{ is_deprecated, since: date NULL, reason: text NULL }`
- `tags`: array of text
- `content_type`: enum {doc, tutorial, reference, example, contract_source, sdk_source, test, readme}

**Indexes**:
- `chunk.embedding` — HNSW (pgvector)
- `chunk.tsvector` — GIN
- `chunk(source_version_id, status)`
- `source_version(source_id) WHERE is_active` — partial unique
- `document(content_hash)` — reuse detection at ingest
- `node(parent_node_id, order_index)` — sibling/parent walks

**Visibility (read API exposure)**:
- Public: every field on `source`, `source_version`, `embedding_model`, `node`, `document`, `chunk`, `package`.
- Excluded from the read API: tables that exist for write-side concerns (`user`, `api_key`, `rate_limit_override`) — these live in the schema for Story 8/9 but are admin-only.

---

## Watching List

*Items that might affect graduated stories:*

- Whether the chunk graph design (sibling links, parent chain) requires a separate `chunk_graph` table or can be normalized into the primary chunk row + a `node` table. Affects S1 schema and S2/S3 ingest semantics.
- Whether to ship a per-document version model (snapshot of full corpus state at time T) or a per-chunk version model. Affects S1 and S6 (confidence ranking sometimes depends on "is this the latest?").
- Whether the local MCP server caches results / embeddings between invocations. Affects S5 perf budget and S11 telemetry.

---

## Glossary

- **MCP**: Model Context Protocol — the standard the local server implements to expose tools to AI assistants.
- **Chunk**: Smallest indexed unit. A range of text/code with its own embedding(s), hierarchical metadata, and provenance.
- **Page / File**: A single source document (Markdown page, source file). Contains one or more ordered chunks.
- **Parent**: The container one level up — e.g. a guide that contains pages, a directory that contains files, a package that contains modules. Parent chain extends to root.
- **Source**: A logical content source — a docs repo, a code repo, a single uploaded file. Has its own root in the parent chain.
- **Provenance metadata**: Attribution, verification status, language/SDK version targets, freshness — fields that feed the confidence score.
- **Confidence score**: A derived ranking signal computed from provenance metadata and retrieval scores, surfaced to callers so agents can weight results.
- **Local MCP server**: The user-facing component that runs on the developer's machine and speaks MCP to an AI client.
- **Cloud server**: The hosted HTTP API in front of the DB. Handles authenticated writes from admins and unauthenticated (or lightly-authenticated) reads from local MCP servers.
- **CLI (`mn-manual` — working name)**: The admin tool used to ingest, update, delete, and audit corpus data. Used both interactively and in CI.

---

## Next Actions

**All 11 stories graduated.** Spec is complete pending user review.

1. **Suggested final pass**: run `validate-spec.py` one more time (passing), then walk the SPEC end-to-end to look for cross-story inconsistencies that the validator can't catch (e.g. terminology drift, conflicting defaults, decisions superseded but still referenced).
2. **Watching list (preserved for implementation)**: Compact tree-sitter grammar status; MCP cold-start measurements against real hardware; scoring policy default weights after real-traffic measurement; rate-limit numeric defaults; multi-query cap ergonomics; mcp install registry growth; MSRV bump cadence; retention window real-world fit.

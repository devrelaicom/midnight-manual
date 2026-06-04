# Feature Specification: rag-platform

**Feature Branch**: `feature/rag-platform`
**Created**: 2026-05-13
**Last Updated**: 2026-05-13
**Status**: In Progress
**Discovery**: See `discovery/` folder for full context

---

## Problem Statement

Developers building on the Midnight Network need accurate, current, and well-attributed knowledge about Midnight's rapidly-changing surface area (Compact language, SDKs, tooling, protocols) at the point of use — inside their AI-assisted coding workflow. The existing MCP server is unfit for purpose, so the project is being restarted as a greenfield Rust implementation. Without a high-quality retrieval surface, developers and the AI agents they work with fall back on stale training data, leading to confidently-wrong Compact code, broken SDK calls, and lost time. Midnight ships breaking changes often, so retrieval must surface not just relevant content but the *right version*, with provenance and verification metadata strong enough to support a confidence score.

## Personas

| Persona | Description | Primary Goals |
|---------|-------------|---------------|
| **DApp Developer (Dev)** | External developer (or AI agent) building on Midnight. Uses an MCP-capable AI assistant locally. May know nothing about platform internals. | Get accurate, version-correct answers and code examples inline while coding. Avoid being misled by stale or unverified content. |
| **Content Maintainer (Maintainer)** | Midnight Network staff. Authors and curates content; runs the CLI to ingest, update, and retire content. Has admin credentials. | Keep the corpus accurate and current. Tag content with attribution and verification status. Make changes scriptable so updates can run in CI. |
| **Operator** | Midnight Network ops/infra. Runs the cloud server; monitors health; manages secrets and rollouts. May overlap with Maintainer. | Reliable hosted service. Clear logs/metrics. Safe deploys. Bounded costs. |
| **Ecosystem Reader** | Partner projects, community contributors, third-party tools. Read-only consumers via the MCP server (or direct API). | Programmatic access to a trustworthy Midnight knowledge base without running their own ingestion. |

---

## User Scenarios & Testing

<!--
  Stories are ordered by priority (P1 first).
  Each story is independently testable and delivers standalone value.
  Stories may be revised if later discovery reveals gaps — see REVISIONS.md
-->

### User Story 1 — Content model & metadata schema (Priority: P1)

**Revision**: v1.0

**As** a Midnight Network maintainer and a DApp developer (read consumer),
**I need** a stable, expressive content model that captures sources, versions, documents, chunks, hierarchy, packages, and provenance,
**So that** every downstream story has unambiguous shapes to build against, and every chunk returned to a caller carries enough metadata to walk its parent chain, cite a public URL, and be ranked by trustworthiness and freshness.

**Acceptance Scenarios**:

1. **Given** an active source_version is being replaced by a new ingest, **When** a read query arrives mid-ingest, **Then** the query returns chunks from the previous active version only — never a mix.
2. **Given** a chunk is returned by the read API, **When** the caller inspects its metadata, **Then** the chunk carries a `parent_chain` array from immediate parent up to the source root, plus `chunk_index`, `total_chunks`, `prev_chunk_id`, `next_chunk_id` (each nullable at boundaries), plus the document's `source_url` and `published_url`.
3. **Given** a code file at `pkg/src/lib.rs` with a `Cargo.toml` at `pkg/Cargo.toml` declaring `name = "midnight-foo"`, **When** the file is ingested, **Then** every chunk emitted from that file carries `package.name = "midnight-foo"`, `package.kind = "rust"`, and `package.manifest_path = "pkg/Cargo.toml"`.
4. **Given** a Compact file containing `module FungibleToken { ... }`, **When** ingested, **Then** every chunk emitted carries `package.name = "FungibleToken"` and `package.kind = "compact"`; for files declaring multiple top-level modules, chunks are tagged with whichever module lexically contains them.
5. **Given** an ingest run provides `--manifest hierarchy.yaml`, **When** chunks are emitted, **Then** their `parent_chain` reflects the manifest tree and ignores on-disk directory structure; and when no manifest is provided, `parent_chain` reflects directory ancestry up to the ingest root.
6. **Given** the corpus is encoded with `bge-base-en-v1.5@1`, **When** a read query arrives carrying `client_embedding_model = "bge-small-en-v1.5@1"`, **Then** the API responds with HTTP 409 and a typed JSON body naming the corpus model, the client model, and the remediation tool to invoke.
7. **Given** a source has `retention_count = 5` with 5 historical source_versions plus one active, **When** a new ingest promotes a sixth version to active, **Then** the previously-active version becomes inactive, the oldest inactive version becomes eligible for sweep, and the database enforces "at most one active source_version per source" as a partial unique constraint.
8. **Given** a Markdown page with frontmatter `verified: true, verified_by: "midnight-foundation", verified_at: "2026-05-01", language_targets: [{name: compact, version_constraint: ">=0.23"}]`, **When** ingested, **Then** these fields are stored once on the document row and inherited at read time by every chunk returned from that document.
9. **Given** a chunk fails to embed (model error, OOM, malformed content), **When** the ingest run completes, **Then** the chunk row exists with `status = "embed_failed"` and `embedding IS NULL`, the read API excludes it from results, and an admin query can still list it.
10. **Given** a Markdown page with no headings, **When** ingested, **Then** chunking falls back to a fixed-window strategy (default 800 tokens, 100-token overlap, both configurable) and the document is indexed without error.
11. **Given** a document is unchanged across two consecutive ingest runs (`content_hash` matches), **When** the new source_version is built, **Then** the schema permits the ingest CLI to insert a fresh chunk row carrying the previous version's embedding bytes — no re-embed required. (Ingest logic lives in Story 2/3.)
12. **Given** a chunk is returned to a caller, **When** the caller fetches `/chunks?document_id=...`, **Then** the API returns every chunk from that document in `chunk_index` order, enabling reconstruction of the full page from any starting chunk.

<details>
<summary>Supporting Decisions</summary>

- **D1**: Embedding library — `fastembed-rs`
- **D4**: Hybrid retrieval — parallel FTS + pgvector, RRF in app code
- **D6**: Code chunking — tree-sitter with line-window fallback
- **D7**: Parent inference — filesystem default + optional manifest override
- **D8 / D15**: Per-source-version snapshots with retention = 5
- **D9**: Compact package detection — in-source `module Foo {` declarations
- **D12**: Model lifecycle — client-supplied model id on every request; server enforces match
- **D13**: Page-level `source_url` and `published_url` (not chunk-level)
- **D14**: Embedding model — `bge-base-en-v1.5` (768 dims)

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

### User Story 2 — Admin ingestion of Markdown content via CLI (Priority: P1)

**Revision**: v1.0

**As** a Midnight Content Maintainer,
**I need** a CLI command that ingests a tree of Markdown files into the cloud corpus — chunking, embedding locally, attaching provenance metadata, and atomically promoting a new source_version,
**So that** I can keep the Midnight docs corpus accurate, current, and trustworthy with a single repeatable command, both interactively and from CI.

**Acceptance Scenarios**:

1. **Given** a source `midnight-docs` exists and a local directory of `.md` / `.mdx` files, **When** I run `mnm ingest md midnight-docs ./path`, **Then** a new source_version is created in the cloud, chunks are emitted for every Markdown file under `./path`, the new version is promoted to active, and the prior active version becomes inactive.
2. **Given** a Markdown file with frontmatter `verified: true, verified_by: "midnight-foundation"`, **When** ingested, **Then** the resulting `document.provenance` carries those fields and `document.frontmatter` holds the full frontmatter verbatim.
3. **Given** I pass `--manifest hierarchy.yaml`, **When** ingest runs, **Then** the `node` tree reflects the manifest; with no `--manifest`, the `node` tree reflects directory ancestry up to the ingest root.
4. **Given** a Markdown file whose `content_hash` matches its document in the prior active source_version, **When** the new version is built, **Then** new chunk rows are inserted carrying the previous version's embedding bytes (no re-embed) and the skip count is reported in the run summary.
5. **Given** `--dry-run`, **When** ingest runs, **Then** the CLI emits the full plan (chunks, expected re-embeds, target revision) without contacting any cloud write endpoint.
6. **Given** ingest is interrupted mid-upload, **When** I rerun the same command, **Then** the CLI resumes the in-progress source_version, uploading only missing chunks, and promotes only after a full successful upload.
7. **Given** `--json`, **When** ingest runs, **Then** stdout is NDJSON, no human-formatted text is written to stdout, and the last record is `{"type":"summary","result":"ok|partial|error",...}`.
8. **Given** the local embedding model is missing or mismatched against the source's active model, **When** ingest starts, **Then** the CLI surfaces an actionable error referencing `mnm models pull` and exits non-zero before contacting the cloud server.
9. **Given** `--source-url-prefix` and `--published-url-prefix` flags (or equivalents in the manifest), **When** ingest emits documents, **Then** `document.source_url` and `document.published_url` are constructed by appending the file's relative path; absolute URLs in the manifest override prefix construction.
10. **Given** a file present in the prior active version but absent from this ingest path, **When** the new version is built, **Then** the document is not carried into the new version; historical queries against earlier active-at-the-time versions still see it.
11. **Given** malformed frontmatter, **When** ingest processes that file, **Then** the CLI emits a warning naming the file and YAML location, and either continues with `frontmatter = null` (default) or skips the file based on `--on-frontmatter-error {continue,skip}`.
12. **Given** a file larger than `--max-file-size` (default 10 MB), **When** ingest processes it, **Then** the file is skipped with a warning and listed in the summary's `skipped_files`; the run does not fail unless `--strict` is set.

**Story 2 CLI surface**:

> **Superseded (2026-05-28):** the PR #50 ingest-UX rework replaced the per-content-type
> `mnm ingest md` / `mnm ingest code` commands with a single manifest-driven
> `mnm ingest run`. Chunker selection is now **per file, by extension** (Markdown →
> heading chunker, code → language chunker, unknown → line-window), so one `ingest run`
> over a mixed tree routes every file to its best chunker. The `mnm ingest md`/`code`
> invocations and flag lists below are retained for historical trace only; the live flag
> set is on `mnm ingest run` (see `crates/mn-cli/src/commands/ingest/run.rs`).

```
mnm sources add <slug> --kind <docs_site|code_repo|standalone|mixed> [--display-name <name>] [--origin-url <url>] [--retention-count <n>]
                                                                              # display_name defaults to slug if omitted
mnm sources list
mnm sources show <slug>
mnm ingest md <slug> <path>
    [--manifest <path>] [--strict-manifest]
    [--source-url-prefix <url>] [--published-url-prefix <url>]
    [--max-file-size <bytes>]
    [--on-frontmatter-error continue|skip]
    [--strict] [--dry-run] [--force-new]
    [--embedding-model <name>] [--batch-size <n>]
mnm versions list <slug>
mnm versions show <slug> <revision>
```

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

Frontmatter `provenance` merges on top of node-level `provenance:`. Files absent from the manifest fall back to directory-tree inference unless `--strict-manifest` is set (in which case unreferenced files raise an error before any upload).

**Implicit cloud write protocol** (concrete endpoints land in Story 9):

- `POST /v1/sources/{slug}/ingest-runs` → returns `ingest_run_id` + `source_version_id`, allocates a `source_version` row in `building` state
- `PUT /v1/sources/{slug}/ingest-runs/{id}/documents` → batch upload of `{document, chunks}` pairs (idempotent on document.content_hash + chunk.content_hash)
- `POST /v1/sources/{slug}/ingest-runs/{id}/finalize` → atomically flips `is_active` to the new version, demotes the prior active
- `POST /v1/sources/{slug}/ingest-runs/{id}/abort` → marks the in-progress version as abandoned, eligible for sweep

<details>
<summary>Supporting Decisions</summary>

- **D1**: fastembed-rs (embedding library)
- **D6**: tree-sitter chunking (Markdown via heading parser; tree-sitter applies to code in Story 3)
- **D7**: filesystem default + manifest override
- **D8 / D15**: source_version snapshots, retention 5
- **D13**: page-level `source_url` and `published_url`
- **D14**: bge-base-en-v1.5 default model
- **D16 / D17 / D18 / D19**: CLI shape (binary name, global flags, config discovery, noun-first grouping)

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

### User Story 4 — Cloud read API: hybrid (FTS + vector) search (Priority: P1)

**Revision**: v1.0

**As** a developer using the local MCP server (and any ecosystem reader),
**I need** an HTTP API on the cloud server that returns relevant chunks for a given query with full hierarchical and provenance metadata, runs hybrid FTS + vector retrieval with RRF merging, and detects model-mismatch cleanly,
**So that** the MCP server (Story 5) has a single, fast, well-typed contract to call — and so partner projects can build directly on the same surface.

**Acceptance Scenarios**:

1. **Given** a single search query, **When** `POST /v1/search` is called with `{text, vector}`, **Then** the API runs FTS (Postgres tsvector / `ts_rank_cd`) and pgvector ANN in parallel, merges via RRF (k=60), and returns up to `limit` chunks (default 20, max 100) with full chunk + document + source + parent_chain + navigation + scores.
2. **Given** a multi-query request with N `{text, vector}` pairs, **When** `POST /v1/search` is called, **Then** retrieval runs hybrid per pair and RRF merges across both retrieval modes and across pairs; the response's `scores.matched_queries` lists which pairs contributed to each result.
3. **Given** a chunk id, **When** `GET /v1/chunks/{id}` is called, **Then** the API returns the chunk with full metadata, document, source, `parent_chain`, navigation, and the corpus embedding model identifier.
4. **Given** a chunk id, **When** `GET /v1/chunks/{id}/next` or `GET /v1/chunks/{id}/prev` is called, **Then** the API returns up to `count` (default 5, max 100) adjacent chunks from the same document in `chunk_index` order, skipping `embed_failed` chunks, suitable for reading-order navigation. (PR #52 replaced the older `/siblings` endpoint with windowed next/prev plus document-scoped lookups under `/v1/documents/{id}`.)
5. **Given** a chunk id, **When** `GET /v1/chunks/{id}/parents` is called, **Then** the API returns the parent chain from the chunk's node up to the source-version root.
6. **Given** a request with `client_embedding_model` that does not match the active corpus model, **When** any search-or-chunk endpoint is hit, **Then** the API responds with HTTP 409 and a typed body of shape `{error: {code: "embedding_model_mismatch", message, remediation, context: {corpus_model, client_model}}}`.
7. **Given** any read endpoint, **When** any version of any source is not active, **Then** chunks/documents from that version are excluded by default; querying historical versions requires an explicit `?source_version_revision=N` parameter.
8. **Given** an anonymous request hits the per-IP rate limit, **When** the next request arrives, **Then** the API responds with HTTP 429, a `Retry-After` header, and a typed error body naming the limit and reset time.
9. **Given** a request with a valid GitHub-SSO bearer token, **When** the request arrives, **Then** the rate limit applies at the per-user (higher) tier from D11; `X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset` headers are returned on every response.
10. **Given** a request matching an active CIDR override entry, **When** the request arrives, **Then** the effective rate limit is the override's `limit_rps` until `expires_at`; CIDR matching is checked before the anonymous/SSO tier (D11).
11. **Given** filters in the search request body (`{attribution, verified, content_type, source_slug, language_target, sdk_dependency, package}`), **When** `POST /v1/search` runs, **Then** results are restricted to chunks whose document.provenance / source / package fields satisfy every filter (logical AND across keys, OR within each key's value array).
12. **Given** `GET /v1/models/active`, **When** called, **Then** the API returns the active embedding model identifier (`{name, revision, dim, provider}`) — used by clients to detect they need to pull a different model before issuing queries.
13. **Given** any request, **When** the response is sent, **Then** the response carries a stable `X-Request-Id` header for log correlation, the API version prefix is `/v1`, and the body is `application/json` matching a documented schema.
14. **Given** an empty `queries` array or a vector-dim mismatch even when `client_embedding_model` agrees, **When** `POST /v1/search` is called, **Then** the API responds with HTTP 400 and a typed `invalid_request` error naming the offending field.
15. **Given** the Postgres backend is temporarily unavailable, **When** any read endpoint is called, **Then** the API responds with HTTP 503 and a `Retry-After` header (Constitution VI graceful degradation); the server never crashes the request.

**Endpoint surface introduced by this story**:

```
POST /v1/search                              # hybrid search
GET  /v1/sources                             # list sources
GET  /v1/sources/{slug}                      # source detail
GET  /v1/sources/{slug}/versions             # list source_versions
GET  /v1/sources/{slug}/versions/{revision}  # version detail (active or historical)
GET  /v1/chunks/{id}                         # chunk detail
GET  /v1/chunks/{id}/next                    # next N chunks in reading order
GET  /v1/chunks/{id}/prev                    # prev N chunks in reading order
GET  /v1/chunks/{id}/parents                 # parent chain to source root
GET  /v1/documents/{id}                      # document detail (overview + chunk_ids)
GET  /v1/documents/{id}/full                 # document detail + inline chunk bodies (capped at 500)
GET  /v1/documents/{id}/chunks               # windowed chunk slice (from/limit)
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
    { "text": "how do I compile a Compact contract", "vector": [0.123, "..."] }
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
  "sort_by": "confidence",
  "min_confidence": 0.0,
  "include_scores": true
}
```

`sort_by` (added by Story 6, REV-001) ∈ `{confidence, trust, relevance, score}`, default `confidence`. `min_confidence` (also REV-001) ∈ `[0,1]`, default `0.0`; results below are filtered before `limit` is applied.

**Search response shape** (per result):

```json
{
  "chunk":              { "id", "content", "chunk_index", "total_chunks", "heading_path", "symbol_path", "start_byte", "end_byte", "token_count", "status" },
  "document":           { "id", "kind", "source_url", "published_url", "source_path", "language", "provenance" },
  "source":             { "slug", "display_name", "kind" },
  "source_version":     { "revision", "ingested_at" },
  "package":            { "kind", "name", "version" },
  "parent_chain":       [ { "id", "kind", "name", "order_index" } ],
  "navigation":         { "prev_chunk_id", "next_chunk_id" },
  "scores":             { "rrf": 0.0312, "fts_rank": 3, "vector_distance": 0.247, "matched_queries": [0, 1] },
  "trust_score":        0.91,
  "confidence":         0.87,
  "confidence_factors": { "...": "see Story 6" }
}
```

`trust_score`, `confidence`, and `confidence_factors` were added by Story 6 (REV-001) and are additive — pre-Story-6 callers ignoring them continue to work. The `search_metadata` envelope also gains `filtered_by_confidence` and `sort_by` per REV-001.

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

Documented error codes:

| Code | HTTP status | Introduced by |
|---|---|---|
| `invalid_request` | 400 | Story 4 |
| `unauthorized` | 401 | Story 4 |
| `forbidden` | 403 | Story 4 |
| `not_found` | 404 | Story 4 |
| `embedding_model_mismatch` | 409 | Story 4 (D12) |
| `run_aborted` | 409 | Story 9 |
| `run_already_finalized` | 409 | Story 9 (EC-57) |
| `nonce_consumed` | 401 | Story 9 (EC-55) |
| `nonce_expired` | 401 | Story 9 (EC-55) |
| `gone` | 410 | Story 4 (EC-33; retired version) |
| `payload_too_large` | 413 | Story 4 (EC-30) |
| `rate_limited` | 429 | Story 4 |
| `service_unavailable` | 503 | Story 4 |
| `query_timeout` | 504 | Story 4 (EC-28) |
| `schema_missing` | 503 | Story 9 (EC-65) |
| `unsupported_field` | 400 | Story 7 (EC-94) |
| `internal` | 500 | Story 4 |

All codes use the same typed envelope shape; new codes added via additive minor releases.

<details>
<summary>Supporting Decisions</summary>

- **D4**: Hybrid retrieval pattern — parallel FTS + pgvector with RRF in app code
- **D11**: Read auth tiers — anonymous + GitHub SSO uplift + CIDR override windows
- **D12**: Client supplies `client_embedding_model` on every request; server enforces match
- **D13**: Page-level `source_url` and `published_url` returned with every result
- **D14**: Active embedding model is `bge-base-en-v1.5` (768 dims)

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

### User Story 5 — Local MCP server: retrieval tools (Priority: P1)

**Revision**: v1.0

**As** a developer (or AI agent) using an MCP-capable assistant on a local machine,
**I need** an MCP server that exposes a small, stable set of retrieval tools — embedding queries locally, calling the cloud read API, reranking the top-K with a cross-encoder, and handling model-state errors with actionable remediation,
**So that** the agent gets fast, accurate, version-correct retrieval from the Midnight corpus without ever leaking query content off the machine and without the developer dropping back to a terminal when a model needs refreshing.

**Acceptance Scenarios**:

1. **Given** the MCP server is launched by an AI client, **When** the MCP handshake completes, **Then** the server returns within 500 ms cold start (Constitution IV), declares its tools and resources, and does not block on model loading.
2. **Given** the first `search` tool call arrives after handshake, **When** the server processes it, **Then** the embedding and reranker models are loaded lazily once (behind a one-shot guard so concurrent first-callers don't double-load), and subsequent calls reuse the in-memory models.
3. **Given** a `search` call with a single `query: string`, **When** processed, **Then** the server embeds the query locally with bge-base-en-v1.5, POSTs `{queries: [{text, vector}], client_embedding_model, filters, limit}` to the cloud `/v1/search` endpoint, reranks the top-K returned candidates with bge-reranker-base, and returns the top `limit` results to the agent.
4. **Given** a `search` call with `queries: string[]` (multi-query for HyDE / expansion per D3), **When** processed, **Then** each query is embedded locally and all pairs sent to `/v1/search` (which RRF-merges across queries); the merged candidate set is reranked locally and the top `limit` returned.
5. **Given** a `search` call with `rerank: false`, **When** processed, **Then** the server skips local reranking and returns the cloud's RRF-ordered top `limit`. Default is `true`.
6. **Given** the cloud responds with HTTP 409 `embedding_model_mismatch`, **When** any retrieval tool receives it, **Then** the tool returns a typed MCP error referencing `pull_models` with the corpus model name in the remediation message; the agent can call `pull_models` to self-heal.
7. **Given** the local embedding or reranker model is missing on first retrieval, **When** the tool runs, **Then** the server returns a typed `models_missing` error with the precise tool name to invoke (`pull_models`) and which model is needed.
8. **Given** the `pull_models` tool is called, **When** it runs, **Then** it downloads the embedding and reranker models to `$XDG_DATA_HOME/midnight-manual/models/`, emits MCP progress notifications during download, and returns `{embedding_model, reranker_model, total_bytes, took_ms}` on success.
9. **Given** the `status` tool is called, **When** it runs, **Then** it returns `{server_version, cloud_reachable, corpus_embedding_model, local_embedding_model, local_reranker_model, model_state, rate_limit_tier}` without requiring models to be loaded; `model_state` ∈ `{ready, missing, stale, loading, corrupt}`.
10. **Given** `get_chunk`, `get_chunk_next`, `get_chunk_prev`, `get_chunk_parents`, `get_document`, `get_document_full`, `get_document_chunks`, or `list_sources` is called, **When** processed, **Then** the server proxies to the corresponding cloud endpoint and returns the raw JSON result; no embedding or reranking is involved. `get_document_full` may surface a typed `too_many_chunks` JSON-RPC error (with `data.next_tool = "get_document_chunks"`) for documents over the 500-chunk inline cap.
11. **Given** the cloud is unreachable (network error / 503), **When** any retrieval tool is called, **Then** the tool returns a typed `service_unavailable` error including any `Retry-After` from the cloud response; the MCP server never crashes the AI client (Constitution V).
12. **Given** the config supplies a bearer token (per D17/D18) and the `MIDNIGHT_MANUAL_DISABLE_TELEMETRY` flag is unset, **When** any tool runs, **Then** the bearer is included in cloud requests via `Authorization: Bearer <token>` and an anonymized telemetry event (`tool_name`, `latency_ms`, `result_count`, `model_state`, `rerank_on`) is emitted — never query content, never the token, never chunk content (Constitution VII).
13. **Given** two concurrent `search` calls arrive while models are loading, **When** they are processed, **Then** both await the single in-flight load (no double-load, no double-download) and complete in order once models are ready.
14. **Given** the AI client kills the subprocess mid-request, **When** SIGTERM is received, **Then** the server cancels in-flight cloud calls cleanly, flushes any pending telemetry, and exits within 1 second.

**MCP tool surface**:

```
search              — primary retrieval tool (text → reranked chunks)
get_chunk           — fetch one chunk by id with full metadata
get_chunk_next      — fetch up to N chunks following a chunk (reading order)
get_chunk_prev      — fetch up to N chunks preceding a chunk (reading order)
get_chunk_parents   — walk the chunk's parent chain to source root
get_document        — document overview + ordered chunk_ids (no bodies)
get_document_full   — document + every chunk body inline (capped at 500)
get_document_chunks — windowed chunk slice of a document ({from, limit})
list_sources        — enumerate available sources for filter-narrowing
pull_models         — download/update local embedding and reranker models
status              — health and model-state introspection
```

**`search` input schema**:

```json
{
  "type": "object",
  "properties": {
    "query":   { "type": "string", "description": "Single query (convenience for casual callers)." },
    "queries": { "type": "array", "items": { "type": "string" }, "description": "Multi-query input for HyDE or expansion; sophisticated callers may pass several reformulations of the user's intent." },
    "limit":   { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
    "rerank":  { "type": "boolean", "default": true },
    "filters": { "type": "object", "description": "Same shape as cloud /v1/search filters: source_slug, attribution, verified, content_type, language_target, sdk_dependency, package." }
  },
  "oneOf": [ { "required": ["query"] }, { "required": ["queries"] } ]
}
```

**`search` result shape** (per result):

Same as the cloud `/v1/search` per-result shape (chunk, document, source, source_version, package, parent_chain, navigation, scores), with an additional top-level `rerank_score` field when `rerank=true`.

**Lazy model loading**: the MCP handshake completes immediately by declaring tools and resources from a static manifest. ONNX model load (~600–700 MB combined RSS for embedder + reranker) is deferred to first retrieval call, behind a single guard so concurrent first-callers share one load. Cold start stays under the 500 ms Constitution IV budget; first retrieval pays a one-time ~1.5 s model-load cost.

**Limit cap rationale**: the `search` tool caps `limit` at 50 even though the cloud `/v1/search` accepts up to 100. The MCP-side cap accommodates the reranker's per-pair latency budget — reranking 50 candidates with bge-reranker-base on CPU is ~400 ms (well under the 1 s p95 constitutional budget); 100 candidates would risk exceeding it. Callers that need more candidates without reranking should set `rerank: false` and accept the cloud's RRF order.

**Configuration** (per D17/D18, loaded once at startup):

```toml
# $XDG_CONFIG_HOME/midnight-manual/config.toml
[server]
url = "https://manual.midnight.network"

[models]
embedding = "bge-base-en-v1.5"
reranker  = "bge-reranker-base"
cache_dir = "~/.local/share/midnight-manual/models"

[telemetry]
enabled = true
```

`MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1` overrides the file. `--config <path>` overrides discovery.

**Authentication** (D28): the MCP server resolves the read-uplift bearer in this order — `--token <jwt>` flag, then `MIDNIGHT_MANUAL_TOKEN` env var, then `auth.toml[read_uplift].token` (see Story 8), then anonymous mode (EC-39). The MCP server NEVER reads the admin token; admin credentials live exclusively in the interactive CLI scope.

The auth file at `$XDG_CONFIG_HOME/midnight-manual/auth.toml` (chmod 0600) has shape:

```toml
schema_version = 1

[admin]                                  # Used by CLI write commands only
user_id    = "aaron"
token      = "eyJhbG..."                # 1-hour HS256 JWT (D21)
expires_at = "2026-05-13T15:30:00Z"

[read_uplift]                            # Used by MCP server and CLI read commands
github_login = "aaron-bassett"
token        = "ru_abc123..."            # 30-day bearer (D28, configurable via MIDNIGHT_MANUAL_READ_TOKEN_TTL_DAYS)
expires_at   = "2026-06-12T14:00:00Z"
```

If the file is missing both sections, the MCP server starts anonymously and surfaces `rate_limit_tier = "anonymous"` via the `status` tool. If the file is malformed or has an unknown `schema_version`, the MCP server fails handshake with exit code 78 (config error per EC-38).

<details>
<summary>Supporting Decisions</summary>

- **D1**: Embedding library — `fastembed-rs`
- **D2**: Server-side cross-encoder reranking (client-side relative to cloud; runs in the MCP server)
- **D3**: Caller-delegated query rewriting — `queries: string[]` input supports HyDE
- **D12**: Client supplies `client_embedding_model` on every cloud request; structured 409 on mismatch
- **D14**: Embedding model — `bge-base-en-v1.5` (768 dims)
- **D17 / D18**: Global flags and config discovery

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

### User Story 3 — Admin ingestion of source-code repos via CLI (Priority: P1)

**Revision**: v1.0

**As** a Midnight Content Maintainer,
**I need** a CLI command that ingests a tree of source files (or a remote git repo) — chunking by AST-aware boundaries where possible, detecting package membership per language, and re-using all the upload, auth, and lifecycle plumbing from Markdown ingest —
**So that** the corpus carries Rust, TypeScript/JavaScript, and Compact code with the same hierarchical and provenance guarantees as Markdown content, and a partner-curated repo of examples can be re-ingested by CI on every upstream commit.

**Acceptance Scenarios**:

1. **Given** a source `compact-examples` registered as `kind=code_repo` and a local directory containing Rust, TypeScript, and Compact files, **When** I run `mnm ingest code compact-examples ./path`, **Then** every recognized source file is chunked (tree-sitter for known languages, line-window fallback otherwise), package membership is assigned per language, and a new source_version is built and promoted.
2. **Given** a Rust file at `pkg/src/lib.rs` with a `Cargo.toml` at `pkg/Cargo.toml` declaring `name = "midnight-foo"`, **When** ingested, **Then** chunks emitted from that file carry `package = {kind: "rust", name: "midnight-foo", manifest_path: "pkg/Cargo.toml"}`.
3. **Given** a TypeScript file at `pkgs/web/src/index.ts` with `package.json` at `pkgs/web/package.json` declaring `"name": "@midnight-ntwrk/web"`, **When** ingested, **Then** chunks carry `package = {kind: "npm", name: "@midnight-ntwrk/web", manifest_path: "pkgs/web/package.json"}`.
4. **Given** a Compact file `contracts/src/token/FungibleToken.compact` with `module FungibleToken { ... }` at top level, **When** ingested, **Then** every chunk inside the module's byte range carries `package = {kind: "compact", name: "FungibleToken", manifest_path: null}`; content outside any module declaration carries `package = null`.
5. **Given** a Compact file with two top-level modules, **When** ingested, **Then** chunks in each module are tagged with their enclosing module name; multiple `package` rows exist for the same file (one per module).
6. **Given** a Cargo workspace with three member crates, **When** ingested, **Then** the workspace virtual root (a `Cargo.toml` with `[workspace]` and no `[package]`) is ignored for package detection and each `.rs` file resolves to its member's `Cargo.toml`; the source_version contains exactly three Rust packages.
7. **Given** a repo with `node_modules/`, `target/`, `vendor/`, `dist/`, and `.git/`, **When** ingested, **Then** these directories are skipped by default; configurable via `--include <glob>` / `--exclude <glob>`.
8. **Given** the repo's `.gitignore` matches certain files, **When** ingested, **Then** matched files are skipped by default; `--no-respect-gitignore` disables.
9. **Given** the `--git <url>` flag (with optional `--ref <branch|tag|sha>`), **When** ingest runs, **Then** the CLI clones the repo into a temp directory, ingests it, and removes the temp directory on exit regardless of success or failure.
10. **Given** a file whose language has no tree-sitter grammar loaded, **When** ingested, **Then** it falls back to a line-window chunker (default 60 lines, 20-line overlap, both configurable) and is indexed with `language = <ext>` and `symbol_path = []`.
11. **Given** a tree-sitter parser encounters a syntax error in an otherwise-supported file, **When** processing that file, **Then** the chunker falls back to a line-window for that file, emits a warning naming the file and the parser error, and continues with subsequent files.
12. **Given** a binary file (detected by magic-number sniff) appears under the ingest path, **When** ingest runs, **Then** the file is skipped with a warning and counted in `summary.skipped_files`.
13. **Given** the `--dry-run`, `--json`, `--strict`, and `--force-new` flags, **When** ingest runs, **Then** they behave identically to `mnm ingest md` (re-uses FR-018, FR-019, FR-020, FR-021).

**CLI surface introduced by this story**:

> **Superseded (2026-05-28):** there is no separate `mnm ingest code` command. Code
> chunking is handled by the unified `mnm ingest run` (see the Story 2 superseding note),
> which dispatches to the correct chunker per file by extension. The code-specific flags
> (`--code-chunk-tokens`, `--code-chunk-lines`, `--code-chunk-overlap`, `--include`,
> `--exclude`, `--max-file-size`) hang on `mnm ingest run`. Git-mode clone-and-ingest
> (`--git`/`--ref`) is deferred to its own follow-up; it is a source-acquisition feature
> orthogonal to chunking. The snippet below is retained for historical trace only.

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

**Default exclusions**:

- `node_modules/`, `target/`, `vendor/`, `dist/`, `build/`, `out/`, `coverage/`, `.git/`
- Lockfiles: `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`, `Cargo.lock`
- Common generated patterns: `*.min.js`, `*.bundle.js`, `*.generated.ts`, `*_pb.ts`, `*_pb.rs`

All defaults are composable with explicit `--include` / `--exclude`.

**Language → tree-sitter grammar mapping**:

| Extensions | Grammar | Symbol path source |
|---|---|---|
| `.rs` | tree-sitter-rust | mod / impl / struct / enum / fn |
| `.ts`, `.tsx` | tree-sitter-typescript | namespace / class / interface / function / method |
| `.js`, `.jsx`, `.mjs`, `.cjs` | tree-sitter-javascript | class / function / method |
| `.compact` | hand-rolled top-level scanner (until a tree-sitter grammar exists); chunks by `module Foo { ... }` for package detection plus line-window inside modules | module names |
| _other_ | line-window fallback | `[]` |

**Package detection rules**:

- **Rust**: walk up from each `.rs` file to the nearest `Cargo.toml` containing a `[package]` section; workspace virtual roots are skipped.
- **TypeScript / JavaScript**: walk up to the nearest `package.json` with a `"name"` field; if `"name"` is missing, fall back to the manifest's directory name and emit a warning.
- **Compact**: parse the file's top-level `module <Name> { ... }` blocks; tag chunks by enclosing module's byte range. No filesystem manifest. Files with no module declaration: `package = null`.
- **Other**: `package = null`.

**Cloud write protocol**: identical to Story 2 (`POST /v1/sources/{slug}/ingest-runs`, `PUT .../documents`, `POST .../finalize`, `POST .../abort`).

<details>
<summary>Supporting Decisions</summary>

- **D1**: Embedding library — `fastembed-rs` (re-used at ingest time)
- **D6**: Code chunking — tree-sitter for known languages, line-window fallback
- **D8 / D15**: Source-version snapshots, retention 5
- **D9**: Compact package detection — in-source `module Foo {` declarations
- **D13**: Page-level `source_url` and `published_url` (still applies; defaults derive from `--git` URL when supplied)
- **D14**: Embedding model — `bge-base-en-v1.5`
- **D16 / D17 / D18 / D19**: CLI shape

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

### User Story 9 — Cloud server: auth, write API, deploy, ops (Priority: P2)

**Revision**: v1.0

**As** a Midnight Network operator (and the admins and developers consuming the read and write surfaces),
**I need** the cloud server to materialize the implicit write protocol from Stories 2 and 3, implement the auth flows from D10 and D11, expose admin operations for CIDR overrides, and ship as a deployable Fly.io artifact with sane sweep jobs, health endpoints, and migrations,
**So that** Stories 2/3 have a concrete endpoint to talk to, the read API has working rate-limiting tiers, the corpus self-maintains under retention rules, and the entire service can be deployed and rolled back by a single CI pipeline.

**Acceptance Scenarios**:

1. **Given** a valid Ed25519 keypair registered in the deployed user store (D10/D20), **When** the CLI runs `mnm login`, **Then** the CLI calls `POST /v1/auth/challenge` (announces `user_id`, receives challenge nonce), signs the nonce, calls `POST /v1/auth/verify` (sends `user_id` + signature), receives a 1-hour HS256 JWT, and writes it to `auth.toml[admin]` with permissions 0600 (D28).
2. **Given** an admin holds a valid JWT (D21), **When** the CLI calls `POST /v1/sources/{slug}/ingest-runs`, **Then** the server creates a `source_version` in `building` state and returns `{ingest_run_id, source_version_id, source_version_revision}`; the response is 401 if the JWT is missing or invalid and 403 if the user lacks the required role.
3. **Given** an admin has an active ingest_run, **When** they call `PUT /v1/sources/{slug}/ingest-runs/{id}/documents` with a batch of `{document, chunks}` pairs, **Then** the server inserts rows under the run's `source_version_id`; replaying the same batch (same content hashes) returns 200 as a no-op (idempotent on hash).
4. **Given** an admin completes uploads, **When** they call `POST /v1/sources/{slug}/ingest-runs/{id}/finalize`, **Then** in one DB transaction the new source_version is set `is_active=true` and the prior active version is demoted; the response carries `{source_version_id, revision, is_active: true, demoted_revision}`.
5. **Given** an admin aborts an ingest_run, **When** they call `POST .../abort`, **Then** the server marks the source_version `aborted`; subsequent `PUT`/`finalize` calls on that run id return 409 with a typed `run_aborted` error.
6. **Given** an unauthenticated read request, **When** `POST /v1/search` is called, **Then** it succeeds at the anonymous rate-limit tier (per-IP, D11) with appropriate `X-RateLimit-*` headers; rate-limit decisions consult CIDR overrides first, then SSO tier, then anonymous (FR-031).
7. **Given** a user runs `mnm auth github` (or the equivalent web flow), **When** `GET /v1/auth/github/start` is hit, **Then** the server redirects to GitHub OAuth; the `callback` exchanges the code, verifies the user is a member of the configured Midnight GitHub org, and mints a 30-day read-uplift bearer token (configurable via `MIDNIGHT_MANUAL_READ_TOKEN_TTL_DAYS`, D28). The bearer grants read-uplift rate-limit tier only — never write permissions.
8. **Given** an admin holds a JWT and calls `POST /v1/admin/ratelimits` with `{cidr, limit_rps, expires_at, note}`, **When** processed, **Then** a `rate_limit_override` row is created and immediately effective; `GET /v1/admin/ratelimits` lists active overrides; `PATCH` extends one; `DELETE` removes one.
9. **Given** the server starts up, **When** it boots, **Then** it (a) loads the user store from `MIDNIGHT_MANUAL_USER_STORE`, (b) loads the JWT signing secret from `MIDNIGHT_MANUAL_JWT_SECRET`, (c) connects to the database, (d) runs pending migrations (unless `MIDNIGHT_MANUAL_AUTO_MIGRATE=false`), (e) seeds `embedding_model` with the active model row if absent, and (f) starts the HTTP listener; any of (a)–(d) failing exits the process non-zero with a structured error to stderr.
10. **Given** a source_version has been inactive for the configured grace window (default 24h), **When** the periodic sweep job runs, **Then** it deletes the version's chunks, documents, nodes, and packages in dependency order in a single transaction, and removes the source_version row.
11. **Given** the server has been running for > 1h with no successful DB query, **When** the `/readyz` endpoint is hit, **Then** the server reports 503 with the most recent DB error in the typed error body; `/healthz` still reports 200 (process is alive).
12. **Given** a request body or query causes any of the documented error codes, **When** processed, **Then** the response follows the typed error envelope from Story 4 (`{error: {code, message, remediation, context}, request_id}`) with the appropriate HTTP status.
13. **Given** the JWT signing secret is rotated (Fly.io secret update + redeploy), **When** the new process starts, **Then** all previously-issued admin tokens fail verification; admins re-authenticate by re-running `mnm login`.
14. **Given** a request arrives with a JWT signed by a different secret (or expired, or with an invalid signature), **When** processed, **Then** the server returns 401 `unauthorized` with remediation `Run 'mnm login' to obtain a fresh token`.

**Endpoint surface introduced by this story** (additions to Story 4's surface):

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

**User store TOML schema** (loaded once at startup, D20):

```toml
schema_version = 1

[[users]]
user_id    = "aaron"
role       = "admin"            # admin | writer | (future roles)
public_key = "ed25519:base64..."
created_at = "2026-05-13"
note       = "founding admin"

[[users]]
user_id    = "ci-bot"
role       = "writer"
public_key = "ed25519:base64..."
created_at = "2026-05-14"
```

Schema is versioned; unknown fields are rejected (fail-fast at startup).

**Fly.io deploy posture**:

- One Fly app: `midnight-manual` (single region at launch — `lhr` or `iad`).
- One Fly managed Postgres cluster with pgvector enabled.
- Required Fly secrets (validated at startup):
  - `DATABASE_URL` — Fly Postgres connection string
  - `MIDNIGHT_MANUAL_JWT_SECRET` — HS256 signing secret (32+ bytes random)
  - `MIDNIGHT_MANUAL_USER_STORE` — TOML body of the user store (mounted at boot)
  - `MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_ID`, `MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET`, `MIDNIGHT_MANUAL_GITHUB_ORG`
- Image: Rust binary in `gcr.io/distroless/cc`; multi-stage build via `cargo-chef`.
- Continuous release: every merge to `main` triggers GitHub Actions → Fly deploy.
- Multi-region path: documented but out of scope for v1.

**Sweep job**: a background tokio task runs every 5 minutes (configurable). For each source, list source_versions older than the `retention_count`-th most recent; any older than `MIDNIGHT_MANUAL_SWEEP_GRACE` (default 24h) since marked inactive get their chunks/documents/nodes/packages deleted in a single transaction, followed by the source_version row. Aborted ingest_runs older than `MIDNIGHT_MANUAL_ABORT_GRACE` (default 1h) are also swept.

**Migrations**: `sqlx migrate` invoked at startup unless `MIDNIGHT_MANUAL_AUTO_MIGRATE=false` (D22). Migrations are forward-only, idempotent, and shipped in `migrations/` as numbered SQL files.

<details>
<summary>Supporting Decisions</summary>

- **D10**: Admin auth — Ed25519 challenge-response with file-backed user store
- **D11**: Read auth — anonymous + GitHub SSO uplift + CIDR overrides
- **D15**: Retention = 5
- **D20**: User store is a deployable artifact, load-only at startup
- **D21**: Admin tokens are HS256-signed JWTs with 1-hour TTL
- **D22**: Migrations auto-run at startup behind a flag

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

### User Story 8 — CLI admin lifecycle (Priority: P2)

**Revision**: v1.0

**As** a Midnight Network admin (running the CLI interactively or in CI), and as a DApp developer (running the same binary for model and MCP setup),
**I need** a complete admin-facing command surface — version lifecycle, user/key management, ratelimit overrides, model lifecycle, MCP installation, diagnostics, login, and migration preflight — with admin commands cleanly hidden from default help output so developers see a small, focused surface,
**So that** every operation in the system has a scriptable, audited CLI command and the two audiences don't pollute each other's `--help`.

**Acceptance Scenarios**:

1. **Given** the default config (no admin mode), **When** I run `mnm --help`, **Then** the output lists only developer-facing commands: `search`, `sources list`, `sources show`, `versions list`, `versions show`, `models`, `mcp`, `doctor`, `config`. Admin commands are NOT shown.
2. **Given** `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` or `cli.show_admin_cmds = true` in config, **When** I run `mnm --help`, **Then** every command is listed, including admin commands.
3. **Given** admin commands are hidden, **When** I run `mnm users list` directly, **Then** it executes normally — visibility never blocks invocation (D23).
4. **Given** I have not run `mnm keys generate`, **When** I run `mnm login --user-id aaron`, **Then** the CLI emits an error pointing at `mnm keys generate` (no local keypair found). When a keypair exists, the CLI completes the challenge-response (Story 9), writes the resulting JWT to the `[admin]` section of `$XDG_CONFIG_HOME/midnight-manual/auth.toml` (file created with permissions 0600 if absent), and prints `logged in as aaron, admin token expires in 60m` (D28).
5. **Given** I run `mnm keys generate`, **When** the command executes, **Then** an Ed25519 keypair is written to `$XDG_CONFIG_HOME/midnight-manual/keys/<user_id>.{public,private}` with permissions `0600` on the private half; the public half is also echoed to stdout in the TOML row format ready to paste into the user-store TOML.
6. **Given** I run `mnm users add --user-id ci-bot --role writer --public-key ed25519:abc... --note "CI pipeline"`, **When** processed, **Then** the local user-store TOML at the path resolved per D18 is updated; `schema_version` is preserved; the CLI emits a warning that the change is local only and points at the deploy step needed (D20).
7. **Given** I run `mnm users list` (or `update`, `show`, `remove`), **When** processed, **Then** the local user-store TOML is read or edited accordingly; with `--json` the output is the user store as a JSON document.
8. **Given** I run `mnm versions promote midnight-docs --revision 12`, **When** processed against a logged-in admin context, **Then** the CLI calls `POST /v1/sources/midnight-docs/versions/12/promote` (Story 9), and on success prints `promoted revision 12; demoted revision 13`.
9. **Given** I run `mnm versions rollback midnight-docs`, **When** processed, **Then** the CLI looks up the most recent prior active version (revision N-1) and calls the same promote endpoint with that revision; if no prior version exists, the command exits non-zero with a clear error.
10. **Given** I run `mnm versions retire midnight-docs --revision 9`, **When** processed, **Then** the CLI calls `POST /v1/sources/midnight-docs/versions/9/retire`; the version is marked retired and becomes eligible for sweep (Story 9 FR-063).
11. **Given** I run `mnm ratelimits add --cidr 169.155.237.15/25 --limit 200/s --ttl 48h --note "hackathon-london"`, **When** processed, **Then** the CLI calls `POST /v1/admin/ratelimits`. `mnm ratelimits list`, `mnm ratelimits extend <id> --ttl 24h`, and `mnm ratelimits remove <id>` use the corresponding endpoints.
12. **Given** I run `mnm models pull`, **When** processed, **Then** the CLI fetches `GET /v1/models/active` to learn the corpus model, downloads the embedding model and reranker to `$XDG_DATA_HOME/midnight-manual/models/`, verifies digests, and prints a summary. `mnm models list` enumerates locally-installed models; `mnm models prune` removes models not matching the active corpus model (`--keep <name>` overrides).
13. **Given** I run `mnm mcp install [--agent claude-code|cursor|...] [--config-path <path>]`, **When** processed, **Then** the CLI updates the named agent's MCP config file (or prints the JSON snippet for manual install when the agent isn't recognized); on success, prints the agent's config path and the snippet that was applied.
14. **Given** I run `mnm doctor`, **When** processed, **Then** the CLI emits a structured diagnostic report covering: CLI version, embedding and reranker model presence and version, MCP server installation status across known agents, cloud server reachability (HEAD `/healthz`), corpus model match status, local keypair presence, login state, admin-visibility flag, and config file location. With `--json` the report is a single JSON object.
15. **Given** I run `mnm db migrate` (admin), **When** processed, **Then** the CLI executes pending migrations against the configured `DATABASE_URL`; intended for deploy-time preflight when `MIDNIGHT_MANUAL_AUTO_MIGRATE=false` (D22). `mnm db status` prints applied vs pending migrations.
16. **Given** I run any command with `--json`, **When** processed, **Then** all output goes to stdout as a single JSON document (single-record commands) or NDJSON (streaming/progressive commands); no human-formatted text touches stdout (FR-021).

**Complete CLI command tree** (Story 8 finalizes; earlier stories introduced subsets):

```
mnm
├── search <query> [--query <alt>]... [--limit N] [--rerank] [--queries-stdin]   # developer; doubles as the admin multi-query debug helper (Story 7)
├── sources
│   ├── list                                            # developer
│   ├── show <slug>                                     # developer
│   ├── add <slug> --kind ... [--origin-url ...]       # admin
│   ├── update <slug> [...]                             # admin
│   └── retire <slug>                                   # admin
├── versions
│   ├── list <slug>                                     # developer
│   ├── show <slug> <revision>                          # developer
│   ├── promote <slug> --revision N                     # admin
│   ├── rollback <slug>                                 # admin (promotes most-recent prior active)
│   └── retire <slug> --revision N                      # admin
├── ingest                                              # admin (entire subtree)
│   ├── md <slug> <path> [...]                          # Story 2
│   └── code <slug> <path> [...]                        # Story 3
├── models
│   ├── pull [--name <model>]                           # developer
│   ├── list                                            # developer
│   └── prune [--keep <name>]                           # developer
├── mcp
│   ├── install [--agent <name>] [--config-path <path>] # developer
│   └── status                                          # developer
├── users                                               # admin (edits local user-store TOML; D20)
│   ├── add --user-id ... --role ... --public-key ...
│   ├── list
│   ├── show <user_id>
│   ├── update <user_id> [...]
│   └── remove <user_id>
├── keys                                                # admin
│   ├── generate [--user-id <id>]
│   └── import --user-id ... --public-key ...
├── ratelimits                                          # admin
│   ├── add --cidr ... --limit ... --ttl ... [--note ...]
│   ├── list
│   ├── extend <id> --ttl ...
│   └── remove <id>
├── login --user-id ...                                 # admin (challenge-response per Story 9; writes auth.toml[admin])
├── logout                                              # admin (clears auth.toml[admin])
├── auth                                                # developer (D28; read-uplift bearer for higher rate limit)
│   ├── github [--no-browser]                           # developer (OAuth flow; web by default, device flow with --no-browser; writes auth.toml[read_uplift])
│   ├── status                                          # developer (reports presence and expiry of both tokens)
│   └── logout                                          # developer (clears auth.toml[read_uplift] only)
├── telemetry                                           # developer (Story 11)
│   ├── status [--json]
│   ├── disable
│   └── enable
├── db                                                  # admin
│   ├── migrate                                         # preflight migration runner (D22)
│   └── status
├── config
│   ├── show [--effective]                              # developer; --effective resolves env+flag overrides
│   ├── get <key>                                       # developer
│   └── set <key> <value>                               # developer (writes the user config file)
├── doctor                                              # developer (universal diagnostic)
└── version                                             # developer (also accepts --version as an alias on any subcommand)
```

**Visibility rules (D23)**:

- **Hidden by default**: `sources add/update/retire`, `versions promote/rollback/retire`, the entire `ingest`, `users`, `keys`, `ratelimits`, `login`, `logout`, `db` subtrees.
- **Visible by default**: `search`, `sources list/show`, `versions list/show`, `models`, `mcp`, `auth`, `telemetry`, `config`, `doctor`, `version`.
- **Toggle**: `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` env or `cli.show_admin_cmds = true` in config (env wins per D18). Toggle affects help output only; invocation is never gated.
- `mnm doctor` always reports the current admin-visibility state.

**Local user-store editing (D20)**:

The `mnm users` subtree edits a local TOML file. After every mutation the CLI emits a warning reminding the admin that the change does not take effect until the file is deployed (Fly secret update + redeploy). With `--json` the warning is a structured event in NDJSON output rather than human text on stderr. The CLI MUST refuse to overwrite a file whose `schema_version` doesn't match the binary's supported version.

<details>
<summary>Supporting Decisions</summary>

- **D10**: Ed25519 challenge-response admin auth (consumed via `mnm login`)
- **D11**: Read auth tiering (consumed via `mnm ratelimits` CIDR overrides)
- **D12**: ML model lifecycle (`mnm models` subtree)
- **D16 / D17 / D18 / D19**: CLI shape
- **D20**: User store load-only model (drives `mnm users` semantics)
- **D21**: JWT TTL (drives `mnm login` UX)
- **D22**: Migrations auto-run + opt-out (drives `mnm db migrate`)
- **D23**: Admin commands hidden by default

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

### User Story 6 — Confidence scoring & result ranking (Priority: P2)

**Revision**: v1.0

**As** a developer (or AI agent) consuming search results, and as a Midnight Network maintainer tuning corpus quality,
**I need** every result to carry a confidence score that blends content trust (from provenance) with retrieval relevance (from hybrid + reranker), plus a per-factor breakdown that lets the consumer explain *why* a result is trustworthy or weak,
**So that** agents can prefer trusted answers over barely-relevant guesses, surface freshness and verification information in citations, and maintainers can tune the corpus by changing scoring policy rather than code.

**Acceptance Scenarios**:

1. **Given** a search request, **When** the cloud computes results, **Then** every result carries `trust_score ∈ [0.0, 1.0]`, `confidence ∈ [0.0, 1.0]`, and `confidence_factors` (an object naming the inputs); these fields are additive to the existing per-result schema — no contract break per Constitution I.
2. **Given** two results identical except for `provenance.attribution`, **When** scored, **Then** the foundation-attributed result has a higher `trust_score` than partner, which is higher than third_party, which is higher than community, which is higher than unknown.
3. **Given** two results identical except for `provenance.verified`, **When** scored, **Then** the verified result has a higher `trust_score` than the unverified one; the multiplier comes from the loaded scoring policy.
4. **Given** two results identical except for `source_modified_at` (one 14 days old, one 2 years old), **When** scored, **Then** the fresher result has a higher `trust_score`; freshness decays exponentially with a configurable half-life (default 180 days).
5. **Given** a result with `provenance.deprecation.is_deprecated = true`, **When** scored, **Then** its `trust_score` is reduced by the deprecation penalty (default ×0.3); the result still appears unless filtered out by `min_confidence`.
6. **Given** a search request with `filters.language_target.version_constraint_satisfies = "0.31"`, **When** scored, **Then** results whose `provenance.language_targets` satisfy that constraint receive a version-match boost; results that miss the constraint receive a version-miss penalty; results with no language_targets are neutral.
7. **Given** `rerank=false` on the MCP search call, **When** the cloud composes its response, **Then** `confidence` is computed using the normalized RRF score as the relevance term; the MCP server passes the cloud's confidence through unchanged.
8. **Given** `rerank=true` on the MCP search call, **When** the MCP server processes results, **Then** it replaces the relevance term with the normalized reranker score and recomputes `confidence` using the same blend formula; `confidence_factors.relevance_source = "rerank"` records the substitution.
9. **Given** a search request with no explicit `sort_by`, **When** results are ranked, **Then** they are returned sorted by `confidence` descending. With `sort_by = "trust"` they are sorted by `trust_score`. With `sort_by = "relevance"` they are sorted by the relevance term used. With `sort_by = "score"` they are sorted by the underlying RRF score (the existing Story 4 default).
10. **Given** a search request with `min_confidence = 0.5`, **When** results are filtered, **Then** results below 0.5 confidence are excluded before the limit is applied; `search_metadata.filtered_by_confidence` reports the count dropped.
11. **Given** the cloud server starts up with `MIDNIGHT_MANUAL_SCORING_POLICY` pointing at a valid TOML file, **When** the policy is loaded, **Then** it is validated and held in memory; absence falls back to compiled-in defaults; invalid policy fails startup (Constitution VI / VIII).
12. **Given** an MCP agent inspects `confidence_factors`, **When** building an explanation, **Then** the breakdown carries enough information to write a sentence like "this is from the Foundation, verified on 2026-04-01, last updated 14 days ago, targets Compact ≥ 0.31" without further API calls.
13. **Given** the scoring policy weights produce a value outside [0.0, 1.0], **When** the cloud finishes computing `confidence` or `trust_score`, **Then** the value is clamped to the range and a structured warning is logged.

**Scoring policy TOML schema** (loaded from `MIDNIGHT_MANUAL_SCORING_POLICY` at startup; compiled defaults otherwise):

```toml
schema_version = 1

[attribution]
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
half_life_days = 180.0
fallback_age_source = "ingested_at"   # when source_modified_at is null

[deprecation]
penalty_multiplier = 0.30

[version_match]
satisfies   = 1.15
neutral     = 1.00
unsatisfied = 0.70

[blend]
# confidence = trust_score^trust_weight * relevance^relevance_weight
trust_weight     = 0.55
relevance_weight = 0.45
```

The TOML is loaded once at startup; weights are validated for finite, non-negative values; unknown keys fail the load (Constitution VIII fail-fast).

**Trust score computation**:

```
base   = attribution_multiplier(provenance.attribution)
ver    = verification_multiplier(provenance.verified, provenance.verified_by)
fresh  = exp(-age_days / half_life_days)                        # age from source_modified_at, else ingested_at
dep    = deprecation_multiplier(provenance.deprecation.is_deprecated)
vmatch = version_match_multiplier(query_filters.language_target, provenance.language_targets)

trust_score = clamp(base * ver * fresh * dep * vmatch, 0.0, 1.0)
```

**Relevance term**:

- Cloud response: normalized RRF score from Story 4. Mapping: `relevance_rrf = 1 - 1/(1 + raw_rrf_score)` (bounded to [0,1], monotonic in raw score).
- MCP server with rerank=true: normalized cross-encoder score from `bge-reranker-base`, sigmoid-mapped to [0,1]: `relevance_rerank = 1 / (1 + exp(-raw_logit))`.

Both normalization functions are compiled-in (not policy-configurable in v1) to ensure reproducibility — changing the normalization would silently alter every confidence score in the corpus.

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
    "language_target_query":  { "name": "compact", "version_constraint_satisfies": "0.31" },
    "language_targets_chunk": [{ "name": "compact", "version_constraint": ">=0.23" }],
    "version_match_multiplier": 1.15,
    "relevance_source": "rerank",
    "relevance_multiplier": 0.873
  }
}
```

**Revisions to existing graduated stories**:

This story produces additive revisions (no contract breaks) to:
- **Story 4 (Cloud read API)** — per-result response shape gains `trust_score`, `confidence`, `confidence_factors`; request body accepts `sort_by ∈ {confidence, trust, relevance, score}` and `min_confidence ∈ [0,1]`.
- **Story 5 (MCP server)** — search result shape gains the same fields; MCP server recomputes `confidence` from the reranker score when `rerank=true`.

Both are forward-compatible: existing callers ignoring the new fields continue to work unchanged.

<details>
<summary>Supporting Decisions</summary>

- **D2**: Cross-encoder reranking (whose score feeds the relevance term when rerank=true)
- **D4**: Hybrid RRF (whose score feeds the relevance term when rerank=false)
- **D14**: Embedding model (no scoring impact, but stable for query-side embedding)
- **D24**: Trust vs relevance separation, TOML-configured weights, geometric-mean blend

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

### User Story 7 — Query enhancement: multi-query support and cookbook (Priority: P2)

**Revision**: v1.0

**As** an AI agent calling the MCP server (and the Midnight Network team supporting agent developers),
**I need** the multi-query input shape from D3 to be explicitly specified, fairly rate-limited, well-instrumented, and well-documented with worked examples in a shipped cookbook,
**So that** sophisticated callers can apply HyDE, multi-query expansion, and step-back prompting to lift recall, while casual callers continue to work unchanged and abusive inputs are bounded.

**Acceptance Scenarios**:

1. **Given** `POST /v1/search` with `queries: [{text, vector}]` (N pairs, 1 ≤ N ≤ 10), **When** the server processes the request, **Then** hybrid retrieval runs once per query pair (FTS + pgvector), and RRF (k=60) merges across both retrieval modes and across query pairs in one pass.
2. **Given** the merged candidate set, **When** a result is returned, **Then** `scores.matched_queries` lists the input query indices (0-based) that contributed at least one of FTS/vector rank to the result.
3. **Given** any multi-query response, **When** the response is built, **Then** `search_metadata.per_query` carries one record per input query: `{query_index, fts_candidates, vector_candidates, fts_latency_ms, vector_latency_ms}`.
4. **Given** a request with `queries.length > MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST` (default 10), **When** the request is validated, **Then** the server returns 400 `invalid_request` naming the cap, before consuming any rate-limit tokens.
5. **Given** a multi-query request with `queries.length = N`, **When** the rate-limiter accounts for it, **Then** the request consumes `max(1, N)` tokens from the caller's bucket per D25; the `X-RateLimit-Remaining` header reflects the post-charge balance.
6. **Given** the single-query convenience form (`{query: "text", vector: [...]}`), **When** the server processes it, **Then** behavior is identical to `queries: [{text: "text", vector: [...]}]`; an internal test verifies byte-identical responses for the two shapes.
7. **Given** `queries: []` or every entry has an empty `text` field, **When** the request is validated, **Then** the server returns 400 `invalid_request`.
8. **Given** the MCP server exposes its `search` tool, **When** an agent inspects the tool description, **Then** the description includes a "Patterns" section documenting three named techniques — `hyde`, `multi_query`, `step_back` — each with a 1–3 line example.
9. **Given** the repo's `docs/cookbook/query-enhancement.md`, **When** a contributor or third-party agent author opens it, **Then** they find a runnable cookbook with worked examples for HyDE, multi-query paraphrase, and step-back prompting, each showing the LLM prompt(s) the calling agent emits and the resulting `queries` array passed to the MCP `search` tool.
10. **Given** a benchmark of 50 labelled query/relevant-chunk pairs, **When** retrieval is measured under (a) single-query and (b) 3-query multi-query (expansion paraphrases) at the same `limit=10`, **Then** the multi-query recall@10 exceeds single-query recall@10 by at least 8 percentage points absolute.
11. **Given** an admin-mode CIDR rate-limit override is active for the caller, **When** a multi-query request arrives, **Then** the override's `limit_rps` applies to the post-multiplied cost (a 200-req/s override accommodates ~40 five-query requests per second).
12. **Given** the CLI's debug `mnm search` command supports multi-query via repeated `--query` flags or a `--queries-stdin` JSON shape, **When** invoked, **Then** the CLI emits the per-query and per-result diagnostics from `search_metadata`.

**Cookbook content shape** (`docs/cookbook/query-enhancement.md`, shipped in the repo):

1. **HyDE (Hypothetical Document Embeddings)** — agent prompts an LLM to write a hypothetical answer; the answer becomes a second query. Useful when the user's question is short or jargon-light.
2. **Multi-query expansion** — agent paraphrases the question 2–3 ways (different vocabulary, broader and narrower phrasings); all variations go in `queries`. Useful when synonyms matter.
3. **Step-back prompting** — agent generates one more abstract version of the question; both go in. Useful when the user asked an over-specific question.

Each pattern documents: when to use it, an example LLM prompt, the resulting `queries` array, and a note on rate-limit cost (D25).

**MCP `search` tool description**: the description block accompanying the tool MUST end with a "Patterns" subsection listing the three named techniques with one-line examples, so an LLM reading the tool catalog discovers them without external docs.

**CLI multi-query shape** (admin debug helper):

```
mnm search "primary text query" --query "alt 1" --query "alt 2" [--limit N]
# or
mnm search --queries-stdin            # reads JSON {queries: [...]} from stdin
```

Both forms honor `--json` and emit per-query diagnostics from `search_metadata`.

<details>
<summary>Supporting Decisions</summary>

- **D3**: Caller-delegated query rewriting — `queries: string[]` input
- **D4**: Hybrid retrieval with RRF (k=60) — extended here to RRF across queries
- **D25**: Multi-query bounds (max 10, per-query rate-limit cost)

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

### User Story 10 — Distribution (Priority: P3)

**Revision**: v1.0

**As** a developer (installing the CLI/MCP server on a fresh laptop), as a Midnight Network operator (shipping the cloud server to Fly.io), and as a maintainer (running the release pipeline),
**I need** every deployable artifact to ship from one continuous-release pipeline on every commit to main — with `cargo install`, Homebrew tap, GitHub Releases, and a Docker image all built from the same SHA,
**So that** install is one command for any user (Constitution IV), versions are traceable across channels, and the team merges and releases at a sustainable cadence (Constitution IX).

**Acceptance Scenarios**:

1. **Given** the repo is at version vX.Y.Z, **When** I run `cargo install midnight-manual --version vX.Y.Z`, **Then** two binaries are installed in `$CARGO_HOME/bin/`: `midnight-manual` and `mnm`; `mnm version --json` reports `{version: "X.Y.Z", commit: "<sha>", build_date: "<date>"}`.
2. **Given** the same release, **When** I run `brew install midnight-network/tap/midnight-manual`, **Then** the same two binaries are installed under the Homebrew prefix; `mnm version` reports the same `version` and `commit` as the cargo install would.
3. **Given** the same release, **When** I download `midnight-manual-vX.Y.Z-<target>.tar.gz` from GitHub Releases, **Then** I extract a directory containing both binaries plus a `SHA256SUMS` file and a `LICENSE`; verifying `sha256sum -c SHA256SUMS` succeeds.
4. **Given** a PR is merged to `main` whose Conventional-Commit messages include any `feat:`, `fix:`, or `BREAKING CHANGE`, **When** release-please runs, **Then** it opens (or updates) a release PR containing the version bump, CHANGELOG.md additions, and manifest updates for cargo/brew.
5. **Given** the release PR is merged, **When** the release workflow fires, **Then** in one CI run: (a) a git tag is created at the merge SHA, (b) cross-compiled binaries are built for all targets in the matrix, (c) artifacts are uploaded to a GitHub Release with checksums, (d) the crate is published to crates.io, (e) the Homebrew formula in `midnight-network/homebrew-tap` is updated with the new URL and SHA, (f) a multi-arch Docker image is pushed to GHCR tagged with both the version and `:latest`, (g) the Fly.io app is deployed from the same image.
6. **Given** a target in `{linux-x86_64-gnu, linux-x86_64-musl, linux-aarch64-gnu, linux-aarch64-musl, darwin-x86_64, darwin-aarch64, windows-x86_64-msvc}`, **When** the release pipeline builds, **Then** every target produces a tarball (or zip on Windows) containing both binaries; the build matrix runs in parallel.
7. **Given** the Cargo.toml `rust-version = "1.NN"` (MSRV pin), **When** CI runs on every PR, **Then** the test matrix exercises both the MSRV toolchain and `stable`; failure on either fails the PR.
8. **Given** the released crate, **When** I run `mnm version` after any install, **Then** the version reported matches both `Cargo.toml` `package.version` and the git tag at the released SHA.
9. **Given** a contributor opens a PR breaking the MCP tool contract (Constitution I), **When** release-please generates the next version bump, **Then** the commit's `!` or `BREAKING CHANGE:` footer forces a MAJOR bump and the release PR explicitly calls out the break in the CHANGELOG.
10. **Given** the GitHub Release artifacts are published, **When** any artifact is downloaded over the next 90 days, **Then** GitHub serves the exact same bytes (immutability invariant — releases are never edited in place; corrections ship a patch release).
11. **Given** an installer (cargo, brew, GitHub binary) is used on each supported OS+arch, **When** the user runs `mnm doctor --json` immediately after install with no further setup, **Then** the report shows `cli_version` populated, `models.state = "missing"`, `mcp.installation = "not installed"`, and the command exits 0.
12. **Given** the Docker image at `ghcr.io/midnight-network/midnight-manual:vX.Y.Z`, **When** Fly.io deploys it, **Then** the container starts the `midnight-manual-server` binary; the user-facing CLI binaries are not present in the server image.

**Distribution channel matrix**:

| Channel | Artifacts | Built from | Updated by |
|---|---|---|---|
| crates.io | `midnight-manual` crate (publishes `midnight-manual` + `mnm` binaries; NOT `midnight-manual-server`) | Release tag | release pipeline |
| Homebrew tap (`midnight-network/homebrew-tap`) | Formula referencing GitHub Release tarballs (mac + linux) | Release tag | release pipeline (PR to tap repo) |
| GitHub Releases | Prebuilt `midnight-manual-vX.Y.Z-<target>.tar.gz` for every target plus `SHA256SUMS` | Release tag | release pipeline |
| GHCR (`ghcr.io/midnight-network/midnight-manual`) | Multi-arch Docker image of `midnight-manual-server` | Release tag | release pipeline |
| Fly.io | Deploy of the GHCR image | Release tag | release pipeline (deploy step) |

**Build target matrix**:

- `x86_64-unknown-linux-gnu` (glibc) and `x86_64-unknown-linux-musl` (static)
- `aarch64-unknown-linux-gnu` and `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin` and `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

(7 user-facing targets. Docker server image: `linux/amd64` + `linux/arm64`.)

**Release pipeline tooling**:

- **release-please** for Conventional-Commit-driven version bumps and CHANGELOG generation.
- **cargo-dist** for the binary cross-compile matrix, checksums, and GitHub Release upload. Fallback: hand-rolled `cargo build --target` matrix in GitHub Actions if cargo-dist proves limiting.
- **homebrew-releaser** action (or hand-rolled formula update) to push to the tap repo.
- **docker buildx** for the multi-arch server image.
- **flyctl deploy** gated on Docker image push success.

**Versioning** (Constitution X):

- MAJOR: MCP tool contract break, CLI flag break, or cloud HTTP endpoint shape break.
- MINOR: additive capabilities (new tool, new flag, new endpoint, new optional response field).
- PATCH: bug fixes, internal refactors with no contract impact.
- MSRV bumps are MINOR (Cargo ecosystem convention).
- Version stamped into the binary at build time and reported by `mnm version`.

**Signing / supply-chain (deferred)**:

- Sigstore / cosign signing of GitHub Release artifacts is documented as v1.next, not v1.
- The release pipeline still emits SHA-256 checksums for every artifact.
- `cargo-vet` or `cargo-audit` runs on every PR as a basic supply-chain guard.

<details>
<summary>Supporting Decisions</summary>

- **D16**: Two CLI binary names (midnight-manual + mnm)
- **D26**: Two distinct binaries — CLI (with MCP serve subcommand, user-facing) and server (Fly-only)

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

### User Story 11 — Observability & telemetry (Priority: P3)

**Revision**: v1.0

**As** an operator running the cloud server (debugging production), a maintainer (understanding which retrieval patterns serve users), and a privacy-conscious developer (deciding whether to trust the telemetry posture),
**I need** structured JSON logging from day one in every component, end-to-end request-id propagation, an opt-out telemetry pipeline that demonstrably never sees query content / tokens / PII, a Prometheus `/metrics` endpoint with documented series, and canary tests in CI that prove the privacy invariants,
**So that** Constitution VII goes from policy to mechanical guarantee, and the project earns the trust it asks ecosystem users to extend.

**Acceptance Scenarios**:

1. **Given** the cloud server processes any HTTP request, **When** it emits log output, **Then** every line is structured JSON with fields `{ts, level, request_id, route, status, latency_ms, tier, error_code}`; logs go to stdout; no human-formatted text mixes in.
2. **Given** the MCP server processes any tool call, **When** it emits log output, **Then** every line is structured JSON with fields `{ts, level, request_id, tool_name, latency_ms, model_state, rerank_on, result_count, error_code}`; logs go to stderr (stdout is reserved for MCP JSON-RPC).
3. **Given** the CLI runs any command, **When** it emits diagnostics, **Then** structured JSON goes to stderr; with `--json` the command's payload goes to stdout, never mixing diagnostic output into the payload stream.
4. **Given** a CLI/MCP request reaches the cloud, **When** the cloud handles it, **Then** the cloud's response carries `X-Request-Id` and every cloud log line touching that request includes the same `request_id`; the originating client's log line for that request includes the same id (allowing end-to-end correlation across components by `request_id` alone).
5. **Given** telemetry is enabled by default, **When** the MCP server processes tool calls, **Then** events are queued in memory and flushed to the cloud's `POST /v1/telemetry` every 30 seconds (configurable) or when 100 events accumulate, whichever comes first.
6. **Given** `MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1`, or `telemetry.enabled = false` in config, or after `mnm telemetry disable`, **When** any component runs, **Then** zero telemetry events are sent; pending in-memory events are discarded; the client never connects to `/v1/telemetry`.
7. **Given** an anonymous client POSTs an NDJSON batch to `/v1/telemetry`, **When** the cloud validates each event, **Then** events conforming to the per-`event_type` schema are written to `telemetry_event_raw`; events with unknown fields or unknown types are dropped and a structured warning is logged.
8. **Given** the canary test suite in CI, **When** any forbidden string (query content like `CANARY_zzz_xyz`, fabricated tokens, chunk content samples, file paths) is fed through any code path, **Then** post-run grep against every captured log file and every received telemetry event finds zero occurrences; any match fails the build.
9. **Given** the cloud server is running, **When** `GET /metrics` is hit, **Then** Prometheus exposition format is returned containing at least: `requests_total{route, status, tier}`, `request_duration_seconds_bucket{route, le}`, `source_versions_active`, `embedding_models_in_corpus`, `telemetry_events_received_total{event_type, component}`, `telemetry_events_dropped_total{reason}`, `sweep_runs_total{outcome}`.
10. **Given** the sweep job ticks, **When** it processes telemetry rows, **Then** it deletes `telemetry_event_raw` rows older than `MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS` (default 7); `telemetry_aggregate_daily` rows are unaffected.
11. **Given** the cloud's `/v1/telemetry` is unreachable from a client, **When** events accumulate beyond the in-memory buffer (default 1000), **Then** the client drops the oldest events (FIFO) and increments a local `telemetry_events_dropped` counter that is itself reported on the next successful flush.
12. **Given** the README's first-run section, **When** a user reads it, **Then** they find a paragraph naming the telemetry endpoint URL, the categories of data collected, the three opt-out mechanisms, and the retention policy — discoverable in plain language (Constitution VII).
13. **Given** `mnm telemetry status [--json]`, **When** run, **Then** it reports `{enabled, endpoint, queue_depth, last_flushed_at, last_drop_count, opt_out_resolved_from}` so an operator can verify both policy and runtime state at a glance.
14. **Given** `mnm telemetry disable`, **When** run, **Then** `telemetry.enabled = false` is written to the user config file and a structured warning records the change. `mnm telemetry enable` reverses it.

**Telemetry event schemas** (per `event_type`):

| event_type | component | fields |
|---|---|---|
| `mcp_tool_call` | mcp | tool_name (enum), latency_ms (int), result_count (int), model_state (enum), rerank_on (bool), error_code (nullable enum) |
| `cli_command` | cli | command (enum, e.g. `"sources.list"`), latency_ms, exit_code (int), error_code (nullable enum) |
| `ingest_complete` | cli | source_slug_hash (sha256 of slug), kind (md/code), files_total, chunks_total, embeds_skipped, duration_ms, outcome |
| `pull_models` | cli | model_name (enum), total_bytes, duration_ms, outcome |
| `mcp_startup` | mcp | cold_start_ms, model_state |
| `mcp_shutdown` | mcp | uptime_s, tool_calls_served, model_state_at_shutdown |

`source_slug_hash` is intentionally a hash of the slug — even source identifiers (which can carry organizational meaning) are not stored on the telemetry side.

Unknown fields cause the event to be dropped; unknown event_types are dropped; schema failures are dropped with a structured warning.

**Stored telemetry schema**:

```
telemetry_event_raw
  id            uuid PK
  received_at   timestamptz   (server-side; client-supplied timestamp is informational only)
  event_type    text
  component     text
  version       text          (component version, e.g. "1.2.3")
  fields        jsonb         (validated against per-event-type schema before insert)
  request_id    text NULL     (allows cross-component join; never user-identifying)
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
- Privacy canary tests (a short paragraph that CI enforces this)

**Forbidden in logs and telemetry** (canary set):

- Verbatim text of any user query
- Verbatim text of any returned chunk
- Bearer tokens, JWTs, API keys, signing secrets
- Filesystem paths from a user's machine
- Environment-variable values resolved at runtime (per Constitution VII — env values are excluded outright; only the resolved coarse-grained scalars in event schemas land in telemetry)
- IP addresses on event rows (IPs are used transiently for rate-limiting and never persisted with event rows)
- Email addresses or user identifiers on event rows

<details>
<summary>Supporting Decisions</summary>

- **D27**: Telemetry self-hosted on the same cloud server; 7-day raw retention; aggregate counters kept indefinitely

*Full context: `discovery/archive/DECISIONS.md`*
</details>

---

## Edge Cases

| ID | Scenario | Handling | Stories Affected |
|----|----------|----------|------------------|
| EC-01 | Empty document (zero chunks emitted after chunking) | Document row inserted with chunk count = 0; read API omits the document from chunk-returning endpoints but admins can list it. Indicates likely upstream content issue. | 1 |
| EC-02 | Document with no parent above the source root (standalone file) | parent_chain is the source_version root only; parent_node_id of the document node is the root. Read API returns parent_chain of length 1. | 1,2 |
| EC-03 | Chunk embedding fails (model error, OOM, malformed input) | Chunk row inserted with status = embed_failed and embedding IS NULL; read API excludes; ingest run reports failures in its summary. | 1,2,3 |
| EC-04 | Multiple active source_versions for the same source (data corruption) | Prevented by partial unique index 'source_version(source_id) WHERE is_active'. Attempting promotion outside a transaction that demotes the prior active raises a DB-level error. | 1 |
| EC-05 | Code file in a directory with both Cargo.toml and package.json | Package detection is language-driven by file extension: .rs uses nearest Cargo.toml; .ts/.tsx/.js/.jsx uses nearest package.json; .compact parses in-source `module <Name> { ... }`. Two packages can coexist for the same directory. | 1,3 |
| EC-06 | Compact file with multiple top-level 'module Foo {' declarations | Each chunk is tagged with the module that lexically contains it (start_byte/end_byte within the module body). A file can contribute chunks to multiple packages. | 1,3 |
| EC-07 | Markdown page with no headings | Fixed-window chunker takes over (default 800 tokens, 100-token overlap, configurable per-source). heading_path is the empty array on resulting chunks. | 1,2 |
| EC-08 | Markdown frontmatter present | Parsed into the document.frontmatter JSONB column verbatim; recognized provenance fields (verified, verified_by, etc.) are also extracted into document.provenance. | 1,2 |
| EC-09 | Unchanged document across consecutive ingests (content_hash matches) | Schema permits the ingest CLI to insert new chunk rows for the new source_version carrying the prior version's embedding bytes — no re-embed. The optimization is implemented in Story 2/3. | 1,2,3 |
| EC-10 | Embedding-model migration mid-corpus (old chunks at bge-base, new at bge-large) | Disallowed within a single source_version: every chunk in a source_version references one embedding_model_id matching the source_version row. Cross-version model differences are normal and surface to clients via the 409 mismatch protocol. | 1,4 |
| EC-11 | Sweep job running while a query reads inactive-but-not-yet-deleted chunks | Sweep is a soft delete (set retired_at) followed by a delayed hard delete after a grace window (configurable, default 24h). Active reads filter by is_active so soft-deleted rows are already excluded. | 1,9 |
| EC-12 | Document with extremely large content (multi-megabyte file) | Hard upper bound at ingest time (default 10 MB, configurable per source). Files exceeding the bound are skipped with a structured warning in the ingest report; the schema imposes no per-document size cap directly. | 1,2,3 |
| EC-13 | Manifest references a file that does not exist on disk | Pre-flight validation fails before any upload; CLI exits non-zero with a structured error listing every missing file. | 2 |
| EC-14 | Manifest claims the same file under two parents | Manifest validator rejects with a structured error naming the duplicate file path; pre-flight only — no uploads occur. | 2 |
| EC-15 | File on disk is not referenced in the manifest | Default: file is ingested with parent_chain inferred from directory ancestry. With --strict-manifest: pre-flight fails before any upload. | 2 |
| EC-16 | Markdown file with BOM, mixed line endings, or non-UTF-8 bytes | BOM is stripped silently; CRLF normalized to LF; non-UTF-8 bytes raise a per-file warning and the file is skipped unless --strict (then the run fails). | 2 |
| EC-17 | Symbolic link inside the ingest tree pointing outside the ingest root | Symlinks are followed only if their target is within the ingest root. Out-of-root targets are skipped with a warning. Cycles raise a per-link error and continue with other files. | 2 |
| EC-18 | Markdown file is empty or contains only frontmatter (no body) | Document is recorded with zero chunks; warning emitted; run does not fail. | 2 |
| EC-19 | Cloud server rejects an uploaded chunk for schema reasons (e.g. embedding dim mismatch) | In-progress source_version is left in 'building' state; CLI exits non-zero with the server's structured error attached to the run summary. The version is never promoted to active. | 2 |
| EC-20 | Two ingest runs attempt to operate on the same source concurrently | Second run's pre-flight detects an existing in-progress source_version with a different ingest_run_id and refuses unless --force-new is set. --force-new aborts the prior in-progress version first. | 2 |
| EC-21 | Auth token expired mid-upload (HTTP 401 from cloud server) | CLI attempts a single token-refresh handshake via stored credentials; on success, resumes the batch. On failure, exits non-zero with a clear remediation message pointing at 'mnm login'. | 2,9 |
| EC-22 | Chunk exceeds the embedding model's max sequence length (e.g. 512 tokens for bge-base) | Chunker re-splits the offending chunk into model-compatible sub-chunks at sentence boundaries (or hard-cut on token boundary if no sentence boundary exists), preserving heading_path. | 2,3 |
| EC-23 | Search query containing only stopwords yields zero FTS matches | FTS half returns empty; vector half still produces candidates; merged result set may be small but is returned with HTTP 200 and a 'fts_zero_matches' diagnostic flag in search_metadata. | 4 |
| EC-24 | Filter values are syntactically valid but match zero rows | Return HTTP 200 with empty results array and search_metadata.matched=0; this is not an error. | 4 |
| EC-25 | Chunk requested by id is in an inactive source_version | GET /v1/chunks/{id} returns 404 unless ?source_version_revision=N is explicitly supplied; the active-version filter is implicit on every read. | 4 |
| EC-26 | Chunk has status='embed_failed' (no vector) | The chunk is excluded from search results entirely (FTS half would still match, but the merge filters out rows whose status is not 'ready'). Direct GET /v1/chunks/{id} returns 404 to the public; admin-mode reads (future) may surface it. | 4 |
| EC-27 | Concurrent ingest finalize fires mid-query | Query runs in a Postgres REPEATABLE READ snapshot taken at request start; either the old or the new source_version is consistently visible to the entire query, never a mix. | 4,1 |
| EC-28 | Hostile or malformed filter combination produces a pathological SQL plan | Postgres statement_timeout is set to 2s by default (configurable). Timed-out requests return HTTP 504 with a typed 'query_timeout' error. Filter combinations are validated for cardinality before query construction; unbounded ANY/OR over array fields requires GIN indexes. | 4,9 |
| EC-29 | Search vector field is missing or vector dim does not match the active corpus model | Returns HTTP 400 with code='invalid_request' when client_embedding_model does match the corpus model (a dim mismatch under matching model is a client bug). Returns HTTP 409 'embedding_model_mismatch' when client_embedding_model does not match the corpus model (the expected migration case). | 4 |
| EC-30 | Request body exceeds the documented max size (e.g. someone POSTs a megabyte query.text) | Reject at the HTTP ingress layer with HTTP 413 'payload_too_large' and a typed body naming the limit. Default max body size 256 KB; configurable per deployment. | 4,9 |
| EC-31 | Cross-origin (CORS) request from a browser-based ecosystem reader | CORS allow: GET on read-safe endpoints from any origin (Access-Control-Allow-Origin: *); POST /v1/search same. Preflight OPTIONS returns 200 with documented Allow-Headers including Authorization, X-Request-Id. Write endpoints (Story 9) do NOT permit wildcard origins. | 4,9 |
| EC-32 | Limit parameter requested above the documented maximum | Server caps silently at the documented max (default 100) and returns search_metadata.limit_capped=true in the response. | 4 |
| EC-33 | Source_version_revision query parameter references a soft-deleted version | Returns HTTP 410 'gone' with a typed body indicating the version has been retired and naming the most recent surviving revision. | 4 |
| EC-34 | Two concurrent search calls during initial model load | Both calls await the single in-flight model-load future (tokio::sync::OnceCell or equivalent); the second call does not start a duplicate download or duplicate ONNX session initialization. | 5 |
| EC-35 | Model files exist on disk but are corrupt (truncated download, bit-flip) | ONNX session creation fails on first use; the server returns a typed 'models_corrupt' error pointing at pull_models; pull_models verifies the file digest against the manifest before swapping in the new model file. | 5,8 |
| EC-36 | AI client kills the MCP server subprocess mid-request (SIGTERM or pipe close) | Server traps shutdown signals; cancels in-flight HTTP requests to the cloud; flushes any buffered telemetry; exits with code 0 within 1 second. Pending tool responses are not emitted; the client must reconnect to issue new tool calls. | 5 |
| EC-37 | Cloud returns HTTP 429 rate_limited | MCP server forwards the cloud's Retry-After header to the agent inside the typed MCP error body; the agent decides whether to back off or surface to the user. The server itself does not transparently retry to avoid amplifying load. | 5 |
| EC-38 | Config file missing or contains schema errors at startup | Server falls back to defaults for missing optional values; fails with a clear stderr message and exit code 78 (config error) for unrecoverable schema errors. The MCP handshake never half-succeeds with a broken config. | 5 |
| EC-39 | Config supplies no bearer token (anonymous mode) | Server starts in anonymous mode; status tool reports rate_limit_tier='anonymous'; all retrieval tools work but are subject to the lower per-IP limit (D11). No warning at startup — anonymous is a first-class supported mode. | 5 |
| EC-40 | search called with both query and queries fields populated | Server rejects with invalid_request error; the schema oneOf constraint is enforced at the MCP boundary, not deferred to the cloud. | 5 |
| EC-41 | search called with an empty query string or empty queries array | Server rejects with invalid_request before contacting the cloud or loading models. | 5 |
| EC-42 | Local reranker model load succeeds but embedding model load fails | First retrieval returns models_corrupt with the offending model name; subsequent calls re-check the loaded set so a successful pull_models recovery works without server restart. | 5,8 |
| EC-43 | Cloud /v1/models/active returns a different model than the locally loaded one (drift detected after launch) | Status tool reports model_state='stale'; retrieval tools return embedding_model_mismatch error referencing pull_models; the server does not auto-pull (D12 — explicit consent for downloads). | 5,12 |
| EC-44 | Mixed-language repo (Rust + TypeScript + Compact in one tree) | Each file resolves to its language by extension; package detection runs per-language; the resulting source_version contains one package row per detected (kind, name) pair. | 3 |
| EC-45 | Cargo workspace virtual root (Cargo.toml with [workspace] but no [package]) | Virtual root is detected and excluded from package resolution; .rs files walk past it to their nearest member Cargo.toml. If a member declares no [package], the file is recorded with package = null and a warning. | 3 |
| EC-46 | package.json without a 'name' field | Package name falls back to the manifest's directory name; a warning is emitted naming the manifest path. Setting --strict-package promotes the warning to an error. | 3 |
| EC-47 | File outside any package manifest (e.g. a top-level script in the repo root) | Chunks from such files carry package = null. The file is still chunked, embedded, and indexed normally. | 3 |
| EC-48 | Git submodule directories inside the ingest path | Skipped by default; --include-submodules enables descent into them. Submodule traversal does not pull a fresh copy from the submodule's remote — it ingests whatever is checked out on disk. | 3 |
| EC-49 | Private git repo requested via --git without available credentials | Clone fails with a clear error naming the URL and the credential discovery order (ssh-agent, then GIT_USERNAME/GIT_PASSWORD env, then ~/.netrc). No partial ingest occurs. | 3,9 |
| EC-50 | Git --ref does not exist in the cloned repo | Clone is attempted with the ref; on failure to check out, the CLI exits non-zero with a structured error naming the ref and the available branch/tag list (truncated). Temp clone directory is removed. | 3 |
| EC-51 | File contains both Compact module declarations and code outside any module | In-module byte ranges are tagged with their enclosing module; out-of-module chunks carry package = null. Both are indexed; nothing is dropped. | 3 |
| EC-52 | Very large generated code file (megabytes, e.g. a bundled JS or auto-generated FFI binding) | Files exceeding --max-file-size (default 10MB) are skipped with a warning. Common generated patterns (e.g. *.min.js, *.bundle.js, *_pb.ts) are also skipped via default exclude patterns. | 3 |
| EC-53 | Shebang-only file with no extension (e.g. a script invoked as ./run) | Language detected by shebang: '#!/usr/bin/env node' -> javascript, '#!/usr/bin/env python3' -> other (line-window). No shebang -> other. | 3 |
| EC-54 | Symlink inside the cloned/ingested repo pointing outside the ingest root | Symlinks outside the ingest root are not followed; a per-link warning is emitted. Symlink cycles inside the ingest root are detected and broken at first revisit. | 3 |
| EC-55 | Challenge nonce is replayed (admin signs the same nonce twice) | Nonces are single-use, recorded by server with a short TTL (60s default). Reusing a nonce returns 401 with code='nonce_consumed' or 'nonce_expired'. | 9 |
| EC-56 | User store TOML fails to parse at server startup | Server exits non-zero before binding the HTTP port; the error logged to stderr names the offending line. No partial-config running state is possible (Constitution VI fail-fast). | 9 |
| EC-57 | Two admin clients race the same ingest_run finalize | Finalize is implemented as an atomic update with WHERE source_version.is_active = false AND status = 'building'. The second finalize finds status='active' or 'aborted' and returns 409 'run_already_finalized'. | 9 |
| EC-58 | GitHub OAuth callback returns a user not in the configured org | Server returns 403 'forbidden' with remediation 'Request membership in the Midnight GitHub org or contact a maintainer'; no token is minted; no row is created in the rate-limit tracker. | 9 |
| EC-59 | Postgres pgvector extension is not available at server startup | Server fails the readiness check during migration ('CREATE EXTENSION IF NOT EXISTS vector' fails) and exits non-zero with a clear remediation message naming the extension and the supported Postgres flavors. /healthz returns 200 only briefly before exit. | 9 |
| EC-60 | An ingest_run is created but never finalized (CLI process killed before finalize) | Sweep job marks runs whose updated_at is older than MIDNIGHT_MANUAL_ABORT_GRACE (default 1h) as 'aborted' and proceeds to garbage-collect their rows. The same user_id may start a new run; --force-new from the CLI explicitly aborts any prior in-progress run for that user. | 9,2,3 |
| EC-61 | An admin tries to call /v1/admin/ratelimits with a writer-role token (not admin role) | Server returns 403 'forbidden' with remediation 'Your role 'writer' lacks permission for this endpoint. Required role: admin'. | 9 |
| EC-62 | CIDR override expires while a request is in flight | Rate-limit decision is taken once per request, at request start. If an override expires mid-request, the in-flight request completes under the override; the next request gets the default tier. | 9 |
| EC-63 | Two overlapping CIDR override rows match the same client IP | Server selects the most specific (longest prefix) match. Ties (same prefix length) are resolved by most-recent created_at, with a startup warning logged that overlapping CIDRs exist. | 9 |
| EC-64 | Sweep job encounters a foreign-key constraint preventing deletion | Sweep transaction is rolled back; the source_version is left in place; a structured warning is logged with the FK-violating row id. The sweep retries on its next tick. Operator can manually inspect via 'mnm versions show'. | 9 |
| EC-65 | Server starts up but DATABASE_URL points at a Postgres without the schema yet (fresh deploy) | Migrations run as part of startup (D22); if auto-migrate is disabled, the server fails readiness with a 'schema_missing' error pointing at 'mnm db migrate'. | 9 |
| EC-66 | Admin runs 'mnm users add' but the local user-store TOML doesn't exist yet | On first add (with no existing file), the CLI initializes a fresh user-store TOML with schema_version, writes the user, and warns 'created new user store at <path>; commit and deploy'. | 8 |
| EC-67 | Admin runs 'mnm users remove <user_id>' on the last admin | The CLI refuses with a clear error: 'removing the last user with role=admin would lock everyone out; pass --confirm-lockout to proceed anyway'. Forcing the action is supported but explicit. | 8 |
| EC-68 | mnm keys generate finds an existing keypair for the user_id | Refuses to overwrite by default; --force flag overrides; on overwrite a backup of the prior public+private files is written with a timestamp suffix. | 8 |
| EC-69 | auth.toml exists but is malformed, world-readable, or has an unknown schema_version | CLI refuses to read tokens from the file; emits a structured error naming the file path and the specific problem (parse error / chmod / schema). Remediation: 'fix file permissions (chmod 0600), rotate tokens via mnm login or mnm auth github'. The MCP server in the same state starts anonymous and reports the issue via the status tool (D28). | 8,5 |
| EC-70 | mnm mcp install --agent <unknown-agent> | The CLI prints the JSON snippet to stdout with instructions: 'agent X is not on the known list; paste this snippet into your client config: ...'. Known agents (claude-code, cursor, continue, ...) have known config paths that the CLI edits directly. | 8 |
| EC-71 | mnm versions rollback when only one source_version exists | Exits non-zero with a typed error: 'no prior version exists to roll back to; this is the only active source_version'. | 8 |
| EC-72 | Admin's local JWT is expired and they invoke a write command | The CLI detects 401 from the server, prints 'your token has expired; run mnm login' (does NOT auto-renew silently), and exits non-zero. Auto-renewal is rejected explicitly because it would mask credential lifecycle issues from the user. | 8 |
| EC-73 | mnm models prune asked to remove the currently-loaded model | If the MCP server is currently using the model (PID file present in the cache dir), the CLI refuses with a clear error pointing at how to stop the MCP server first, or --force to remove anyway. | 8 |
| EC-74 | Config file 'cli.show_admin_cmds = true' but env MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=0 | Env wins per D18 — admin commands are hidden from help. The --effective flag on 'mnm config show' makes this resolution visible to the operator. | 8 |
| EC-75 | User-store TOML on disk has schema_version higher than the CLI supports | CLI refuses any mutating 'mnm users' operation; suggests upgrading the CLI. 'mnm users list' still works in read-only mode. | 8 |
| EC-76 | mnm doctor on a machine with no config file at all | Doctor still runs to completion using built-in defaults; the report flags 'no config file found' as a yellow status (not red). Suggests 'mnm config set ...' to create one. | 8 |
| EC-77 | Chunk's document has source_modified_at = NULL | Freshness age falls back to source_version.ingested_at per scoring policy's fallback_age_source setting. The confidence_factors response records 'age_source' so the consumer knows which timestamp was used. | 6 |
| EC-78 | Chunk has all-empty / unknown provenance (attribution='unknown', no verification, no language_targets) | trust_score is computed against the policy's 'unknown' attribution multiplier (default 0.30) with no version-match boost; the chunk still appears in results, just with a low trust_score that the consumer can reason about. | 6 |
| EC-79 | Multiple language_targets where the query satisfies some but not all | Treated as 'satisfies' (the chunk is applicable to the requested version, even if it also covers other versions). Strict mode (a future scoring policy flag) could change this; the default permits over-coverage. | 6 |
| EC-80 | Deprecated AND very fresh content | Deprecation multiplier dominates: trust_score is reduced regardless of freshness. The confidence_factors makes this visible so a consumer can choose to show 'recently deprecated' callouts. | 6 |
| EC-81 | Scoring policy TOML has an unknown key (e.g. typo) | Server startup fails with a structured error naming the offending key and the policy file path. Defaults are not silently used in this case — operators expect explicit feedback when a policy doesn't load (Constitution VIII). | 6,9 |
| EC-82 | Caller requests min_confidence above the highest result's confidence | Returns 200 with empty results array and search_metadata.filtered_by_confidence equal to the total candidates considered. Not an error — informing the caller they may need to broaden the query. | 6 |
| EC-83 | Scoring policy multiplier produces confidence > 1.0 (e.g. version_match boost stacks beyond the cap) | Value is clamped to 1.0 and a structured warning is logged with the offending input vector. Repeated occurrences over a threshold (e.g. >1% of recent results) escalate to an alert (Story 11). | 6,11 |
| EC-84 | MCP rerank=true but reranker is in 'models_missing' or 'models_stale' state | MCP server passes the cloud's confidence through unchanged and sets confidence_factors.relevance_source = 'rrf'; a warning event is emitted to the MCP client telemetry channel suggesting pull_models. | 6,5 |
| EC-85 | sort_by and min_confidence both supplied with conflicting effect | Both honored: min_confidence filters first; sort_by orders the survivors. The orthogonal semantics make the combination meaningful (e.g. 'top by relevance among results above 0.7 confidence'). | 6 |
| EC-86 | Identical inputs produce different confidence across two runs | Must not happen. Scoring is deterministic given (policy, query, chunk inputs). A test invariant enforces reproducibility: same inputs always yield identical floats. Bit-flip-grade non-determinism is a programmer error (Constitution VI). | 6 |
| EC-87 | Request contains queries.length = 0 OR every queries[i].text is empty | Validated as 400 invalid_request before any retrieval work; the error message names the offending field and accepted shape. | 7 |
| EC-88 | Request contains queries.length > MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST | 400 invalid_request before any work or rate-limit consumption; remediation message names the configured cap and the hard ceiling (50). | 7 |
| EC-89 | Single query is sent via the convenience 'query' field while 'queries' is also present | Returns 400 invalid_request: 'query and queries are mutually exclusive'. The shape is validated before retrieval; callers pick one form. | 7 |
| EC-90 | Two of the input queries are identical (same text + same vector) | Server detects duplicate queries by content hash, deduplicates internally before retrieval (so duplicates do not double-charge rate-limit tokens beyond max(1, distinct.length)), and notes deduplicated_count in search_metadata. | 7 |
| EC-91 | One query in a multi-query batch has a malformed vector (wrong dimension) | Whole request is rejected with 400 invalid_request naming the offending query index and field; partial-batch retrieval is intentionally not supported because it would muddy benchmark and confidence interpretation. | 7 |
| EC-92 | Caller's rate-limit budget is insufficient for the multi-query cost | Returns 429 rate_limited with X-RateLimit-Remaining showing the current balance and the per-query cost the request would incur; remediation suggests reducing queries.length or upgrading the rate-limit tier. | 7 |
| EC-93 | A multi-query batch where only one query matches anything | The merged candidate set still returns the matches; search_metadata.per_query shows zero candidates for the queries that returned nothing; scores.matched_queries reflects which queries contributed. | 7 |
| EC-94 | Caller passes 'queries' to a future endpoint that doesn't accept multi-query | Endpoint-specific validation: the multi-query shape is only accepted by /v1/search at MVP. Other endpoints reject with 400 'unsupported_field' to keep contracts narrow. | 7 |
| EC-95 | A release-pipeline step fails after some artifacts have already been published (e.g. crates.io publish succeeds but Homebrew formula update fails) | Pipeline does not auto-rollback published artifacts. The maintainer resolves manually: either fix forward (next patch release re-syncs the Homebrew formula) or ship a patch immediately. Pipeline is designed to make the rare double-publish recoverable rather than impossible. | 10 |
| EC-96 | A cross-compile target fails (e.g. linker error on musl) | That target's job fails the release workflow; the release does NOT publish for any channel until the failing target is fixed. Yanking partial cargo publishes is not automated; the rule is 'release atomically or not at all'. | 10 |
| EC-97 | Conventional Commit lint fails on a PR | PR is blocked from merge with a clear message naming the malformed commit and the expected format. The lint runs locally via a pre-commit hook (offered, not enforced) and in CI on every PR. | 10 |
| EC-98 | A user installed via cargo from a stale crates.io snapshot and reports a bug already fixed on main | mnm doctor reports the installed version; the doctor checks an environment variable (default disabled) for a 'latest stable' hint and politely suggests upgrading. No silent telemetry pings from doctor; the upgrade-check is explicit. | 10,8 |
| EC-99 | Homebrew formula points at a GitHub Release URL but the asset is renamed/deleted | Formula installs fail with a clear curl error. Recovery: re-issue the patch release with the correct asset name; tap repo is updated. GitHub Release assets are never renamed (immutability invariant; if rename is unavoidable, ship a new release). | 10 |
| EC-100 | A vX.Y.Z release is required to be yanked from crates.io due to a security or correctness issue | Maintainer yanks the crate via cargo yank. The GitHub Release page and CHANGELOG are amended (this is one of the rare cases where the release is edited in place — explicitly to add a deprecation notice — but the artifacts remain immutable). A patch release ships the fix. | 10 |
| EC-101 | Build matrix is slow enough to delay releases meaningfully (>30 min) | Release pipeline has a 60-minute soft cap; exceeding it triggers an alert. Maintainers can opt-in to caching strategies (sccache, cargo-chef, GitHub Actions cache) or trim the target matrix temporarily; trimmed targets MUST be re-enabled before the next minor release. | 10 |
| EC-102 | Two PRs both merge in rapid succession, each warranting a release | release-please coalesces into one release PR covering both commits. If the release PR is already open and pending, the second merge appends its changelog entry. A maintainer never has to manually deconflict version bumps. | 10 |
| EC-103 | Clock skew between client and cloud server | Server uses its own receipt time as authoritative for received_at; client's timestamp is stored as info-only in fields. No reliance on client time for retention or ordering. | 11 |
| EC-104 | Client opts out mid-session while events are pending in the buffer | The component flushes nothing further; in-memory queue is discarded immediately. The opt-out is observable within one tool call; pending events from before the opt-out are NOT sent because the user's intent has changed mid-session. | 11 |
| EC-105 | Telemetry batch arrives with one valid and one invalid event | Server processes valid events and drops invalid ones with structured warnings naming the offending index and field; the request returns 200 with a body reporting (accepted, rejected). Partial acceptance prevents one bad client event from blocking the rest. | 11 |
| EC-106 | Telemetry endpoint returns 503 sustained for hours | Client retries with exponential backoff (max 30s); when in-memory buffer overflows, FIFO drops oldest. Drop counter is reported on next successful flush. The system degrades gracefully (Constitution VI) — no tool call ever waits on telemetry. | 11 |
| EC-107 | Log volume grows large enough to impact Fly.io machine I/O | Log level defaults to 'info' in production; 'debug' is opt-in via MIDNIGHT_MANUAL_LOG_LEVEL. Fly captures stdout but rate-limits high-volume logs at the platform layer; the server does not implement its own rotation since logs are not persisted to disk in the container. | 11,9 |
| EC-108 | A canary string accidentally appears in a developer's own log message during local testing | Canary tests run only in CI's controlled environment using known canary strings unlikely to collide with normal usage (e.g. CANARY_zzz_xyz_<random_uuid>). Local development uses different canary strings; collision in production is statistically impossible. | 11 |
| EC-109 | Operator wants to debug a specific user's session and asks for query content from logs | Refused. The codified position is: query content is never logged. If a user needs help debugging, the protocol is for the user to share their own query and the server's request_id — server-side correlation by request_id is sufficient to find the relevant log lines without ever logging the query itself. | 11 |
| EC-110 | /metrics scraped by an unauthenticated party | Metrics are intentionally public — they expose only aggregates without user-identifying content (Constitution VII). No authentication required at /metrics. Per-user or sensitive operational metrics, if added later, MUST live behind admin auth. | 11 |

---

## Requirements

### Functional Requirements

| ID | Requirement | Stories | Confidence |
|----|-------------|---------|------------|
| FR-001 | System MUST store each chunk with a parent_chain reachable from the chunk's node up to the source_version root via the node table. | 1 | 🔵 Confirmed |
| FR-002 | System MUST record the embedding_model_id used to compute each chunk's vector, matching the source_version's embedding_model_id. | 1 | 🔵 Confirmed |
| FR-003 | System MUST enforce 'at most one active source_version per source' via a partial unique index. | 1 | 🔵 Confirmed |
| FR-004 | System MUST store provenance metadata (attribution, verified, verified_by, verified_at, language_targets, sdk_dependencies, deprecation, tags, content_type) at the document level; chunks inherit at read time. | 1,6 | 🔵 Confirmed |
| FR-005 | System MUST store source_url and published_url at the document level (nullable); the read API returns both with every chunk that originates from that document. | 1,2,5 | 🔵 Confirmed |
| FR-006 | System MUST detect package membership for code chunks: Rust via nearest Cargo.toml [package].name; TypeScript/JavaScript via nearest package.json .name; Compact via in-source top-level 'module <Name> {' declaration; otherwise no package. | 1,3 | 🔵 Confirmed |
| FR-007 | System MUST chunk Markdown on heading boundaries when headings exist; fall back to a fixed-window chunker (default 800 tokens, 100-token overlap, both configurable) when none exist. | 1,2 | 🔵 Confirmed |
| FR-008 | System MUST chunk source code via tree-sitter for known languages (Rust, TypeScript/TSX, JavaScript/JSX, Compact when grammar available) and fall back to a line-window chunker for unknown languages (default 60 lines, 20-line overlap). | 1,3 | 🔵 Confirmed |
| FR-009 | System MUST record start_byte and end_byte on each chunk so the original source range can be reconstructed exactly. | 1,3 | 🔵 Confirmed |
| FR-010 | System MUST denormalize total_chunks and chunk_index onto every chunk row so the read API can return 'chunk N of M' without an aggregate query. | 1,4 | 🔵 Confirmed |
| FR-011 | System MUST provide GIN index on chunk.tsvector for FTS and HNSW index on chunk.embedding for ANN; reads issue FTS and ANN queries in parallel and merge results via RRF (k=60) in the application layer. | 1,4 | 🔵 Confirmed |
| FR-012 | System MUST permit a chunk row with embedding IS NULL and status = 'embed_failed'; the read API MUST exclude such rows by default. | 1,2,3 | 🔵 Confirmed |
| FR-013 | System MUST retain N most recent source_versions per source (N defaults to 5, overridable per source); older versions are soft-deleted via retired_at then hard-deleted after a configurable grace window. | 1,8,9 | 🔵 Confirmed |
| FR-014 | System MUST permit cross-version embedding-model differences and surface them to clients via a typed 409 'embedding_model_mismatch' response when the client-supplied model id does not match the active corpus model. | 1,4,5 | 🔵 Confirmed |
| FR-015 | System MUST permit chunk reuse across source_versions when document content_hash is unchanged (chunks inserted with previously-computed embedding bytes); the implementation of reuse lives in Story 2/3. | 1,2,3 | 🔵 Confirmed |
| FR-016 | mnm ingest md MUST chunk Markdown on heading boundaries when headings exist, falling back to fixed-window chunking (default 800 tokens, 100-token overlap) when none exist. | 2 | 🔵 Confirmed |
| FR-017 | The CLI MUST compute deterministic document.content_hash over normalized content (UTF-8 LF, trailing whitespace stripped per line, frontmatter excluded from the body hash but recorded separately). | 2 | 🔵 Confirmed |
| FR-018 | The CLI MUST upload chunks in batches; partial batch failures MUST NOT corrupt the in-progress source_version, and an interrupted run MUST be resumable in-place by default and replaceable via --force-new. | 2 | 🔵 Confirmed |
| FR-019 | The CLI MUST authenticate to the cloud server using the appropriate token type per command (admin JWT for writes via --admin-token / MIDNIGHT_MANUAL_ADMIN_TOKEN / auth.toml[admin]; read-uplift bearer for reads via --token / MIDNIGHT_MANUAL_TOKEN / auth.toml[read_uplift]). MUST NOT log tokens at any level. Write commands MUST exit non-zero before any work when no valid admin token is available; read commands fall through to anonymous (D28). | 2,3,8 | 🔵 Confirmed |
| FR-020 | With --dry-run the CLI MUST NOT call any cloud-server write endpoint; read endpoints (e.g. fetching the source's current model) are permitted. | 2,3 | 🔵 Confirmed |
| FR-021 | With --json the CLI MUST emit only NDJSON on stdout (one JSON object per line); human-readable diagnostics go to stderr only; the last record MUST be a summary object with shape {"type":"summary", "result": "ok|partial|error", "stats": {...}, "errors": [...]}. | 2,3,8 | 🔵 Confirmed |
| FR-022 | Ingest MUST resolve the embedding model in precedence order: --embedding-model flag, then the source's current active model, then the project default from D14; mismatches between local model availability and the resolved model MUST fail before any embedding work begins. | 2,3 | 🔵 Confirmed |
| FR-023 | The CLI MUST refuse to operate on a source slug that does not already exist on the cloud server; source creation requires an explicit 'mnm sources add' command. | 2,3 | 🔵 Confirmed |
| FR-024 | The CLI MUST emit progress events at granularity of one record per file in --json mode and one summary line per batch in human mode; the final stdout record is always the summary. | 2,3 | 🔵 Confirmed |
| FR-025 | Manifest validation MUST occur before any upload and MUST detect: missing-file references, duplicate-parent assignments, cycles, and unknown schema fields; validation failure exits non-zero with a structured error per offending entry. | 2 | 🔵 Confirmed |
| FR-026 | POST /v1/search MUST run FTS and pgvector ANN as parallel queries and MUST merge their results in application code via Reciprocal Rank Fusion with k=60 (D4). Raw FTS rank and vector distance MUST also be returned per result when include_scores=true. | 4 | 🔵 Confirmed |
| FR-027 | All read endpoints MUST implicitly filter to source_version.is_active. Explicit historical-version access MUST require an opt-in query parameter (source_version_revision). | 4 | 🔵 Confirmed |
| FR-028 | Every read endpoint MUST validate the client_embedding_model field (when supplied or required) against the active corpus model and MUST respond with HTTP 409 and a typed embedding_model_mismatch body on mismatch (D12). | 4 | 🔵 Confirmed |
| FR-029 | Every response MUST include X-Request-Id (stable per request), X-RateLimit-Limit, X-RateLimit-Remaining, and X-RateLimit-Reset headers; the request id MUST appear in every server log line emitted while handling that request. | 4,11 | 🔵 Confirmed |
| FR-030 | All error responses (4xx and 5xx) MUST use the typed envelope {error: {code, message, remediation, context}, request_id}; the error code MUST come from the documented enum. | 4 | 🔵 Confirmed |
| FR-031 | Rate-limit decisions MUST consult CIDR overrides first (D11), then the per-user (SSO bearer) tier, then the anonymous per-IP tier. The chosen tier and limit MUST be reported via response headers. | 4,9 | 🔵 Confirmed |
| FR-032 | Every search response MUST include parent_chain from the chunk's node up to the source_version root and navigation (prev_chunk_id, next_chunk_id) for in-document traversal. | 4 | 🔵 Confirmed |
| FR-033 | Filters MUST combine as logical AND across keys and logical OR within each key's array of values. language_target.version_constraint_satisfies and sdk_dependency.version_constraint_satisfies MUST evaluate semver constraints server-side. | 4,6 | 🔵 Confirmed |
| FR-034 | The server MUST never log secrets, bearer tokens, query content, or PII (Constitution VII). It MUST log structured fields: request_id, route, status, latency_ms, tier, source_count, rate_limit_decision. | 4,11 | 🔵 Confirmed |
| FR-035 | All transient cloud-store failures (Postgres unavailable, timeout) MUST return HTTP 503 with a Retry-After header; the server MUST NOT panic or crash the process on these conditions (Constitution VI). | 4,9 | 🔵 Confirmed |
| FR-036 | The MCP server MUST implement the MCP protocol over stdio transport using JSON-RPC framing. | 5 | 🔵 Confirmed |
| FR-037 | MCP tool names, input schemas, and result schemas are stable public APIs; breaking changes MUST require a major version bump per Constitution I. | 5,10 | 🔵 Confirmed |
| FR-038 | The MCP server MUST lazily load embedding and reranker models on first retrieval call, guarded so concurrent first-callers share one load. Handshake MUST complete within 500ms cold start (Constitution IV) regardless of model state. | 5 | 🔵 Confirmed |
| FR-039 | Every cloud read request issued by the MCP server MUST include the client_embedding_model header/field (D12). Cloud-returned 409 embedding_model_mismatch responses MUST be translated into typed MCP errors referencing the pull_models tool. | 5,4,12 | 🔵 Confirmed |
| FR-040 | The MCP server MUST run cross-encoder reranking on cloud-returned candidates by default (D2); the search tool MUST accept rerank=false to disable for ultra-low-latency callers. | 5,6 | 🔵 Confirmed |
| FR-041 | The MCP server MUST NOT log query content, bearer tokens, chunk content, or any user-controlled string at any log level. Permitted structured log fields: tool_name, latency_ms, result_count, model_state, rerank_on, error_code. | 5,11 | 🔵 Confirmed |
| FR-042 | The MCP server MUST surface model-state issues (missing/stale/corrupt models) as typed MCP tool errors with remediation pointing at pull_models. The handshake itself MUST still succeed in these states so an agent can inspect and self-heal. | 5,12 | 🔵 Confirmed |
| FR-043 | The MCP server MUST accept --token <jwt> and --config <path> flags; MUST honor MIDNIGHT_MANUAL_TOKEN (read-uplift bearer only) and MIDNIGHT_MANUAL_CONFIG. Token resolution order: flag, then env, then auth.toml[read_uplift].token, then anonymous. The MCP server MUST NOT read auth.toml[admin] (D28). Config discovery follows D18: $XDG_CONFIG_HOME/midnight-manual/config.toml. | 5 | 🔵 Confirmed |
| FR-044 | pull_models MUST verify downloaded model files against expected digests before installation; corrupt downloads MUST be discarded and reported with the offending file name. | 5,8 | 🔵 Confirmed |
| FR-045 | The MCP server MUST handle graceful shutdown (SIGTERM, pipe close) by cancelling in-flight cloud requests, flushing telemetry, and exiting within 1 second; it MUST NOT leave orphaned HTTP connections or partially-emitted MCP responses. | 5 | 🔵 Confirmed |
| FR-046 | mnm ingest code MUST chunk via tree-sitter for Rust (.rs), TypeScript (.ts, .tsx), and JavaScript (.js, .jsx, .mjs, .cjs); chunks MUST carry symbol_path reflecting the AST nesting that contains them. | 3 | 🔵 Confirmed |
| FR-047 | Compact (.compact) chunking MUST identify top-level 'module <Name> { ... }' blocks at MVP without a full tree-sitter grammar; chunks MUST be tagged with the enclosing module name; inside-module chunking MAY use line-window until a grammar is available — IMPLEMENTED via the compactp parser (rowan CST), superseding the hand-rolled scanner; single top-level module → package (P1); per-chunk multi-module tagging deferred. | 3 | 🔵 Confirmed |
| FR-048 | Files whose extension maps to no known grammar MUST fall back to a line-window chunker (default 60 lines, 20-line overlap, both configurable via --code-chunk-lines / --code-chunk-overlap). | 3 | 🔵 Confirmed |
| FR-049 | Tree-sitter parser errors on any file MUST trigger line-window fallback for that file and emit a per-file warning; a parser error MUST NOT fail the run unless --strict is set. | 3 | 🔵 Confirmed |
| FR-050 | Package detection: Rust resolves to nearest Cargo.toml with [package]; TS/JS to nearest package.json with a name; Compact to in-source module declarations; other languages produce package = null. Workspace virtual roots are excluded. | 3,1 | 🔵 Confirmed |
| FR-051 | Default exclusions MUST include node_modules, target, vendor, dist, build, out, coverage, .git, common lockfiles, and known generated-file patterns. --include and --exclude globs MUST compose on top of defaults. | 3 | 🔵 Confirmed |
| FR-052 | .gitignore MUST be respected by default (recursively across the ingest tree); --no-respect-gitignore disables. | 3 | 🔵 Confirmed |
| FR-053 | Binary files (detected via magic-number sniff) MUST be skipped with a warning regardless of extension or .gitignore status. | 3 | 🔵 Confirmed |
| FR-054 | --git <url> [--ref <ref>] MUST clone the repo into a temp directory, ingest from that directory, and remove the temp directory on exit (success or failure). Auth credentials MUST come from ssh-agent or env vars only — never persisted in any local config. | 3,9 | 🔵 Confirmed |
| FR-055 | Each chunk MUST carry start_byte and end_byte into the original source file (re-stating Story 1 FR-009 in this story's context); reconstruction of the original file from its chunks MUST be exact modulo whitespace normalization. | 3,1 | 🔵 Confirmed |
| FR-056 | Admin auth MUST implement Ed25519 challenge-response: POST /v1/auth/challenge returns a single-use nonce with TTL <= 60s; POST /v1/auth/verify validates the signature against the user store and returns an HS256 JWT with 1-hour TTL. | 9 | 🔵 Confirmed |
| FR-057 | The user store TOML MUST be loaded once at startup from MIDNIGHT_MANUAL_USER_STORE and held in memory; the server MUST NOT mutate it at runtime (D20). Unknown fields and schema_version mismatch MUST fail startup. | 9 | 🔵 Confirmed |
| FR-058 | JWTs MUST be HS256-signed by the secret in MIDNIGHT_MANUAL_JWT_SECRET, carry claims {sub, iat, exp, role, jti}, and have a TTL of 1 hour (D21). Verification MUST reject expired tokens, wrong-signature tokens, and tokens lacking required claims. | 9 | 🔵 Confirmed |
| FR-059 | Write endpoints MUST require an admin JWT with role >= writer for ingest operations and role = admin for /v1/admin/* operations; role checks MUST be enforced at the route layer before any DB write occurs. | 9 | 🔵 Confirmed |
| FR-060 | PUT /v1/sources/{slug}/ingest-runs/{id}/documents MUST be idempotent on (document.content_hash, chunk.content_hash); replays of identical content MUST return 200 with conflicts:[] and zero rows inserted. | 9,2,3 | 🔵 Confirmed |
| FR-061 | Finalize MUST be atomic: a single Postgres transaction sets the new source_version is_active=true AND demotes the prior active version. The partial unique index 'source_version(source_id) WHERE is_active' MUST guarantee at most one active version per source. | 9,1 | 🔵 Confirmed |
| FR-062 | GitHub OAuth flow MUST verify the authenticated GitHub user is a member of the configured org (MIDNIGHT_MANUAL_GITHUB_ORG); non-members MUST receive 403 and no token. Tokens minted via this flow grant only read-uplift rate-limit tier — never write permissions. | 9 | 🔵 Confirmed |
| FR-063 | Sweep job MUST run every 5 minutes (configurable via MIDNIGHT_MANUAL_SWEEP_INTERVAL) and MUST delete inactive source_versions older than MIDNIGHT_MANUAL_SWEEP_GRACE (default 24h) plus aborted ingest_runs older than MIDNIGHT_MANUAL_ABORT_GRACE (default 1h). Each sweep cycle MUST be transactional per source_version. | 9 | 🔵 Confirmed |
| FR-064 | Migrations MUST run automatically at startup by default; MIDNIGHT_MANUAL_AUTO_MIGRATE=false defers migration to an explicit 'mnm db migrate' preflight (D22). Migration failure MUST exit the process non-zero with the failing migration name in the error message. | 9,8 | 🔵 Confirmed |
| FR-065 | /healthz MUST return 200 whenever the process is alive (it MUST NOT depend on DB or external services). /readyz MUST return 200 only when (a) the DB is reachable within 1s, (b) the user store is loaded, (c) at least one embedding_model row exists, and (d) the most recent successful DB query was within 60s. | 9,11 | 🔵 Confirmed |
| FR-066 | Admin commands MUST be hidden from --help output by default; visibility is enabled by MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS env or cli.show_admin_cmds in config (env wins). Visibility MUST NOT gate invocation — hidden commands run normally when invoked by name (D23). | 8 | 🔵 Confirmed |
| FR-067 | mnm keys generate MUST produce an Ed25519 keypair; the private half MUST be written with 0600 permissions and never echoed to stdout or any log; the public half MUST be echoed in the TOML row format ready to paste into the user-store. | 8 | 🔵 Confirmed |
| FR-068 | mnm login MUST persist the issued admin JWT to $XDG_CONFIG_HOME/midnight-manual/auth.toml under the [admin] section, creating the file with permissions 0600 if absent. mnm auth github MUST persist the read-uplift bearer to the same file under [read_uplift]. File permissions MUST be re-verified on every read; the CLI MUST refuse to operate on a world-readable auth file. No keychain backend in v1 (D28). | 8 | 🔵 Confirmed |
| FR-069 | mnm MUST NOT silently refresh an expired admin JWT; expired-admin-token errors from the server MUST be surfaced with remediation 'run mnm login'. Expired read-uplift bearers MUST surface 'run mnm auth github'. The CLI MUST distinguish the two token types in error messages — admins should never be told to run the wrong renewal command. | 8 | 🔵 Confirmed |
| FR-070 | mnm users mutations MUST preserve the user-store TOML's schema_version field and refuse to operate on a file whose schema_version is newer than the binary supports (read-only operations may still succeed in compatibility mode). | 8 | 🔵 Confirmed |
| FR-071 | Every mutation to the local user-store TOML MUST produce a stderr (human mode) or NDJSON record (--json mode) warning that the change is local only and naming the deployment step required to apply it (D20). | 8 | 🔵 Confirmed |
| FR-072 | mnm versions rollback MUST resolve the target revision by querying GET /v1/sources/{slug}/versions for the most recent prior active version, then call POST /v1/sources/{slug}/versions/{rev}/promote with that revision; if no prior version exists, the command MUST exit non-zero with a typed error. | 8 | 🔵 Confirmed |
| FR-073 | mnm doctor MUST produce a structured report covering: cli_version, models (presence, digests, dim), mcp (installation status across known agents), cloud (reachable, model_match, ratelimit_tier), local_keypair (presence per user_id), auth (path to auth.toml, permission bits, admin token presence+expiry+user_id, read_uplift token presence+expiry+github_login), admin_visibility (resolved value), telemetry (enabled state and resolution source), config (path and schema_version). With --json the report is a single object. | 8 | 🔵 Confirmed |
| FR-074 | mnm mcp install MUST recognize at least claude-code at MVP; additional clients (cursor, continue, others) MUST be addable via a built-in registry, not code changes. For unknown agents the command MUST print the JSON snippet ready to paste plus the documented MCP config schema URL. | 8 | 🔵 Confirmed |
| FR-075 | Every Story 8 admin subcommand whose effect is a server mutation MUST honor --dry-run (preview the request the CLI would send) and --json (NDJSON output). Read-only commands MUST honor --json. | 8 | 🔵 Confirmed |
| FR-076 | Every cloud search result MUST carry trust_score, confidence, and confidence_factors. These fields are additive to the Story 4 response shape and MUST NOT break existing callers (Constitution I forward-compatibility). | 6,4 | 🔵 Confirmed |
| FR-077 | trust_score MUST be computed deterministically from (provenance.attribution, provenance.verified, provenance.verified_by, freshness age, provenance.deprecation, language_target match) per the loaded scoring policy. Identical inputs MUST produce identical outputs. | 6 | 🔵 Confirmed |
| FR-078 | confidence MUST be the weighted geometric-mean blend defined in D24: trust_score^trust_weight * relevance^relevance_weight, both clamped to [0,1] before exponentiation and the result clamped to [0,1] after multiplication. | 6 | 🔵 Confirmed |
| FR-079 | When rerank=true, the MCP server MUST replace the cloud-supplied relevance term with the normalized reranker score and recompute confidence using the same blend formula and weights. confidence_factors.relevance_source MUST be set to 'rerank' in that case. | 6,5 | 🔵 Confirmed |
| FR-080 | POST /v1/search MUST accept optional sort_by ∈ {confidence, trust, relevance, score} (default: confidence) and optional min_confidence ∈ [0,1] (default: 0.0). min_confidence MUST be applied before limit; sort_by orders survivors. | 6,4 | 🔵 Confirmed |
| FR-081 | Search response MUST include search_metadata.filtered_by_confidence (count of candidates dropped by min_confidence). When sort_by is supplied, search_metadata.sort_by reflects the resolved value. | 6,4 | 🔵 Confirmed |
| FR-082 | Scoring policy MUST be loaded once at startup from MIDNIGHT_MANUAL_SCORING_POLICY (TOML file) or fall back to compiled-in defaults. Policy load failures (parse error, unknown key, out-of-range weight) MUST fail server startup with a structured error naming the file path and the offending entry. | 6,9 | 🔵 Confirmed |
| FR-083 | confidence_factors MUST include every input that contributed to the final score: attribution, attribution_multiplier, verified, verified_by, verification_multiplier, age_days, age_source, freshness_multiplier, deprecation, deprecation_multiplier, language_target_query, language_targets_chunk, version_match_multiplier, relevance_source, relevance_multiplier. | 6 | 🔵 Confirmed |
| FR-084 | Confidence and trust_score MUST be clamped to [0.0, 1.0]; out-of-range computations MUST log a structured warning with the offending input vector but MUST NOT crash the request. | 6 | 🔵 Confirmed |
| FR-085 | When the MCP reranker is unavailable (models_missing or models_stale) and rerank=true was requested, the MCP server MUST silently fall back to the cloud-supplied confidence and set confidence_factors.relevance_source = 'rrf', plus emit a telemetry event so observers can detect degraded reranking. | 6,5 | 🔵 Confirmed |
| FR-086 | POST /v1/search MUST accept a queries:array of length 1..N where N=MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST (default 10, hard ceiling 50). Each entry is {text:string, vector:float[]}. Exceeding the cap returns 400 invalid_request before any retrieval or rate-limit consumption (D25). | 7,4 | 🔵 Confirmed |
| FR-087 | Multi-query rate-limit cost MUST equal max(1, distinct queries.length) tokens per request. Server-side deduplication of identical (text+vector) entries MUST occur before charging (so a request with 5 entries, 2 identical, costs 4 tokens). search_metadata.deduplicated_count reports the dedup activity (D25). | 7 | 🔵 Confirmed |
| FR-088 | Cloud search MUST run FTS + pgvector retrieval once per query and RRF-merge across (queries × modes) in one pass. Per-query timings and candidate counts MUST be reported in search_metadata.per_query. | 7 | 🔵 Confirmed |
| FR-089 | Each result MUST carry scores.matched_queries listing the 0-based input query indices that contributed at least one (FTS or vector) rank to the result. | 7 | 🔵 Confirmed |
| FR-090 | The single-query convenience form (top-level query+vector) MUST produce byte-identical responses to the equivalent 1-element queries form. This invariant is preserved by an internal test. | 7 | 🔵 Confirmed |
| FR-091 | The MCP server's search tool description MUST include a 'Patterns' subsection naming hyde, multi_query, and step_back with one-line examples. The description MUST update whenever the patterns evolve. | 7,5 | 🔵 Confirmed |
| FR-092 | The repo MUST ship docs/cookbook/query-enhancement.md containing worked examples for HyDE, multi-query expansion, and step-back prompting. Each example MUST include the agent's LLM prompt, the resulting queries array, and a note on rate-limit cost. | 7 | 🔵 Confirmed |
| FR-093 | Multi-query input MUST be validated holistically (all queries parse, all vectors match the corpus model's dimension, no entry has empty text) before any retrieval work begins. Invalid input rejects the entire request — partial-batch retrieval is not supported. | 7 | 🔵 Confirmed |
| FR-094 | The CLI 'mnm search' command MUST accept multi-query input via repeated --query flags or a --queries-stdin JSON shape, and MUST emit per-query and per-result diagnostics in --json output. | 7,8 | 🔵 Confirmed |
| FR-095 | Every supported install channel (cargo install, brew install, GitHub Release tarball) MUST install both midnight-manual and mnm binaries from the same release tag SHA; mnm version MUST report the same {version, commit} regardless of channel. | 10 | 🔵 Confirmed |
| FR-096 | The release pipeline MUST be triggered by merging a release-please PR; merging the release PR MUST in one workflow run: tag the SHA, build the binary matrix, upload GitHub Release artifacts with SHA256SUMS, publish to crates.io, update the Homebrew formula via PR to the tap repo, push the multi-arch Docker image to GHCR, and trigger flyctl deploy. | 10 | 🔵 Confirmed |
| FR-097 | Cargo.toml MUST pin an MSRV via rust-version; CI MUST run the full test suite under both MSRV and stable toolchains on every PR; either failing blocks the PR. | 10 | 🔵 Confirmed |
| FR-098 | Every PR MUST be Conventional-Commit-linted; commits violating the format MUST block merge. Breaking changes MUST be marked with '!' in the type/scope or a 'BREAKING CHANGE:' footer. | 10 | 🔵 Confirmed |
| FR-099 | Build target matrix MUST cover {x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu, aarch64-unknown-linux-musl, x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-msvc} for user-facing binaries. Docker image MUST cover linux/amd64 and linux/arm64. | 10 | 🔵 Confirmed |
| FR-100 | Every GitHub Release tarball MUST contain both user-facing binaries, a SHA256SUMS file, and a LICENSE file; verifying SHA256SUMS MUST succeed for every artifact in the release. | 10 | 🔵 Confirmed |
| FR-101 | Published release artifacts MUST be immutable: once a vX.Y.Z release is published, its tarball / crate / Docker tag MUST NOT be edited or replaced. Corrections ship as a new patch release. Yanking a crate from crates.io is permitted and is the only way to retract a release. | 10 | 🔵 Confirmed |
| FR-102 | The Docker server image MUST contain only the midnight-manual-server binary (not the user-facing CLI binaries); the image base is distroless or equivalently minimal (no shell, no package manager). | 10,9 | 🔵 Confirmed |
| FR-103 | cargo-audit (or cargo-vet) MUST run on every PR; CVEs in dependencies MUST block the merge until addressed (acknowledge with a documented allowlist, or upgrade). | 10 | 🔵 Confirmed |
| FR-104 | mnm version MUST be deterministic at build time: it MUST report exactly the package.version, the git SHA of the build commit, and the build date. The same SHA MUST always produce the same {version, commit} (date is allowed to differ for reproducibility tests). | 10 | 🔵 Confirmed |
| FR-105 | All three components (cloud server, MCP server, CLI) MUST emit structured JSON logs only — one event per line, no human-formatted text intermixed on the same channel as machine data. | 11 | 🔵 Confirmed |
| FR-106 | request_id MUST propagate end-to-end via the X-Request-Id header: client-generated when the client initiates a request, echoed by the server in its response, and present in every log line of every component touching that request. | 11,4 | 🔵 Confirmed |
| FR-107 | Telemetry MUST be opt-out with three equivalent mechanisms (env, config key, CLI command). All three MUST be documented in the README and in --help output of every component that ships telemetry (Constitution VII). | 11 | 🔵 Confirmed |
| FR-108 | When telemetry is disabled by any mechanism, the affected component MUST send zero events; the network connection to /v1/telemetry MUST NOT be opened; and any in-memory queued events MUST be discarded. | 11 | 🔵 Confirmed |
| FR-109 | Every telemetry event MUST conform to a versioned, per-event-type schema; events with unknown event_types or unknown fields MUST be dropped at the server boundary with a structured warning. The schema and the dropped-events metric MUST be the source of truth for what is collected. | 11 | 🔵 Confirmed |
| FR-110 | telemetry_event_raw rows MUST be auto-deleted after MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS (default 7). telemetry_aggregate_daily rows MUST be retained indefinitely. Deletion runs as part of the sweep job (Story 9 FR-063). | 11,9 | 🔵 Confirmed |
| FR-111 | GET /metrics MUST return Prometheus exposition format and MUST expose at minimum: requests_total, request_duration_seconds_bucket, source_versions_active, embedding_models_in_corpus, telemetry_events_received_total, telemetry_events_dropped_total, sweep_runs_total. Metrics MUST never include user-identifying labels. | 11,4 | 🔵 Confirmed |
| FR-112 | Canary tests MUST run in CI on every PR: a known set of canary strings is fed through every code path that handles user-controllable content; post-run inspection of every log file and every telemetry row MUST find zero canary occurrences. Any match fails the build. | 11 | 🔵 Confirmed |
| FR-113 | When the telemetry endpoint is unavailable, clients MUST NOT block any user-facing operation; events MUST queue in-memory with a bounded buffer (default 1000); on overflow, oldest events MUST be dropped (FIFO) and the drop count reported via the next successful flush. | 11 | 🔵 Confirmed |
| FR-114 | The repo MUST ship a top-level 'Telemetry & Privacy' section in the README listing: collected event types, what is NOT collected (forbidden set), endpoint URL, opt-out mechanisms, retention period, and a reference to the canary tests. The section MUST be verified at release time per Constitution XI. | 11 | 🔵 Confirmed |
| FR-115 | mnm auth github MUST initiate the GitHub OAuth flow against the cloud server's /v1/auth/github/start endpoint (web browser by default; device flow with --no-browser per Constitution IV frictionless setup), cache the resulting 30-day read-uplift bearer in auth.toml[read_uplift], and report the github_login of the authenticated user. mnm auth status reports both token states (presence, expiry, identity). mnm auth logout clears the read-uplift entry only. | 8 | 🔵 Confirmed |
| FR-116 | POST /v1/telemetry MUST be exempt from the standard rate-limit tiers (anonymous / SSO / CIDR override) and MUST use a dedicated per-IP bucket sized for the documented flush behavior (default 1000 events/min/IP, configurable via MIDNIGHT_MANUAL_TELEMETRY_RATE_LIMIT). The telemetry rate-limit decision MUST NOT consume tokens from the search rate-limit bucket. /v1/telemetry MUST NOT require authentication; supplied bearer tokens are ignored on this endpoint to prevent linking telemetry events to identified users. | 11,9 | 🔵 Confirmed |
| FR-117 | GitHub OAuth callback (per Story 9 FR-062) MUST mint a 30-day read-uplift bearer (configurable via MIDNIGHT_MANUAL_READ_TOKEN_TTL_DAYS, D28). The token grants ONLY read-uplift rate-limit tier — never write permissions. Token format and signing parity with admin JWTs is permitted (HS256 with distinct claims indicating tier=read_uplift) but the tier check MUST happen at the route layer before any write endpoint is reached. | 9 | 🔵 Confirmed |

### Key Entities

Concrete DDL is deferred to Story 2 planning. The shapes below are the contract for every downstream story.

**v1 capacity target**: design assumes a corpus of ~100k chunks (referenced by SC-002, SC-013). This is comfortably within Fly.io managed Postgres + pgvector capabilities on a default machine size. Scaling past ~1M chunks may require partitioning the chunk table by source_version and revisiting HNSW index parameters — out of scope for v1.

#### `source`
A stable handle for a logical content source (a docs repo, a code repo, a one-off file).

| Field | Type | Notes |
|---|---|---|
| `id` | uuid PK | |
| `slug` | text UNIQUE | Stable human handle (e.g. `midnight-docs`) |
| `display_name` | text | |
| `kind` | enum | `docs_site`, `code_repo`, `standalone`, `mixed` |
| `origin_url` | text NULL | Where ingest pulls from |
| `retention_count` | int NOT NULL DEFAULT 5 | Per-source override of D15 |
| `created_at` | timestamptz | |
| `retired_at` | timestamptz NULL | Soft-retire of an entire source |

#### `source_version`
Immutable snapshot of a source. The unit of versioning (D8 / D15).

| Field | Type | Notes |
|---|---|---|
| `id` | uuid PK | |
| `source_id` | uuid FK → source | |
| `revision` | int | Monotonic per source |
| `is_active` | bool | Partial unique index: `(source_id) WHERE is_active` |
| `ingested_at` | timestamptz | |
| `ingest_cli_version` | text | Provenance for the ingest run |
| `embedding_model_id` | uuid FK → embedding_model | All chunks in this version share this model |
| `content_hash` | text | Hash of the version's manifest of document hashes |
| `notes` | text NULL | |
| `retired_at` | timestamptz NULL | Soft-delete marker for the sweep job |

**`source_version` lifecycle state machine** (resolves the inactive/retired/deleted ambiguity flagged in the consistency pass):

```
                  ┌───────────────────────────────────────────┐
                  │                                           │
   building ─────►│   active     ─────►   inactive   ─────►   retired  ─────►  (hard-deleted)
   (allocated by  │   (is_active │       (is_active │        (retired_at        (row removed by
    POST          │    = true)   │        = false,  │         set by sweep      sweep after
    ingest-runs)  │              │        retired_at│         or by explicit    SWEEP_GRACE since
                  │              │        = NULL)   │         POST .../retire)  retired_at)
                  └──────────────┴──────────────────┴─────────────────────────────────────►
```

Transitions:
1. `building → active`: `POST /v1/sources/{slug}/ingest-runs/{id}/finalize` (Story 9 FR-061) atomically flips the new version to `is_active=true` and the prior active to `is_active=false`.
2. `building → aborted`: `POST .../abort`, or sweep after `MIDNIGHT_MANUAL_ABORT_GRACE` (default 1h) of no activity. Aborted rows skip directly to retired.
3. `active → inactive`: automatic on finalize of a newer version.
4. `inactive → retired`: sweep marks `retired_at = now()` when the version falls outside the source's `retention_count` window; OR explicit `POST /v1/sources/{slug}/versions/{rev}/retire` (Story 9, called by `mnm versions retire`).
5. `retired → deleted`: sweep hard-deletes the row and all its chunks/documents/nodes/packages once `now() - retired_at > MIDNIGHT_MANUAL_SWEEP_GRACE` (default 24h).

Active reads (FR-027) filter on `is_active=true`, so chunks become invisible to public reads at transition (3), not (5). The grace window in (5) exists to support last-mile in-flight queries and operator inspection.

#### `embedding_model`
Registry of models the corpus has been encoded with. Initial row: `(bge-base-en-v1.5, revision=1, dim=768, provider=baai)`.

| Field | Type | Notes |
|---|---|---|
| `id` | uuid PK | |
| `name` | text | e.g. `bge-base-en-v1.5` |
| `revision` | int | Bump when the same model name re-trains |
| `dim` | int | Vector dimension (768 for the default) |
| `provider` | text | e.g. `baai`, `nomic-ai`, `snowflake` |
| `created_at` | timestamptz | |

**Wire format**: model identifiers cross API boundaries as the string `{name}@{revision}` — e.g. `bge-base-en-v1.5@1`. This is the form `client_embedding_model` takes in `POST /v1/search` (Story 4), the form `GET /v1/models/active` returns alongside the structured fields, and the form error envelopes use in `embedding_model_mismatch` context. Parse with a single split on `@`.

#### `node`
Generic tree representing the parent chain (root → groups → documents → chunks). One root per source_version.

| Field | Type | Notes |
|---|---|---|
| `id` | uuid PK | |
| `source_version_id` | uuid FK | |
| `parent_node_id` | uuid FK NULL | NULL at the root |
| `kind` | enum | `root`, `group`, `document`, `chunk` |
| `name` | text | Display label for the node |
| `order_index` | int | Position among siblings |
| `created_at` | timestamptz | |

#### `document`
An ingested page (Markdown) or file (code, plaintext).

| Field | Type | Notes |
|---|---|---|
| `id` | uuid PK | |
| `source_version_id` | uuid FK | |
| `node_id` | uuid FK → node | The `document`-kind node |
| `kind` | enum | `markdown`, `code`, `plaintext` |
| `source_url` | text NULL | Raw upstream URL (D13) |
| `published_url` | text NULL | Rendered/public URL (D13) |
| `source_path` | text | Relative to the ingest root |
| `language` | text NULL | `rust`, `compact`, `typescript`, … |
| `content_hash` | text | For reuse detection (FR-015) |
| `source_modified_at` | timestamptz NULL | From upstream when known |
| `frontmatter` | jsonb NULL | Verbatim Markdown frontmatter |
| `provenance` | jsonb NOT NULL | Schema below |
| `package_id` | uuid FK NULL → package | Single package per document |
| `char_count` | int | |
| `token_count` | int | |
| `created_at` | timestamptz | |

#### `chunk`
The smallest indexed unit. Carries the FTS vector and the embedding.

| Field | Type | Notes |
|---|---|---|
| `id` | uuid PK | |
| `source_version_id` | uuid FK | |
| `document_id` | uuid FK | |
| `node_id` | uuid FK → node | The `chunk`-kind node |
| `chunk_index` | int | 0-based within document |
| `total_chunks` | int | Denormalized for "N of M" UX |
| `content` | text | The chunk text |
| `content_hash` | text | For chunk-level reuse if granularity is added later |
| `tsvector` | tsvector GENERATED STORED | GIN-indexed for FTS |
| `embedding` | vector(768) NULL | HNSW-indexed; NULL when `status='embed_failed'` |
| `embedding_model_id` | uuid FK | Must equal `source_version.embedding_model_id` |
| `heading_path` | text[] | Markdown: H1 → nearest heading |
| `symbol_path` | text[] | Code: `module.class.fn` |
| `start_byte` | int | Byte offset into source for reconstruction |
| `end_byte` | int | |
| `token_count` | int | |
| `status` | enum | `ready`, `embed_failed`, `deprecated` |
| `created_at` | timestamptz | |

#### `package`
Language-aware grouping of code documents.

| Field | Type | Notes |
|---|---|---|
| `id` | uuid PK | |
| `source_version_id` | uuid FK | |
| `kind` | enum | `rust`, `npm`, `compact`, `other` |
| `name` | text | e.g. `midnight-foo`, `@midnight-ntwrk/midnight-js`, `FungibleToken` |
| `version` | text NULL | When detectable (Cargo.toml/package.json); always NULL for Compact |
| `manifest_path` | text NULL | Path to Cargo.toml / package.json; NULL for Compact |
| `metadata` | jsonb | Language-specific extras |

#### Provenance JSONB (on `document.provenance`)

| Field | Type | Notes |
|---|---|---|
| `attribution` | enum | `foundation`, `partner`, `third_party`, `community`, `unknown` |
| `verified` | bool | |
| `verified_by` | text NULL | e.g. `midnight-foundation`, `openzeppelin` |
| `verified_at` | date NULL | |
| `verification_notes` | text NULL | |
| `language_targets` | array | `{ name, version_constraint }` (e.g. `{name: "compact", version_constraint: ">=0.23"}`) |
| `sdk_dependencies` | array | `{ kind, name, version_constraint }` — kind ∈ `npm`, `cargo`, `compact` |
| `deprecation` | object | `{ is_deprecated, since: date NULL, reason: text NULL }` |
| `tags` | array of text | Admin-set free-form tags |
| `content_type` | enum | `doc`, `tutorial`, `reference`, `example`, `contract_source`, `sdk_source`, `test`, `readme` |

#### Tables that exist in the schema for write-side stories (Story 8/9) — not part of Story 1 scope

- `user` — (id, user_id text UNIQUE, public_key bytea, role enum, created_at, revoked_at NULL)
- `api_key` — read-side uplift tokens
- `rate_limit_override` — CIDR + limit + expires_at (D11)

These are listed here so the schema migration plan is complete; their semantics are owned by Stories 8 and 9.

#### Indexes summary

| Table | Index | Purpose |
|---|---|---|
| `chunk` | HNSW on `embedding` | Vector ANN |
| `chunk` | GIN on `tsvector` | FTS |
| `chunk` | btree `(source_version_id, status)` | Active filtering |
| `source_version` | partial unique `(source_id) WHERE is_active` | One active per source |
| `document` | btree `(content_hash)` | Reuse detection at ingest |
| `node` | btree `(parent_node_id, order_index)` | Parent / sibling walks |

---

## Success Criteria

| ID | Criterion | Measurement | Stories |
|----|-----------|-------------|---------|
| SC-001 | Schema covers 100% of the test corpus (midnight-docs main branch + 3 example code repos + 5 standalone files) without manual fixup. | CI fixture ingests the test corpus end-to-end with zero schema-violation errors; assertion on row counts and metadata-field non-null rate. | 1,2,3 |
| SC-002 | Round-trip 'fetch chunk then walk parents to root then list document siblings' completes under 50ms p95 on a 100k-chunk corpus on Fly.io managed Postgres. | pgbench-style scenario in CI; track p95 via timing histograms over 1000 trials. | 1,4 |
| SC-003 | Switching the corpus embedding model from bge-base to any other supported model requires only an ingest run plus an admin flag flip; no DDL migration. | Integration test: ingest under model A; ingest new source_version under model B; assert promotion succeeds without DDL; assert 409 mismatch fires for stale clients. | 1,4,5 |
| SC-004 | At most one source_version per source is_active at any moment, enforced by the database not by application logic. | Concurrent test: two transactions attempt to promote different source_versions to active simultaneously; exactly one succeeds; the other receives a constraint violation. | 1 |
| SC-005 | Every chunk exposed by the read API can be unambiguously traced to its source, source_version, embedding_model, document, and optional package. | Schema test: every API-returned chunk has non-null FKs to source_version and document; source_version.embedding_model_id non-null; package nullable but consistent with document.language when present. | 1,4,5 |
| SC-006 | Adding a new field to document.provenance JSONB does not require a schema migration; the read API surfaces the new field automatically. | Integration test adds a new provenance key during ingest and asserts the read API returns it in the chunk metadata payload. | 1,4 |
| SC-007 | Initial ingest of the current midnight-docs main branch (~300 pages) completes in under 5 minutes on a developer laptop, including local embedding. | CI fixture clones midnight-docs main; runs 'mnm ingest md midnight-docs' against a local server; asserts wall-clock < 300s on the reference dev profile (8-core M-series or x86 equivalent). | 2 |
| SC-008 | Re-ingesting an unchanged corpus completes in under 30 seconds with zero re-embeds. | Same CI fixture: run ingest twice in succession; assert second run's wall-clock < 30s and summary.stats.embeds_skipped equals summary.stats.chunks_total. | 2 |
| SC-009 | --dry-run performs zero writes to the cloud server. | Test harness mocks all write endpoints and asserts none were called during a dry-run ingest; the harness also asserts the dry-run summary still reports a complete plan. | 2,3 |
| SC-010 | NDJSON output emitted under --json validates against a JSON Schema for every documented event type (manifest_validated, file_processed, chunked, embedded, uploaded, warning, error, summary). | CI runs 'mnm ingest md ... --json' against a test corpus and pipes output through a JSON Schema validator; non-conforming records fail the build. | 2,3,8 |
| SC-011 | A simulated network interruption after 50% of chunks uploaded results in successful resume on the next invocation with no duplicate chunks and no lost chunks. | Integration test injects a connection-reset after N batches; second run completes successfully; cloud-side assertion: every chunk appears exactly once in the final source_version. | 2 |
| SC-012 | Manifest validation catches every documented invalid case before any chunk is uploaded. | Table-driven test feeds 10 invalid manifests (missing file, duplicate parent, cycle, unknown field, etc.); each run exits non-zero with a structured error and asserts zero write calls were made. | 2 |
| SC-013 | POST /v1/search p95 latency under 500ms on a 100k-chunk corpus on Fly.io managed Postgres (single region). | Load test issues 1000 queries against the test corpus; histogram captures p50/p95/p99; p95 must be below 500ms. Budget breakdown: FTS query <100ms, vector query <150ms, RRF merge <50ms, serialization <50ms. | 4 |
| SC-014 | Hybrid retrieval recall@10 measurably outperforms either single mode on a held-out test set. | Construct a labelled test set of 50 query-relevant_chunk pairs from the Midnight docs. Compare recall@10 of FTS-only vs vector-only vs hybrid+RRF; hybrid must exceed both single modes by at least 10 percentage points. | 4 |
| SC-015 | Active-version filter never leaks inactive content. | Test inserts a known-distinctive chunk into an inactive source_version; runs 100 search queries known to hit semantically similar content; asserts that the distinctive chunk never appears in any result. | 4,1 |
| SC-016 | Every error response validates against the documented JSON Schema for the error envelope. | Contract test enumerates every documented error code, triggers each one against the running server, and validates the response body shape with a JSON Schema validator. | 4 |
| SC-017 | Rate-limit tier precedence behaves as specified (CIDR override beats SSO tier beats anonymous tier). | Integration test seeds a CIDR override entry, exercises an anonymous request from a matching IP, an SSO-authenticated request from a matching IP, and requests from outside the CIDR; asserts X-RateLimit-Limit reflects the correct tier for each case. | 4,9 |
| SC-018 | Server logs contain zero occurrences of query content, bearer tokens, or PII. | Soak test exercises every endpoint with payloads containing canary tokens and known query strings; post-run grep against all log files asserts none of the canaries appear; CI fails if any do. | 4,11 |
| SC-019 | MCP handshake completes within 500ms cold start (process launch to handshake-done) on a developer laptop. | Test launches the server in a subprocess with a cold filesystem cache, performs the MCP initialize handshake, and asserts elapsed time < 500ms over 100 trials, with p95 reported. | 5 |
| SC-020 | First-call search latency (including lazy model load) under 2.5s; steady-state search p95 under 1s end-to-end (Constitution IV). | Integration test: cold-start, issue 100 search calls; record latencies; assert first-call < 2500ms and p95 of calls 2..100 < 1000ms. | 5 |
| SC-021 | Reranking measurably improves nDCG@5 over the un-reranked cloud result set on a held-out test set. | Build a labelled relevance test set (50 query/relevant-chunk pairs); run search with rerank=false and rerank=true; assert nDCG@5 improvement of at least 0.05 absolute (typical published cross-encoder uplift on similar workloads). | 5,6 |
| SC-022 | MCP server logs contain zero query content, bearer tokens, or chunk content under any tool invocation. | Soak test exercises every tool with canary-laden inputs; post-run grep against all log files for the canaries must return zero matches; CI fails if any match. | 5,11 |
| SC-023 | Embedding model mismatch is recoverable from inside the MCP session: agent receives typed error, calls pull_models, retries the original tool, succeeds. | End-to-end test scripts the full recovery dance against a server whose cloud counterpart returns 409 on the first call and 200 after pull_models completes. | 5,12 |
| SC-024 | Graceful shutdown completes within 1 second of SIGTERM with no orphaned HTTP connections and no half-emitted MCP responses. | Test launches the server, fires a long-running search request (mock cloud delays response 10s), sends SIGTERM at t=200ms; asserts process exits within 1s and lsof reports no lingering sockets after exit. | 5 |
| SC-025 | Ingesting a representative Midnight code-example repo (e.g. counter-tutorial) produces correct package metadata for every file. | CI fixture clones the reference example repo, runs 'mnm ingest code', and asserts: every .rs chunk has package.kind='rust' with a non-null name; every .ts chunk has package.kind='npm' with a non-null name; every .compact chunk has package.kind='compact' with a non-null name matching one of the file's declared modules. | 3 |
| SC-026 | Ingesting a Cargo workspace assigns each chunk to the correct member crate, and the workspace virtual root is not treated as a package. | Test fixture creates a synthetic 3-member Cargo workspace with a virtual root; ingests it; asserts the source_version contains exactly 3 packages and every .rs file's chunks resolve to its member crate's Cargo.toml. | 3 |
| SC-027 | Ingesting a TypeScript monorepo (pnpm or npm workspaces) assigns each chunk to the correct member package. | Test fixture creates a 2-package pnpm workspace; ingests; asserts each .ts file's chunks resolve to its nearest package.json with the right name. | 3 |
| SC-028 | Ingesting the OpenZeppelin compact-contracts repo (a real-world Compact monorepo) produces module-tagged chunks for every .compact file declaring modules. | Acceptance test clones https://github.com/OpenZeppelin/compact-contracts; ingests; asserts every .compact file with a 'module Foo {' declaration has at least one chunk tagged package.kind='compact' with name = Foo. | 3 |
| SC-029 | Tree-sitter fallback works: a deliberately malformed Rust file still produces indexable chunks via the line-window fallback. | Test injects a syntactically-broken .rs file alongside valid files; asserts ingest emits a warning naming the broken file, that the file still appears in summary.files_processed, and that chunks from it have symbol_path = []. | 3 |
| SC-030 | Git-mode ingest cleans up its temp directory regardless of success, abort, or panic. | Three integration test cases: (1) successful clone + ingest, (2) clone succeeds but upload fails midway, (3) process is sent SIGKILL mid-clone. After each case, assert the temp directory tree no longer exists. | 3 |
| SC-031 | Challenge-response auth round-trip (challenge + verify) completes in under 200ms p95 from the CLI's perspective. | Integration test runs 'mnm login' 100 times against a deployed server; measures end-to-end wall-clock; asserts p95 < 200ms. | 9 |
| SC-032 | Finalize is atomic: under concurrent finalize attempts on the same source from two clients, exactly one succeeds and the partial-unique constraint is never violated. | Stress test fires N=100 concurrent finalize requests for two competing source_versions of the same source; assert that exactly one wins, one loses with 409, and is_active count for the source is exactly 1 at all times during the test. | 9,1 |
| SC-033 | JWT rotation invalidates all outstanding admin tokens within one restart cycle. | Test: issue 5 admin JWTs; rotate MIDNIGHT_MANUAL_JWT_SECRET and restart the server; assert all 5 previously-issued tokens now return 401 on a protected endpoint. | 9 |
| SC-034 | GitHub-SSO callback rejects users outside the configured org and never mints a token for them. | Integration test against a GitHub mock: simulate a successful OAuth callback for a user not in the org; assert response is 403 and no rate_limit_tracking row is created for that user. | 9 |
| SC-035 | Sweep job removes eligible inactive source_versions within one cycle of the configured grace window expiring. | Test: insert 3 inactive source_versions with retired_at older than grace; run one sweep cycle; assert all 3 versions and their dependent rows are gone; remaining sources are untouched. | 9 |
| SC-036 | Cold-start of the cloud server (process launch to readyz=200) under 5 seconds on the reference Fly.io machine. | Deploy test machine; restart the process; record time from PID creation to first successful /readyz response; assert < 5s. (Constitution IV is a CLIENT cold-start target; this is the server's analog.) | 9,10 |
| SC-037 | Default 'mnm --help' lists no admin commands; with MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1 it lists all admin commands. | Snapshot test captures the help output in both modes; admin command names appear only in admin mode; the snapshot is reviewed on every PR. | 8 |
| SC-038 | Every admin command remains invokable when admin commands are hidden from help. | Test runs each admin command by name with admin-visibility disabled; asserts exit codes and parsed output match the visible mode. | 8 |
| SC-039 | Private-key files written by 'mnm keys generate' have permissions exactly 0600; the keypair survives a round-trip 'mnm login' against the deployed server. | Filesystem check after key generation; integration test runs login successfully using the freshly-generated keypair against a test server. | 8 |
| SC-040 | Local user-store edits are warned and never auto-deployed. | Test runs 'mnm users add ...' and asserts (a) the local TOML changed, (b) the cloud server's /v1/admin/users (which does not exist) was not called, (c) a deploy reminder appears on stderr or in --json output. | 8 |
| SC-041 | mnm mcp install --agent claude-code succeeds on a fresh machine: the agent's MCP config now references the local mnm binary, and starting the agent successfully invokes the MCP handshake. | End-to-end test in CI: install mnm, run 'mnm mcp install --agent claude-code', launch the agent, assert it reports the midnight-manual MCP server connected. | 8,10 |
| SC-042 | mnm doctor produces a complete report in under 3 seconds on a developer laptop, including cloud reachability check. | Benchmark: time 'mnm doctor --json' across 50 runs; assert p95 < 3000ms; assert the JSON output validates against the documented schema. | 8 |
| SC-043 | Trust score is monotonic in attribution: for otherwise-identical inputs, foundation > partner > third_party > community > unknown. | Table-driven test enumerates the five attributions with all other factors held constant and asserts the strict ordering on trust_score. | 6 |
| SC-044 | Freshness produces a measurable drop between 1-month-old and 2-year-old content under default policy. | Test computes trust_score for identical chunks at age=30 days vs age=730 days; asserts a difference of at least 0.20 absolute under the default half_life_days=180. | 6 |
| SC-045 | Deprecated content scores at least 0.3 absolute lower in trust_score than equivalent non-deprecated content. | Table-driven test toggles deprecation.is_deprecated on otherwise-identical inputs; asserts trust_score delta >= 0.30 under default policy. | 6 |
| SC-046 | min_confidence filter excludes the right results and reports the dropped count. | Test seeds results with confidences {0.1, 0.3, 0.6, 0.8}; queries with min_confidence=0.5; asserts response contains only the {0.6, 0.8} results and search_metadata.filtered_by_confidence=2. | 6 |
| SC-047 | Confidence is reproducible: identical inputs always produce bit-identical confidence values. | Property-based test (proptest) generates random (provenance, policy, relevance) tuples; computes confidence twice; asserts byte-equal floats. 10,000 iterations. | 6 |
| SC-048 | Confidence-sorted results outperform RRF-sorted results on a held-out trust-relevance benchmark. | Construct a 50-pair benchmark with relevance AND trust labels; assert that nDCG@5 on the combined label is at least 5 percentage points higher when sort_by=confidence vs sort_by=score under default policy. | 6 |
| SC-049 | Multi-query (3-query paraphrase) recall@10 exceeds single-query recall@10 by at least 8 percentage points absolute on the labelled benchmark. | Build a 50-pair test set; run search with the user's original question as a single query, then with the original plus 2 paraphrases; assert recall@10_multi >= recall@10_single + 0.08. | 7 |
| SC-050 | Cost accounting is exact: a request with N distinct queries consumes exactly N tokens, and a request with K duplicates consumes (N - dup) tokens. | Test seeds rate-limit bucket at 10, fires a 5-query request (no dups) expecting bucket=5, then a 4-query request with 2 dups expecting bucket=3, then a 5-query request expecting 429 rate_limited with no DB call. | 7 |
| SC-051 | Single-query convenience form is byte-identical to the 1-element queries form. | Property test compares responses for 100 random inputs across the two forms; the response bodies (sans request_id) MUST be byte-equal. | 7 |
| SC-052 | Cap enforcement: requests above the configured cap reject without DB query and without consuming rate-limit tokens. | Test sets MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST=5; fires a 6-query request; asserts 400 invalid_request, zero DB queries (via instrumented connection pool), and unchanged rate-limit bucket. | 7 |
| SC-053 | Cookbook documents pass markdown link-check and code-block schema validation in CI. | CI runs (a) a markdown link checker against docs/cookbook/query-enhancement.md (no broken links), (b) a JSON-Schema validator against every fenced JSON block claiming to be a 'queries' array (matches the documented schema). | 7 |
| SC-054 | MCP search tool description contains the named pattern subsection in a programmatically-checkable form. | Test fetches the server's tool catalog via the MCP handshake and asserts the search tool's description contains the strings 'hyde', 'multi_query', and 'step_back' inside a section heading named 'Patterns'. | 7,5 |
| SC-055 | Every install channel produces identical mnm version output for the same release. | CI matrix installs the same vX.Y.Z via cargo, brew, and the GitHub Release tarball on a clean container per channel; runs 'mnm version --json'; asserts {version, commit} are byte-identical across channels. | 10 |
| SC-056 | Release pipeline completes in under 60 minutes wall-clock from release-PR-merge to flyctl-deploy-success. | Pipeline duration histogram tracked over the last 10 releases; p95 < 60 min; alert if a single run exceeds 90 min. | 10 |
| SC-057 | All artifacts in a release tarball verify against SHA256SUMS. | CI smoke test downloads each tarball post-release and runs 'sha256sum -c SHA256SUMS'; any mismatch fails the post-release verification job. | 10 |
| SC-058 | Server image is < 50 MB compressed. | Post-build job inspects the GHCR image layer sizes; sum of compressed layer sizes < 50 MB. Distroless base + statically-linked Rust binary is the architecture; deviations require justification. | 10 |
| SC-059 | Cold start of the cloud server binary in its production Docker image is under 5 seconds (process launch to /readyz=200). | Post-release smoke test boots the image on a Fly-equivalent machine; measures cold-start; asserts < 5s. This is the same SC-036 from Story 9, now measured against the production image specifically. | 10,9 |
| SC-060 | Released crate compiles on the documented MSRV exactly as on stable. | Per-PR CI runs the full test suite under both MSRV and stable; release pipeline additionally rebuilds the published crate from a clean cache on MSRV after upload, ensuring the published artifact remains buildable for the documented MSRV window. | 10 |
| SC-061 | Canary set never appears in any log file or telemetry row across the full test suite. | CI step (1) feeds canary strings through every component endpoint and tool, (2) captures stdout/stderr from every process and dumps telemetry_event_raw via SQL, (3) greps for each canary string. Any non-zero match count fails the build with the offending file and line. | 11 |
| SC-062 | Every cloud server log line is valid JSON that parses against a documented schema. | CI runs the full suite, captures all server logs, parses each line as JSON, validates against a JSON Schema for log lines. Any parse or schema failure fails the build. | 11 |
| SC-063 | request_id appears in 100% of log lines for traffic that included a request id, in every component touching that request. | Trace test: end-to-end script makes a CLI call (CLI generates request_id), MCP server forwards to cloud, cloud responds. After the call, grep all logs for the request_id. Assert presence in at least one line of CLI, MCP, and cloud logs respectively. | 11 |
| SC-064 | Opt-out is honored: with telemetry disabled, the server receives zero events from the opted-out client. | Test sets MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1; runs 100 MCP tool calls; asserts the server-side telemetry_event_raw count attributable to that client (by IP) is exactly zero. Test repeated for the config-based and CLI-based opt-out mechanisms. | 11 |
| SC-065 | Telemetry raw rows older than the retention window are deleted within one sweep cycle. | Test inserts 10 telemetry_event_raw rows with received_at older than retention; runs one sweep cycle; asserts all 10 are gone and the equivalent aggregate counters remain incremented. | 11,9 |
| SC-066 | /metrics output is a valid Prometheus exposition that scrapes cleanly with promtool. | CI runs 'promtool check metrics' against the /metrics output across a representative traffic mix; non-zero exit fails the build. Snapshot test asserts the documented series names are present. | 11 |

---

## Appendix: Story Revision History

*Major revisions to graduated stories. Full details in `archive/REVISIONS.md`*

| Date | Story | Change | Reason |
|------|-------|--------|--------|
| 2026-05-13 | Story 4 | Additive: response gains `trust_score`, `confidence`, `confidence_factors`; request gains `sort_by`, `min_confidence`; `search_metadata` gains `filtered_by_confidence`, `sort_by` | Story 6 graduation — D24 |
| 2026-05-13 | Story 5 | Additive: MCP search result gains `trust_score`, `confidence`, `confidence_factors`; MCP server substitutes reranker score for relevance when rerank=true | Story 6 graduation — D24 |

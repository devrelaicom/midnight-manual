# MCP Tool Surface v2 — Design

**Date**: 2026-06-10
**Status**: Approved (pending spec review)
**Scope**: Extends PR #79 (`mcp-response-format`). Spans `mn-mcp`, `mn-server`, `mn-store`, `mn-skills`, `mn-cli`, `mn-telemetry`, and the `specs/001-rag-platform/contracts/` files.
**Predecessor**: `docs/superpowers/specs/2026-06-09-mcp-tool-response-format-design.md` (Phases 1–3, already implemented on this branch).

## Goals

1. Tool descriptions that tell a calling agent *what the tool does and when to use it* — nothing else. No repo file paths (callers don't have our repo), no field inventories (outputSchema carries those), no search-method tutorials.
2. Responses that are efficient for modern clients (`structuredContent`) while remaining self-sufficient for legacy clients that only read `content[0].text` — without duplicating everything into both.
3. A simpler tool surface: the 90% case must be trivially callable.
4. Standard MCP tool annotations on every tool.

**Compatibility stance**: pre-1.0, no published clients — tool renames/removals are free. "Backwards compatible" here means exactly one thing: `content[0].text` must carry the most important information for text-only clients, while avoiding wholesale duplication with `structuredContent`.

## 1. Tool surface (14 → 13 tools)

| Tool | Change |
|---|---|
| `search` | Slimmed to `{query (required string), mode?, limit?}`. No oneOf, no filters, no rerank flag (rerank on by default internally). |
| `advanced_search` | **New.** `{queries: [string] (1–10, required), mode?, limit?, rerank?, filters?}`. One query = one-element array; no oneOf. Full filter surface. Description defers to the `midnight-advanced-search` skill for patterns. |
| `get_chunks` | **Replaces `get_chunk`.** `{ids: [uuid] (1–20)}`. Backed by new cloud batch endpoint. |
| `get_chunk_next` / `get_chunk_prev` / `get_chunk_neighbors` | Structurally unchanged; descriptions + summaries rewritten. |
| `get_chunk_parents` | Kept; response enriched (§3.5). |
| `get_document` | **= renamed `get_document_full`**; old `get_document` removed. Returns document metadata + chunk *skeleton* array `[{id, chunk_index, token_count}]` — no bodies. The 500-chunk cap and the `TOO_MANY_CHUNKS` error envelope are deleted (a skeleton is cheap at any size). |
| `get_document_chunks` | Kept: the one way to fetch chunk bodies for a document, windowed (`from`/`limit`). |
| `list_sources` | Gains cursor pagination + filters (§3.2). |
| `facets` | Drill-down shape (§3.3). |
| `pull_models` | **Removed.** Reranker loads lazily on first reranked search; `status` reports its state. Deleting it also deletes the legacy bge-singleton path (`tools.rs:505-542`) and closes the Task 9.4 wrong-model bug by removal. |
| `status` | Reworked (§4). |
| `install_search_skill` | Kept; response additions (§5). |

### Descriptions

Every tool description is rewritten to 1–3 sentences: what it does + when to use it. Mechanical constraints (caps, defaults, ranges) move to per-property descriptions inside the input schema. Indicative drafts (final wording at implementation):

- `search`: "Search the Midnight Network documentation and code corpus. Returns ranked excerpts (chunks) with confidence scores and source attribution. Use whenever you need facts about Midnight, Compact, or the SDK. For multi-query strategies, filters, or rerank control, use `advanced_search`."
- `advanced_search`: "Full-control corpus search: multi-query fusion (HyDE / expansion / step-back), per-facet filters, mode and rerank control. Use when basic `search` results are insufficient or when the `midnight-advanced-search` skill prescribes a pattern. Call `facets` to discover valid filter values."
- `get_chunks`: "Fetch the full content of one or more chunks by id (typically ids returned by a search). Use after a search to read the actual text behind a result."
- `get_document`: "Fetch a document's metadata and an ordered skeleton of its chunks (ids, positions, token counts — no bodies). Use to size up a document before reading it with `get_document_chunks`."
- `status`: "Diagnose connectivity, auth, and model readiness — cloud reachability, who you're authenticated as, rate-limit state, VoyageAI key validity, reranker state. Call when searches fail or before a long session."

### Annotations

Added to every tool in `tools/list`:

| Tools | readOnlyHint | destructiveHint | idempotentHint | openWorldHint |
|---|---|---|---|---|
| `search`, `advanced_search`, `get_chunks`, `get_chunk_next`, `get_chunk_prev`, `get_chunk_neighbors`, `get_chunk_parents`, `get_document`, `get_document_chunks`, `list_sources`, `facets`, `status` | `true` | — | — | `false` (closed corpus / known endpoints) |
| `install_search_skill` | `false` | `false` (only writes/updates its own skill directory) | `true` | `false` |

## 2. Response format rules

### 2.1 `suggested_next_actions` (renamed from `next_actions`, everywhere — including error envelopes)

Entry shape: `{description, tool?, arguments?}`.

- `description`: a human-written sentence stating what the action achieves ("Fetch the full content of the top-ranked chunk").
- `tool` is **optional** so entries can describe user actions that aren't tool calls (e.g. install_search_skill's reload step).

Per-tool actions:

| Tool | suggested_next_actions |
|---|---|
| `search` / `advanced_search` | (1) "Fetch the top-ranked chunk's full content" → `get_chunks {ids: [top]}`; (2) "Fetch the top 5 ranked chunks in one call" → `get_chunks {ids: [top 5]}`; (3) "Read the surrounding chunks of the top result for more context" → `get_chunk_neighbors {id: top}`; (4) "Get the top result's parent document overview" → `get_document {id}`; (5) conditional skill nudge (§2.3). |
| `get_chunks` | Neighbors of first chunk; "Fetch the parent document overview" → `get_document`. |
| `get_chunk_next` / `get_chunk_prev` | Continue paging (keyed to last/first returned chunk) + parent document. |
| `get_chunk_neighbors` | Parent document. |
| `get_chunk_parents` | "Fetch the containing document" → `get_document {document_id}`. (Only the document-kind node maps to a document; group/root nodes don't — so this is one action, not one per parent.) |
| `get_document` | → `get_document_chunks {id, from: 0}`. |
| `get_document_chunks` | Next window (when more exist) + document overview. |
| `list_sources` / `facets` | Next page (cursor) + a **concrete, valid** example using real values from this response: "Restrict a search to this source" → `advanced_search {queries: ["…"], filters: {source: {any_of: ["<actual-slug>"]}}}`. Never placeholder-only examples. |
| `status` | Contextual — e.g. invalid Voyage key → user-action entry to fix the key; unauthenticated → login pointer. |
| `install_search_skill` | User-action entries carrying each harness's `reload_step` ("Ask the user to restart Claude Code / refresh skills…"). |

### 2.2 `content[0].text` (summary line + one ```json fence, per PR #79 pattern)

| Tool | Text content |
|---|---|
| `search` / `advanced_search` | `"N matches (M candidates). Top: {source_path} › {heading} [{attribution} · {confidence}]"` + trimmed results fence. **`corpus <model>` is removed from the summary.** Low-candidate nudge line when §2.3 fires. |
| `get_chunks`, 1 id | Fence contains the **full chunk content** (the legacy-client payload requirement). Modern clients see the content twice in this case; accepted cost. |
| `get_chunks` >1 id; `get_chunk_next/prev/neighbors` | Summary + per-chunk `{id, source_path, heading_path, snippet}` where snippet = first ~150 chars of content. |
| `get_document_chunks` | Snippets in text; full bodies only in `structuredContent`. **Flagged tradeoff (accepted)**: text-only legacy clients cannot read full bodies via this tool; their full-content path is `get_chunks`. |
| `get_document` | `"{source_path} ({source_display_name}): N chunks, ~T tokens."` + metadata fence; skeleton truncated to first 50 entries in the fence (full skeleton in `structuredContent`). |
| `get_chunk_parents` | `"N ancestors of chunk {id}:"` then per node `name (kind) — id`, root last, plus a source line. |
| `list_sources` | `"Showing 1–20 of 43 sources."` + fence of `[{id, display_name, kind}]` only. |
| `facets` (overview) | `"7 filter dimensions: …. Values feed advanced_search filters."` + compact fence. |
| `facets` (drill-down) | `"tags: showing 1–50 of 312 values."` + fence. |
| `status` | e.g. `"Cloud reachable (v0.9.2); auth: github_oauth aaronbassett (read); requests 58/60; embed tokens 86k/100k hr · 940k/1M day; Voyage key valid; reranker bge-reranker-base loaded."` |
| `install_search_skill` | Outcome summary; ends with the explicit instruction: "Ask the user to refresh/restart their harness's skills to activate." |

### 2.3 Low-candidate skill nudge

When `total_candidates < 5` **and** the `midnight-advanced-search` skill is not detected locally (mn-skills harness probe, cached per process): append a text line — "Few candidates matched — the midnight-advanced-search skill teaches recovery patterns (install_search_skill)." — and a corresponding `suggested_next_actions` entry. Suppressed when the skill is already installed.

### 2.4 `matched_queries`

Cloud response unchanged (it's the 0-based indices of input queries that contributed a rank to the result — `mn-server/src/routes/search.rs:573`). The MCP projector **omits it from basic `search`** (single query → always `[0]`, pure noise) and keeps it for `advanced_search`, where the outputSchema describes it: "indices into your `queries` array showing which queries ranked this result."

### 2.5 `version_match_multiplier` (documentation only, no behavior change)

The `advanced_search` outputSchema describes it: "Trust multiplier from comparing your `filters.language_target[].version_satisfies` constraint against the chunk's ingest-declared language targets: 1.15 satisfied / 1.00 no constraint or no matching target / 0.70 declared but unsatisfied." (Computation: `mn-core/src/scoring.rs:171-204`; policy defaults `scoring_policy.rs:130-134`.) Basic `search` has no filters, so the factor is always neutral there.

## 3. Cloud API changes (`mn-server` / `mn-store` / contracts)

1. **Batch chunks — `GET /v1/chunks?ids=a,b,c`** (cap 20). Returns `{chunks: [ChunkWithContext]}` preserving request order; unknown ids land in `missing: [id]` instead of failing the call. Costs 1 rate-limit token. Store: single `WHERE id = ANY($1)`.
2. **Sources pagination — `GET /v1/sources`** gains `cursor`, `limit` (default 20, max 100), `created_after`, `created_before`, `kind`, `retired` (default `false` → active only). Keyset cursor on `(created_at, id)`, opaque base64 token. Response: `{sources, total, next_cursor?}`.
3. **Facets drill-down — `GET /v1/facets`** gains optional `facet`, `cursor`, `limit`. No `facet` → overview (per-dimension samples trimmed to 10 + `total`); with `facet` → paginated value list for that dimension (same keyset pattern).
4. **`GET /v1/me`** — auth introspection: `{authenticated, auth_type (github_oauth | admin | anonymous), identity, permission_level, rate_limit, token_limits, server_version}`. Callers have **two independent limit systems**, and `/v1/me` reports both:
   - `rate_limit` — the request rate limit (req/s token bucket, one bucket per caller): `{tier, limit, remaining, reset_secs}`. Exposes the state the rate-limit middleware already computes for `X-RateLimit-*` headers, peeked without spending a token (`charge(key, limit, 0)`).
   - `token_limits` — the embedding token budget (`tokenlimit.rs`, charged by `POST /v1/embeddings`, surfaced today only via `x-tokenlimit-*` headers): `{tier, hourly: {limit, remaining, reset_at_secs}, daily: {limit, remaining, reset_at_secs}}`, from the limiter's non-consuming `snapshot_for` (`tokenlimit.rs:484`).
5. **Parents enrichment — `GET /v1/chunks/:id/parents`**: each node gains `document_id` (non-null only for `kind: document` nodes, joined from documents) and the response gains top-level `source: {slug, display_name}` from the source version. Node ids remain the structural hierarchy ids; the document node now carries the fetchable document id.

`openapi.yaml` and `mcp-tools.json` updated for all of the above.

## 4. `status` (MCP tool + shared assembler)

Assembles in parallel, ~3s timeout per probe, into a `StatusReport`:

- **Cloud health**: `/readyz` → `reachable | degraded | unreachable` + server version.
- **Auth + limits**: `GET /v1/me`; `anonymous` when no token. Carries both the request rate-limit bucket and the embedding token-limit windows (hourly + daily).
- **VoyageAI**: if `VOYAGE_API_KEY` set → `GET https://api.voyageai.com/v1/files` → `valid | invalid_key | unreachable`; else `not_configured`.
- **Reranker**: configured reranker id, remote/local, load state if local.

## 5. `install_search_skill`

- Response gains a `detected` list (harnesses found), complementing the existing `not_detected`; `installed` stays as the per-harness outcome detail.
- `reload_step` per harness moves into `suggested_next_actions` user-action entries; text summary ends with the refresh-skills instruction (§2.2).

## 6. `mnm status` CLI command

- **Shared assembler**: the §4 `StatusReport` assembler lives in `mn-mcp`; `mn-cli` already depends on `mn-mcp` (same pattern as `mnm mcp serve`). MCP tool and CLI command are two renderers over one struct — they cannot drift.
- **Command**: `mnm status`, new top-level command (noun-first D19 compatible), no admin-visibility gating. Global `--json` emits the raw `StatusReport`; human output is sectioned (Cloud / Auth / VoyageAI / Reranker / Rate limits). Non-zero exit when the cloud is unreachable, so it's scriptable.
- **`mnm doctor`** stays the deep operator diagnostic; its human output gains a one-line pointer to `mnm status` instead of duplicating it.
- Registers a `CliCommandName::Status` telemetry variant like every other command.

## 7. Bundled skill content (`mn-skills`)

`midnight-advanced-search` SKILL.md is rewritten for the new surface: `search` → `advanced_search` for all filters/multi-query/rerank patterns; `get_chunk` → `get_chunks`; `get_document_full` → `get_document`; drop `pull_models`; document facets drill-down and sources pagination. `install_search_skill` updates installs in place, so previously-installed agents converge on next run — which is also why the low-candidate nudge fires `install_search_skill` (it updates, not just installs).

## 8. Telemetry

- `McpToolCall.tool_name` emits the new names (`advanced_search`, `get_chunks`, …) — additive values, no schema change. New tools wire through the same event path; search-projector telemetry (PR #79 Phase 3) feeds identically from `search` and `advanced_search`.
- Nothing new crosses FR-112: cursors, facet names, and counts never enter telemetry.

## 9. Testing

- **mn-mcp**: `tools/list` snapshots (13 tools, annotations present); outputSchema conformance (`result_shape.rs`) extended to new/renamed tools; dispatch tests — `get_chunks` (1 id → full content in text; >1 → snippets; >20 → InvalidParams), facets drill-down, sources cursors, search nudge (fires at <5 candidates + skill absent; suppressed when installed); `suggested_next_actions` rename asserted everywhere including error envelopes.
- **mn-server**: batch chunks (order preserved, `missing` populated), keyset pagination (stability across pages, filter composition), `/v1/me` per auth type, parents enrichment (`document_id` only on document nodes).
- **mn-cli**: `mnm status` rendering (json + human) over a canned `StatusReport`; exit-code behavior.
- **Integration (CI-only)**: pagination + batch against real Postgres.
- **Canary**: new telemetry values stay within enum/count vocabulary.

## 10. Out of scope / riding along

- `McpToolCall.model_state` hard-coded `Missing` — pre-existing, still out of scope (flagged in PR #79).
- Deleted with this work: `pull_models` + legacy bge singleton path; old `get_document`; `TOO_MANY_CHUNKS` envelope + 500-chunk cap.

## Open decisions log (resolved during brainstorming)

| Decision | Outcome |
|---|---|
| Back-compat meaning | Text-block self-sufficiency only; breaking changes free (pre-1.0). |
| Search split | Two tools: `search` (query/mode/limit) + `advanced_search` (full surface). |
| Batch chunk shape | Single `get_chunks` array-only tool replacing `get_chunk`; cloud batch endpoint. |
| `pull_models` | Removed (lazy load + `status` reporting suffice; first-search cold-load latency accepted). |
| Facets pagination | Drill-down param on one tool (overview vs per-facet paginated values). |
| Branch strategy | Extend PR #79 on `mcp-response-format`. |
| CLI status | `mnm status` over the shared `StatusReport` assembler. |

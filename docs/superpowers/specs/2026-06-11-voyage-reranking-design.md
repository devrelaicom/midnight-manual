# VoyageAI Reranking — Design

**Date**: 2026-06-11
**Status**: Approved for planning
**Supersedes**: the local fastembed/ONNX reranker subsystem

## Summary

Reranking moves to VoyageAI (`rerank-2.5` / `rerank-2.5-lite`) everywhere, with three
placements: **server-side** (inline in `POST /v1/search`, server's Voyage key, charged to
user + global token budgets), **local BYOK** (client calls Voyage directly with the user's
own key), or **off**. Placement defaults follow the embedding pattern: `VOYAGE_API_KEY`
present → local; absent → server. Default model is `rerank-2.5` in both placements;
`rerank-2.5-lite` is selectable and charged at half tokens (rounded up) server-side.
Voyage's instruction-following is exposed via derived default instructions plus
agent-supplied overrides, with instruction-writing guidance added to the bundled
`midnight-advanced-search` skill. The entire local ONNX/fastembed reranker catalog is
deleted.

## Background

### Current state

- Server (`mn-server/src/routes/search.rs`) performs RRF fusion (k=60) and
  trust × relevance confidence scoring only; no reranking (`search.rs:8`).
- CLI (`mn-cli/src/commands/search.rs`) has opt-in `--rerank` (default **off**):
  over-fetches 50 candidates (`RERANK_FETCH`), reranks locally.
- MCP (`mn-mcp/src/tools.rs`) `advanced_search` has `rerank: bool` (default **true**),
  same local path; lazy `OnceCell` reranker singleton.
- Reranker catalog (`mn-embedding/src/reranker_catalog.rs`): fastembed-native
  (`bge-reranker-base` default, `bge-reranker-v2-m3`, `jina-reranker-v1-turbo-en`),
  HuggingFace ONNX (`ms-marco-*`, `mxbai-*`), `custom` local path, and Voyage API entries
  (`voyage-rerank-2.5`, `voyage-rerank-2.5-lite`, `voyage-rerank-2`).
- `VoyageReranker` (`mn-embedding/src/voyage.rs`) already implements `POST /v1/rerank`
  (http1-only, 30s timeout, returns `total_tokens`).
- Two independent server limit systems: request-rate token bucket (req/s) and embedding
  token budget (rolling hourly/daily windows per subject + global cap), charged from
  Voyage-reported `total_tokens`.

### Voyage facts that constrain the design

- Instructions are **not** an API parameter — they are natural-language text appended to
  the `query` string.
- Token accounting: `(query_tokens × num_documents) + sum(document_tokens)`. Instruction
  length is multiplied by candidate-pool size.
- Limits (rerank-2.5/-lite): 8K query tokens, 1,000 documents, 600K aggregate tokens per
  request, 32K tokens per query+single-document pair. None binding at our pool sizes.
- `relevance_score` is already in [0, 1]. The existing `normalize_rerank` sigmoid is
  correct for bge logits but compresses Voyage scores into ~[0.5, 0.73] — a live bug in
  the current Voyage catalog path, fixed by this design.
- `rerank-2.5-lite` is priced at half the per-token rate of `rerank-2.5`, which grounds
  the 50% billed-token discount.

## Decisions (from brainstorming)

| # | Decision | Choice |
|---|----------|--------|
| D1 | Local reranking surface | **Voyage only.** All in-process ONNX/fastembed reranking deleted. No documented base-URL escape hatch (`MIDNIGHT_MANUAL_VOYAGE_BASE_URL` stays undocumented/dev-only). There is no OpenAI rerank standard; self-hosters can later target the Cohere/Voyage `/rerank` shape if demand appears (out of scope). |
| D2 | Server rerank placement | **Inline in `POST /v1/search`** (not a `/v1/rerank` proxy endpoint). One round trip; candidate pool never crosses the wire; raw API consumers get reranked results with zero orchestration. |
| D3 | Budget exhaustion | **Degrade + flag.** Skip rerank, return RRF order, set `search_metadata.rerank.reason`. Search never hard-fails on rerank cost. |
| D4 | Instruction composition | **Agent replaces default.** Derived defaults apply only when no agent instruction is supplied. |
| D5 | Lite discount | Server charges `ceil(total_tokens / 2)` for `rerank-2.5-lite`. Model choice has no effect on request rate limits. |
| D6 | Placement default | Explicit config/flag > auto. Auto: `VOYAGE_API_KEY` present → local BYOK; absent → server. |

## 1. Server: rerank inside `POST /v1/search`

### Request

`SearchRequest` gains:

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `rerank` | `"rerank-2.5" \| "rerank-2.5-lite" \| "none"` | `"rerank-2.5"` | Omitted ⇒ default model. |
| `rerank_instructions` | `string`, optional | — | Hard cap **400 chars**. Over-cap ⇒ HTTP 400 with explanatory message (no silent truncation). Ignored when `rerank: "none"`. |

### Pipeline

When `rerank != "none"`:

1. RRF fusion produces a candidate pool of `max(limit, 50)` results (instead of
   truncating to `limit`).
2. Resolve the rerank query: first query text + instruction (agent-supplied, else derived
   default per §3), instruction **appended** to the query (Voyage's documented
   convention). Multi-query requests rerank against the **first** query (matches current
   client behavior).
3. Pre-gate: estimate cost (`chars / 4` heuristic over query×docs + doc text) against the
   user's remaining token budget and the global cap. Insufficient ⇒ degrade (skip Voyage
   call entirely).
4. Call Voyage `POST /v1/rerank` via the existing `VoyageReranker` client (server's key,
   http1-only, 30s timeout, `truncation: true`). Documents = chunk content as stored
   (same text the client rerank path uses today).
5. Charge actual billed-equivalent tokens (§2) to the user windows + global cap
   (gate-then-charge, consistent with the embeddings proxy; boundary overdraw allowed as
   embeddings allows today).
6. Recompute confidence: `confidence = trust × relevance_score` with
   `relevance_source: "rerank"`. Voyage scores are used **directly** (already 0–1; no
   sigmoid).
7. Re-sort by confidence (or requested `sort_by`), apply `min_confidence`, truncate to
   `limit`.

Voyage error or timeout ⇒ degrade to RRF order + flag. A flaky upstream never fails
search.

### Response metadata

Every search response gains:

```json
"search_metadata": {
  "rerank": {
    "applied": true,
    "model": "rerank-2.5",
    "reason": null
  }
}
```

`applied: false` carries `reason`:
`"not_requested" | "token_budget_exhausted" | "provider_error" | "disabled"`.
`model` is present only when a rerank was attempted.

### Ops kill switch

Server env flag `MIDNIGHT_MANUAL_SERVER_RERANK` (`on`/`off`, default `on`) disables all
server-side reranking ⇒ degrade + `reason: "disabled"`. Incident-response lever; no
deploy needed.

## 2. Token accounting

- **Billed-equivalent tokens** (name the concept in code):
  `rerank-2.5` ⇒ `total_tokens`; `rerank-2.5-lite` ⇒ `ceil(total_tokens / 2)`.
- Deducted from the **same** rolling hourly/daily windows the embeddings proxy charges
  (per-subject: anon IP / SSO user / admin), plus the global cap. The budget is now a
  general "Voyage token budget", not embedding-specific; `/v1/me` shape is unchanged.
- **No effect on the request-rate token bucket** in either model.
- Local BYOK reranking deducts nothing — the server never sees it.

## 3. Default instructions

A single shared function in `mn-core` (alongside the scoring policy) derives the default
instruction from request shape. The server and locally-reranking clients call the same
function, so identical searches rerank identically regardless of placement.

v1 rule table (deliberately minimal — every default token is multiplied by ~50 docs per
search; grow the table against real query data later):

| Condition | Instruction |
|-----------|-------------|
| `code_mode: "exclusive"` | "Prioritize chunks containing code examples, function signatures, and API usage over prose." |
| version facet filter present | "Prefer content applying to version {X}; deprioritize other versions." |
| otherwise | none (bare query) |

If both conditions hold, the two sentences concatenate (they are non-contradictory by
construction). Agent-supplied instructions replace the derived default wholesale (D4).

## 4. Clients (CLI + MCP)

### Placement resolution (shared)

`explicit flag/config > auto`. Auto: `VOYAGE_API_KEY` (or config key) present → **local
BYOK**; absent → **server**.

- **Local path**: send `rerank: "none"` to the server, over-fetch 50 candidates, call
  Voyage directly with the user's key, apply the same instruction derivation (§3) unless
  an explicit instruction is given, use scores directly (no sigmoid), recompute
  confidence, sort, truncate, stamp `rerank_score`. Exactly one rerank pass per search is
  structurally guaranteed: local placement always sends `rerank: "none"`.
- **Server path**: send `rerank: "<model>"` (+ instructions if given); display
  `search_metadata.rerank` outcomes (e.g. note when degraded).
- **Off**: send `rerank: "none"`, no local rerank, no over-fetch.

### CLI (`mn-cli`)

| Surface | Shape | Default |
|---------|-------|---------|
| `--rerank <auto\|local\|server\|off>` | replaces today's bool flag | `auto` |
| `--rerank-model <rerank-2.5\|rerank-2.5-lite>` | model in either placement | `rerank-2.5` |
| `--rerank-instructions <text>` | explicit instruction (≤400 chars, validated client-side) | — |
| Config `[rerank]` | `location`, `model` | — |
| Env | `MIDNIGHT_MANUAL_RERANK`, `MIDNIGHT_MANUAL_RERANK_MODEL` | — |

Precedence: flag > env > config > auto. **Removed**: `--reranker`, `--reranker-path`,
`MIDNIGHT_MANUAL_RERANKER`, config `[models].reranker` / `reranker_path` (pre-1.0 hard
cutover, no aliases).

**Behavior change**: CLI search is reranked **by default** (was opt-in off). Cost: every
default search gains ~100–300ms (inside the single server request for keyless users; an
extra client→Voyage call for BYOK users), and keyless users' searches consume rerank
tokens from their server budget by default. `--rerank off` opts out.

### MCP (`mn-mcp`)

Agents don't choose placement or model — those are deployment concerns resolved from
env/config exactly as the CLI does.

- `advanced_search`: keeps `rerank: bool` (default `true`); gains
  `rerank_instructions: string` (≤400 chars). `rerank: true` + resolution = wherever
  config says; `rerank: false` ⇒ off for this call.
- `search` (simple tool): **unchanged** — no new parameters; config defaults govern.
- The `OnceCell<LoadedReranker>` lazy-load machinery is replaced by the stateless Voyage
  HTTP client (nothing heavy left to lazy-load).

## 5. Deletions

- `mn-embedding/src/reranker_catalog.rs` — entire fastembed/ONNX catalog (incl.
  `voyage-rerank-2` legacy entry).
- ONNX loading paths in `mn-embedding/src/reranker.rs`; `LoadedReranker` collapses to the
  Voyage client type.
- `models pull` reranker download path (`mn-cli/src/commands/models.rs`).
- `normalize_rerank` sigmoid in `mn-core/src/scoring.rs` (Voyage scores are 0–1; this
  also fixes the existing Voyage-path score-compression bug).
- The `fastembed` dependency itself, **pending verification** that no embedding path
  still uses it (embeddings moved to Voyage; planning task: grep + remove).

## 6. Contracts, privacy, telemetry, skill

- **Contracts**: update `specs/001-rag-platform/contracts/openapi.yaml` (search
  request/response: `rerank`, `rerank_instructions`, `search_metadata.rerank`) and
  `mcp-tools.json` (`advanced_search` schema). Contract tests enforce both surfaces and
  must be updated in the same change.
- **Privacy (README "Telemetry & Privacy")**: state that server-side reranking sends
  result chunk text and the query to VoyageAI — same third-party exposure class as the
  embeddings proxy, now explicit.
- **Telemetry** (FR-109 schema style, three-mechanism opt-out honored): one event per
  rerank decision: `{placement: server|local, model, billed_tokens, applied, reason}`.
- **Skill** (`mn-skills/assets/midnight-advanced-search/SKILL.md`): new
  instruction-writing section covering: the three instruction archetypes (query emphasis,
  document filtering, contextual disambiguation); terse-is-cheaper (instruction tokens ×
  candidate-pool multiplication, 400-char cap); corpus-specific good/bad examples; when
  to omit instructions and trust the derived defaults; that `rerank: false` exists for
  cheap exploratory queries.

## 7. Testing

- **Unit**: instruction derivation table (incl. both-conditions concatenation),
  billed-equivalent math (`ceil` halving), all four degrade reasons, 400-char cap
  rejection, placement resolution precedence.
- **Contract**: openapi.yaml + mcp-tools.json round-trips.
- **Integration** (CI; sandbox has no Docker): server rerank against a mocked Voyage
  `/v1/rerank` endpoint — happy path, budget-gate degrade, provider-error degrade, charge
  assertion against the windows.
- **Client tests**: BYOK-path tests must run with `VOYAGE_API_KEY=` cleared (sandbox sets
  it globally).
- **Canary**: no new privacy invariants expected, but confirm rerank telemetry events
  pass the existing canary assertions.

## Out of scope

- Cohere-shape adapter for self-hosted rerankers (TEI/vLLM) — revisit on demand.
- Rerank result caching.
- A `/v1/rerank` proxy endpoint for arbitrary documents.
- Per-facet instruction rules beyond the v1 table.

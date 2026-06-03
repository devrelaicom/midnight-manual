# Design — VoyageAI embeddings, embedding token limits, and a configurable reranker

- **Date**: 2026-06-02
- **Status**: Approved (design); pending implementation plan
- **Author**: Aaron Bassett (with Claude Code)
- **Feature branch (current)**: `feat/midnight-ingestion-manifests`
- **Scope decision**: all-in-one — one design + one phased implementation plan

## 1. Problem & motivation

Manual testing of the corpus (after `scripts/ingest-midnight.sh` loaded a handful of example repos) produced poor search results. The dominant cause is the **local embedding model** (`fastembed` `bge-base-en-v1.5`, 768-dim): cheap to run locally, but the retrieval quality on Midnight/Compact code and docs is not worth it.

This design:

1. **Switches the corpus embedding model to VoyageAI `voyage-code-3`** (a code-specialised model), keeping the existing *client-side-embedding* architecture.
2. **Adds tiered, per-subject token limits** for embeddings the server performs on a user's behalf (abuse control on real Voyage spend), overridable by an admin CLI — mirroring the existing rate-limit tiers/overrides.
3. **Adds a `POST /v1/embeddings` endpoint** that returns the embedding data, the model used, tokens consumed, and tokens remaining (hourly + daily).
4. **Supports BYOK** — with `VOYAGE_API_KEY` (env) or `--voyage-api-key` (flag), `mnm` embeds **client-side directly against Voyage**, bypassing the server token limits.
5. **Adds an admin model-migration command** (`mnm models migrate`) that re-ingests sources onto a new model, provenance-ordered (the `voyage-code-3 → voyage-code-4` story).
6. **Makes the reranker configurable** — 8 curated local models, a custom-ONNX path, and the Voyage reranker (BYOK), since reranking is always client-side.

### Goals

- Materially better retrieval quality via `voyage-code-3`.
- Cost control on server-performed embeddings without billing-grade precision (soft caps; Voyage's 200M free tokens/month gives large headroom).
- A model-migration path that reuses the existing ingest pipeline and atomic `is_active` swap, ordered by source trust.
- Configurable, laptop-friendly reranking with an escape hatch to a custom model and to Voyage's hosted reranker.

### Non-goals (explicit)

- Multi-dimension / multi-model **coexistence** in the live corpus, and **zero-gap** cross-dimension migration (dual columns / per-model embedding table).
- **Durable/exact** (Postgres-per-charge) token accounting, sliding-log accounting, or cross-instance shared counters.
- `int8` / `binary` / `halfvec` embedding **storage** optimisation.
- **Server-side reranking.**
- Auto-converting arbitrary HuggingFace rerankers to ONNX (we ship a curated set + a custom-path only).

## 2. Findings about the current system (verified by source inspection)

These shaped the design and answer questions raised during brainstorming.

- **The model name *is* stored next to each chunk.** `chunk.embedding_model_id` is a `NOT NULL` FK into an `embedding_model` registry (`name`, `revision`, `dim`, `provider`, `UNIQUE(name, revision)`), with the wire format `{name}@{revision}` already implemented in `mn-core/src/model_id.rs` (e.g. `bge-base-en-v1.5@1`). A trigger enforces that **all chunks in a `source_version` share one model**.
- **The active model is pinned per immutable `source_version`** — there is **no** global "active" flag on `embedding_model`. `source_version.is_active` (boolean; a unique partial index allows one active version per source; a CHECK ties `is_active = true` to `status = 'active'`) is the mechanism. `embedding_model::get_active()` (in `mn-store`) joins active versions and returns one model row; `mn-server` resolves it **at boot** into `ServerConfig.corpus_model` (`name@revision`) and the `/v1/search` handler 409s on `client_embedding_model != corpus_model`. Search filters `sv.is_active = true`. The corpus migrates a source by **ingesting a new version and atomically promoting it** (`is_active` swap), not by mutating chunks. The 409 remediation already points at `mnm models pull`.
- **Reranking is always client-side.** `mn-server` never reranks — it does pgvector + FTS + RRF (k=60) + confidence scoring and returns candidates. Reranking happens **only in the MCP client** (`mn-mcp`), lazily loading `bge-reranker-base`; the plain CLI `mnm search` skips reranking for latency.
- **Embedding is already client-side.** Both CLI and MCP embed locally and POST `{text, vector}` + `client_embedding_model` to `POST /v1/search`; the server validates the model (409 on mismatch) and the vector dimension (currently hard-coded `768`). The server never embeds for search.
- **The embedding column is fixed-dimension.** `chunk.embedding vector(768)` with an HNSW index (`vector_cosine_ops`, m=16, ef_construction=64) bound to it; GIN index on the generated `tsvector`.
- **Provenance / attribution.** `Attribution` (in `mn-core/src/provenance.rs`) is `Foundation, Partner, ThirdParty, Community, Unknown` (snake_case on the wire), with trust multipliers `1.00 / 0.85 / 0.60 / 0.40 / 0.30` (`scoring_policy.rs`). Provenance lives in the `document.provenance` **JSONB** column (no dedicated column); `trust_score` is **computed in-app, not stored**.
- **Rate limiting is in-memory per-second token buckets** keyed by IP (anon) / JWT `sub` (uplift, admin), with CIDR overrides stored in `rate_limit_override` (Postgres) refreshed to memory ~every 30s. Tiers: anon 10 rps, uplift 60 rps, admin 1000 rps. There is **no** hourly/daily accounting today.
- **`fastembed` 4.9.1** ships **4 native rerankers** and a **`TextRerank::try_new_from_user_defined(UserDefinedRerankingModel { onnx_source, tokenizer_files }, …)`** path for custom ONNX (and the equivalent for embedders). Native rerankers: `BGERerankerBase` (default, EN+ZH, MIT), `BGERerankerV2M3` (multilingual, via the `rozgo/bge-reranker-v2-m3` fork, Apache), `JINARerankerV1TurboEn` (English, Apache), `JINARerankerV2BaseMultiligual` (cc-by-nc-4.0 — non-commercial).

### Key consequence: BYOK means "your Voyage key," not "a local model"

Because the corpus is embedded with one model and search requires the query vector to match it, a *local* model would produce vectors that can't match a Voyage corpus. So **BYOK = embed against your own Voyage account** (same model), not "fall back to a local embedder." The local fastembed **embedder retires from the corpus path**; fastembed remains only for **local reranking**.

## 3. VoyageAI facts used by this design (verified against docs.voyageai.com)

- `voyage-code-3` output dims: default **1024**, options 256/512/2048. Output dtypes: `float` (default), `int8`/`uint8`, `binary`/`ubinary`. Max input **32,000 tokens**; max **1,000 texts / 120K tokens per request**.
- `input_type`: `null`/`query`/`document`. Setting it **changes** the embedding (a prompt is prepended), but with/without are stated mutually compatible. We use `document` for chunks, `query` for queries — a free quality lever.
- `POST https://api.voyageai.com/v1/embeddings`, `Authorization: Bearer $VOYAGE_API_KEY`. Response includes **`usage.total_tokens`** → we charge **actual** tokens after each call.
- Pricing: **$0.18 / 1M tokens**, **200M free tokens/month**. Voyage API rate limits (Tier 1): 3M TPM / 2000 RPM (scale with spend tier).
- Token counting: Voyage publishes a `voyage-code-3` tokenizer on HuggingFace (`voyageai/voyage-code-3`) and a Python `count_tokens`. A Rust pre-flight counter via the `tokenizers` crate is **likely but unconfirmed at the file-format level** — see the endpoint's fallback in §6.
- **No official Rust client** (the `voyageai` crate is an unofficial 0.0.1 stub) → use raw `reqwest`.
- Voyage rerankers: `rerank-2.5`, `rerank-2.5-lite`, `rerank-2`, `rerank-2-lite`. `POST /v1/rerank`, response includes `usage.total_tokens`; max 1000 docs/request. Pricing $0.05 / $0.02 per 1M, 200M free/month.
- **Privacy**: Voyage **retains and trains on inputs by default**; zero-retention requires an explicit opt-out (payment method + org admin + ToS acceptance). See §9.

Source URLs are listed in §12.

## 4. Embedding architecture & schema

### 4.1 Corpus goes Voyage-only

- Register `voyage-code-3@1` in `embedding_model` (`dim=1024`, `provider='voyageai'`).
- Encoding: `output_dimension=1024`, `output_dtype=float`; `input_type="document"` for chunks, `input_type="query"` for queries.
- `mn-embedding` gains a `VoyageEmbedder` (raw `reqwest`, `Bearer` auth). A thin `Embedder` enum `{ Voyage, Local }` provides the seam (no speculative impls); the local fastembed embedder is removed from the search/ingest paths but the crate retains fastembed for reranking.

### 4.2 Query flow (CLI + MCP) — downstream unchanged

1. If `--voyage-api-key` / `VOYAGE_API_KEY` is set → **mnm calls Voyage directly** (`input_type=query`); no server tokens spent.
2. Otherwise → **mnm POSTs query text to `POST /v1/embeddings`**; the server calls Voyage with its key, charges tokens, returns vectors.
3. Either way mnm then POSTs `{text, vector}` + `client_embedding_model="voyage-code-3@1"` to `POST /v1/search` — exactly as today.

### 4.3 Ingestion flow (admin)

Bulk ingest sets `VOYAGE_API_KEY` on the ingest host and embeds **directly via Voyage** (`input_type=document`, batched ≤1000 texts / ≤120K tokens/request). This sidesteps the server token caps for bulk work and removes server round-trips. The old `--enable-server-embedding` path is **removed**.

### 4.4 Active model & partial-migration filtering (reuse the existing mechanism)

There is **no** global active flag, and we don't add one. We keep `source_version.is_active` + `embedding_model::get_active()` + the boot-resolved `corpus_model`. Two small changes enable a partially-migrated corpus to behave safely:

- **Search filters `sv.embedding_model_id = <corpus_model_id>`** in addition to `sv.is_active = true`. Today that's redundant (all active versions share one model), but mid-migration it ensures only sources already on the target model are returned — preventing dimension-mismatch errors and excluding not-yet-migrated sources.
- **`corpus_model` is re-resolved on each successful ingest finalize** (and at boot), so promoting a re-ingested source flips the corpus model without a manual restart.

No `active` column, no document-level invariant, no `stale` chunk status, no bespoke reembed endpoints — the `source_version`-level model trigger stays intact.

### 4.5 Schema migration & the dimension change

- New migration: register `voyage-code-3@1`; re-type `chunk.embedding` → `vector(1024)`; drop & recreate the HNSW index; add the `sv.embedding_model_id` search filter; update `search.rs` to read the expected dim from `corpus_model`.
- A `vector(1024)` column **cannot hold the old 768-dim vectors**, so the re-type clears existing embeddings. Because the current corpus is throwaway test data, the **initial cutover is simply: migrate the schema → re-ingest fresh on `voyage-code-3`** (via `scripts/ingest-midnight.sh` once voyage is the registered/target model). No migration tool is needed for the initial switch.
- **General rule for future migrations:**
  - **Same-dimension** model swap (e.g. `voyage-code-3` → a future 1024-dim model): **zero-gap, incremental** — both models' chunks coexist in the column, and the `sv.embedding_model_id` filter serves the target model while sources are re-ingested one at a time.
  - **Dimension-changing** migration: requires re-typing the column → existing vectors cleared → a coverage gap until sources are re-ingested. Zero-gap cross-dimension migration (dual columns / per-model embedding table) stays out of scope (§1).

## 5. Model migration — `mnm models migrate`

Migration is **re-ingestion per source**, on the existing immutable-`source_version` + atomic-`is_active`-swap machinery. There is no in-place chunk mutation, no relaxed invariant, and no new active-model concept.

### 5.1 What it does

Enumerate sources whose **active** `source_version` is **not** on the target model, **ordered by provenance** — attribution rank `Foundation > Partner > ThirdParty > Community > Unknown` (a `CASE` on `document.provenance->>'attribution'`), then `verified`, then recency. (Because `trust_score` is computed in-app, not stored, the DB sort uses the attribution rank + tiebreakers rather than a stored score.) Restrict by `--source a,b,c` (source names). For each source, **re-ingest it on the target model** (clone from `source.origin_url` + its manifest → walk → chunk → embed → finalize/promote), reusing the existing ingest endpoints. The old version is retired by the promote.

### 5.2 Command (admin-only)

```
mnm models migrate --to <model@rev> [--source a,b,c] [--max-docs N] [--token-budget N]
```

- `--to` — the target model (defaults to the most recently registered model).
- `--token-budget` — a **client-side session** counter summing Voyage `usage.total_tokens` across **server-path and BYOK** embedding. It **never** bypasses the server rate limit (a server-path run can also stop on a `429`).
- `--max-docs` — a document budget for the run.
- **Granularity is source-at-a-time** (a consequence of re-ingestion): a `source_version` is promoted all-or-nothing, so limits are evaluated **at source boundaries**. If `--token-budget` / `--max-docs` is reached, or a `429` hits mid-source, that source's in-flight version is **aborted (not promoted)** and the run stops cleanly; already-migrated sources stay migrated.

### 5.3 New server surface (minimal)

Just a way to list sources by their active version's model: `GET /v1/admin/sources?not_model=<model@rev>` → sources still needing migration, **provenance-ordered**. The re-ingest itself reuses the existing `StartIngestRun` / chunk-upload / finalize endpoints — no bespoke reembed endpoints.

### 5.4 Retention & incomplete-migration visibility (no new prune command)

Migration produces superseded old-model versions, and these are **already collected** by the existing source-retention sweep (FR-063, `crates/mn-server/src/jobs/source_retention.rs`): `finalize()` demotes the previous active version to `inactive`; Phase 14 (`sweep_aged_inactive`) hard-deletes inactive/retired versions outside the source's `retention_count` and past the grace window, cascading to documents + chunks. This covers the *deleted-document* case — chunks for a doc removed upstream live only in the now-inactive old version, are never searched (`sv.is_active = true`), and are swept on the normal schedule. **No chunk-level prune command is added** (it would be redundant for inactive versions and dangerous for un-migrated active versions, which hold live content that needs migrating, not deleting).

The only lingering case is an **abandoned migration**: a source whose **active** version is still on the old model is hidden by the `sv.embedding_model_id = corpus_model` filter yet (correctly) never swept. To keep that from being silent, add **`mnm models status`** → lists sources whose active version `≠ corpus_model` (backed by `GET /v1/admin/sources?not_model=<model@rev>`, §5.3), resumable via `mnm models migrate`.

The dimension-changing column re-type clears all old-dim vectors (including inactive husks); the sweep reaps the husks on its normal schedule.

## 6. `POST /v1/embeddings` endpoint

```
POST /v1/embeddings   (bearer middleware: anon→per-IP, uplift→per-account, admin→per-account; anon allowed)

Request:
{
  "input": ["…", "…"],              // string or array; search = 1 short string
  "input_type": "query" | "document" // default "query"; ingest/migrate pass "document"
}
```

- The server owns `model` / `output_dimension` (1024) / `output_dtype` (float) from the active-model config; clients don't choose them. A `model` field, if present, is validated → 409 on mismatch.

**Flow:** resolve subject+tier → *(best-effort)* pre-count tokens with the bundled `voyage-code-3` tokenizer → check hourly + daily buckets **in-memory** → if over, **429 before calling Voyage** (no spend) → call Voyage → **charge actual `usage.total_tokens`** → respond.

- **Tokenizer fallback:** if the Rust tokenizer can't be loaded for `voyage-code-3`, gate on `remaining > 0` and charge actual — one boundary call may slightly overshoot, then subsequent calls reject.
- **Voyage error/timeout:** do **not** charge; pass through 502/504.
- Voyage per-request caps (≤1000 texts / ≤120K tokens) enforced → `413` if exceeded (clients batch).
- Both limiters apply: the existing **RPS** limit *and* the new **token** limit must pass.

```
Response 200:
{
  "model": "voyage-code-3@1",
  "embeddings": [[f32; 1024], …],
  "usage": { "total_tokens": 8 },
  "rate": {
    "hour": { "limit": 2000,  "remaining": 1992,  "reset_at": "<ISO8601>" },
    "day":  { "limit": 20000, "remaining": 19992, "reset_at": "<ISO8601>" }
  }
}

Response 429:
{ "error": "token_limit_exceeded", "window": "hour"|"day", "limit": …, "remaining": 0, "reset_at": … }  + Retry-After
```

Mirrored headers (like the existing `x-ratelimit-*`): `x-tokenlimit-hour-{limit,remaining,reset}` + day variants. (Returning *both* hourly and daily remaining is a deliberate extension of the original "hourly only" ask.)

## 7. Token-limit subsystem (bucketed)

### 7.1 Defaults per tier

| Tier | Subject key | Hourly | Daily (24h) |
|---|---|---|---|
| anon | IP (via `fly-client-ip`/XFF, same as RPS) | 2,000 | 20,000 |
| uplift | GitHub account (JWT `sub`) | 4,000 | 40,000 |
| admin | account (JWT `sub`) | 500,000 | 100,000,000 |

### 7.2 Accounting

Per subject: a **60-slot minute ring** (rolling 60 min → hourly gate) + **~25 hourly buckets** (rolling 24h → daily gate). Both summed **in-memory**, so hourly **and** daily reject with **no DB hit**. A charge adds actual tokens to the current minute slot + current hour bucket.

A **~5-min job** snapshots each active subject's hourly buckets to a new `token_usage_snapshot` table (restart durability only), evicts idle subjects (no activity >24h), and trims stale buckets. Startup reloads the snapshot (≤5-min loss). Memory is bounded by **#active subjects** (<1 KB each) with a reaper cap → OOM-safe (a deliberate improvement over a request-count-scaled store, given the prior prod OOM).

**Documented soft caveats:** ≤5-min loss on restart; multi-instance multiplies the caps (same property as today's RPS limiter).

### 7.3 Overrides

New `token_limit_override` table (mirrors `rate_limit_override`): `id, subject_kind ('cidr'|'user'), subject, hourly, daily, expires_at, note, created_by, created_at`; refreshed to memory ~every 30s. Resolution: **override (longest-prefix CIDR / exact user) → tier default**. Unlike the CIDR-only rate-limit override, this supports **both** CIDR (anon) and user (uplift/admin) subjects.

### 7.4 Admin CLI (mirrors `mnm ratelimits`)

```
mnm tokenlimits add (--cidr <CIDR> | --user <id>) --hourly <N> --daily <N> --ttl <DUR> [--note <S>]
mnm tokenlimits list
mnm tokenlimits extend <id> --ttl <DUR>
mnm tokenlimits remove <id> [--yes]
```

Backed by `POST/GET/PATCH/DELETE /v1/admin/tokenlimits` (admin-gated). Env knobs follow the RPS pattern: `MIDNIGHT_MANUAL_TOKEN_LIMIT_{ANON,UPLIFT,ADMIN}_{HOURLY,DAILY}`, `MIDNIGHT_MANUAL_TOKEN_SNAPSHOT_SECS` (default 300).

## 8. Reranker catalog

Reranking stays **client-side** (MCP today; server never reranks). A **named reranker registry** in `mn-embedding`, selectable via `[models] reranker = "<id>"`, `--reranker <id>` (CLI + MCP), or `MIDNIGHT_MANUAL_RERANKER`. Three load paths: **native** (fastembed `RerankerModel`), **auto-fetched ONNX mirror** (pulled via `hf-hub`, then `try_new_from_user_defined`), and **custom** (`--reranker custom --reranker-path <dir>` with `model.onnx` + the 4 tokenizer files). Default stays **`bge-reranker-base`**.

| id | Lang | ~Size | License | Load | Pick when |
|---|---|---|---|---|---|
| `ms-marco-minilm-l2` | EN | ~60 MB | Apache | ◐ mirror | absolute lowest latency |
| `ms-marco-minilm-l6` | EN | ~90 MB | Apache | ◐ mirror | English speed/quality sweet spot |
| `ms-marco-minilm-l12` | EN | ~130 MB | Apache | ◐ mirror | more quality, still small |
| `jina-reranker-v1-turbo-en` | EN | 151 MB | Apache | ✅ native | long passages (8k ctx), out-of-box |
| `bge-reranker-base` *(default)* | EN+ZH | ~280 MB | MIT | ✅ native | balanced, current default |
| `bge-reranker-v2-m3` | 100+ | ~280 MB int8 | Apache | ✅ native | best permissive multilingual / quality |
| `mxbai-rerank-base-v1` | EN | ~370 MB | Apache | ⚙ self-supply | DeBERTa alternative to BGE |
| `mxbai-rerank-base-v2` | 100+ +code | ~500–600 MB int8 | Apache | ⚙ stretch | top accuracy — validate first (decoder-style; fastembed ONNX compat unconfirmed) |

Plus **`custom`** (your own ONNX) and **Voyage** (`voyage-rerank-2.5`, `…-2.5-lite`, `…-2`) — Voyage rerankers require `VOYAGE_API_KEY`, call `/v1/rerank` **client-side**, and never touch our server or token caps (Voyage bills the user's account).

`jina-reranker-v2-base-multilingual` is **excluded from defaults** (cc-by-nc-4.0, non-commercial); mentioned in docs as opt-in-with-warning only.

Small addition: the plain CLI gets an opt-in **`--rerank`** flag (default off, preserving today's low-latency behaviour); MCP keeps `rerank=true`.

## 9. Privacy & data handling

- Corpus = **public** Midnight repos → embedding via Voyage is fine.
- **Query text via the server endpoint reaches Voyage under the *server's* account.**
- **Decision (accepted trade-off):** the server's Voyage account will have **training disabled** (zero-retention opt-out) — an operational must, documented in deploy docs. The residual exposure — non-BYOK *query text* reaching Voyage for embedding — is **accepted as a trade-off** and **not** further mitigated.
- Telemetry continues to **never log query text** — the new endpoint + accounting log only token *counts* + subject keys.
- **BYOK** sends text to the *user's* Voyage account under their own terms — surfaced in docs.
- README "Telemetry & Privacy" gains an **"Embeddings & third-party processing"** subsection; add a **privacy canary** asserting the embedding/accounting paths emit no query text (matches FR-112 / SC-061). Confirm the canary wording against `CONSTITUTION.md` during planning.

## 10. Config surface, rollout, testing

### 10.1 Config / flags

- `VOYAGE_API_KEY` env + `--voyage-api-key` (CLI + MCP); precedence **flag > env**; presence ⇒ BYOK path (embeddings + optional Voyage reranker).
- `mn-core [models]`: `embedding = "voyage-code-3"`, `reranker = "<id>"`, `reranker_path` (custom), `voyage_output_dimension = 1024`, `voyage_output_dtype = "float"`.
- Server: `VOYAGE_API_KEY` (Fly secret) + the token-limit env vars above.

### 10.2 Rollout

1. Schema migration (1024 column, HNSW recreate, `sv.embedding_model_id` search filter + `corpus_model` re-resolution, `token_limit_override`, `token_usage_snapshot`).
2. Voyage embedder client + `/v1/embeddings` endpoint + token limiter + `mnm tokenlimits` CLI.
3. Set server `VOYAGE_API_KEY` with **training disabled** (zero-retention opt-out).
4. **Initial cutover:** register `voyage-code-3@1` + re-ingest fresh on voyage via the ingest script.
5. Reranker catalog + CLI `--rerank`; `mnm models migrate` for future migrations.

### 10.3 Testing

- **Mock Voyage HTTP** (embeddings + rerank) in CI; one opt-in **live smoke test** gated on a key (like the existing manifest smoke test).
- **Token limiter** unit tests: minute-ring / hour-bucket rollover, snapshot reload, override resolution (CIDR longest-prefix + exact user), 429 windows.
- **Endpoint** integration (testcontainers Postgres): tier resolution, pre-count gating, charge-actual, 413/429/502.
- **Migration**: `mnm models migrate` enumerates non-target sources provenance-ordered; source-at-a-time atomic promote; abort-not-promote on mid-source limit/429; `sv.embedding_model_id` filter excludes not-yet-migrated sources; `corpus_model` re-resolves after finalize.
- **Retention/visibility**: `mnm models status` (and `GET /v1/admin/sources?not_model=…`) lists sources whose active version ≠ `corpus_model`; the existing source-retention sweep still hard-deletes superseded inactive old-model versions (regression-guard that migration doesn't break FR-063).
- **Search**: dim now from active model; 409 mismatch still works; partial-migration filtering.
- `fmt` / `clippy -D warnings` / MSRV / canary gates.

## 11. Decisions log

- **Scope**: all-in-one (one design, phased implementation).
- **Token accounting**: bucketed in-memory (minute-ring + hour-buckets), DB snapshot only for restart durability. Rejected: Postgres-exact (DB load), pure in-memory soft, and a per-request-entry + 5-min-sweep hybrid (daily under-count bug + request-count-scaled memory).
- **Active model**: it is `source_version.is_active` / `get_active()` / boot-resolved `corpus_model` (verified in code) — **not** a new flag. We reuse it and add a `sv.embedding_model_id = corpus_model` search filter + re-resolve `corpus_model` on ingest finalize.
- **Migration**: **re-ingest per source** (architecture-native; atomic `is_active` swap), chosen over in-place chunk re-embed. Source-at-a-time granularity; no document-level invariant, no global active flag, no `stale` status, no bespoke reembed endpoints.
- **Dimension**: 1024 / `float` / `document`-vs-`query` input types. Initial cutover clears the column (test data) → re-ingest fresh. Same-dim future swaps are zero-gap incremental; dim-changing ones have a coverage gap.
- **Provenance order**: `Foundation > Partner > ThirdParty > Community > Unknown` (verified in `mn-core/provenance.rs`); `trust_score` computed in-app, so the DB sort uses an attribution `CASE` + `verified`/recency tiebreakers.
- **BYOK** = your own Voyage key (same model), not a local fallback embedder.
- **Reranker default** stays `bge-reranker-base`; `jina-v2-multilingual` excluded (NC licence).
- **Privacy**: server account training disabled; residual non-BYOK query-text exposure to Voyage accepted as a trade-off.
- **Retention/prune**: no new prune command — the existing FR-063 source-retention sweep already collects superseded old-model versions (incl. deleted-doc chunks; inactive versions are never searched). Added `mnm models status` for incomplete-migration visibility (§5.4).

## 12. References

- VoyageAI embeddings: https://docs.voyageai.com/docs/embeddings.md · API ref: https://docs.voyageai.com/reference/embeddings-api.md
- VoyageAI reranker: https://docs.voyageai.com/docs/reranker · API ref: https://docs.voyageai.com/reference/reranker-api.md
- VoyageAI pricing: https://docs.voyageai.com/docs/pricing · rate limits: https://docs.voyageai.com/docs/rate-limits · tokenization: https://docs.voyageai.com/docs/tokenization.md · FAQ/privacy: https://docs.voyageai.com/docs/faq
- `voyage-code-3` tokenizer: https://huggingface.co/voyageai/voyage-code-3
- `fastembed` crate: https://crates.io/crates/fastembed (verified against installed 4.9.1 source)
- Rerankers: https://huggingface.co/BAAI/bge-reranker-base · https://huggingface.co/BAAI/bge-reranker-v2-m3 · https://huggingface.co/onnx-community/bge-reranker-v2-m3-ONNX · https://huggingface.co/jinaai/jina-reranker-v1-turbo-en · https://huggingface.co/cross-encoder/ms-marco-MiniLM-L6-v2 · https://huggingface.co/Xenova/ms-marco-MiniLM-L-6-v2 · https://huggingface.co/mixedbread-ai/mxbai-rerank-base-v1 · https://huggingface.co/mixedbread-ai/mxbai-rerank-base-v2

## 13. Open questions / verifications for planning

- **Active model** — RESOLVED: `source_version.is_active` + `embedding_model::get_active()` + boot-resolved `corpus_model`; reused per §4.4 (add the `sv.embedding_model_id` search filter + re-resolve `corpus_model` on ingest finalize).
- **Provenance ordering** — RESOLVED: `Foundation > Partner > ThirdParty > Community > Unknown` (`mn-core/src/provenance.rs`); JSONB-stored; `trust_score` in-app.
- **Privacy** — DECIDED: server account training disabled; residual query-text exposure accepted (§9). Still confirm the canary wording vs `CONSTITUTION.md` during planning.
- **Retention/prune** — RESOLVED: the existing FR-063 source-retention sweep (`jobs/source_retention.rs`, Phase 14) already hard-deletes superseded old-model versions + their deleted-doc chunks; no chunk-level prune added. `mnm models status` surfaces abandoned migrations (§5.4).
- **Voyage tokenizer** — confirm a Rust-loadable `tokenizer.json` (HF fast-tokenizer format) exists for `voyage-code-3`; otherwise ship the `remaining > 0` fallback (§6).
- **`mxbai-rerank-base-v2`** — validate ONNX / seq-classification compatibility with fastembed's rerank path before listing as supported; else mark experimental.
- **`corpus_model` re-resolution** — confirm the cheapest hook (post-finalize re-resolve vs a periodic refresh like the override cache) when wiring §4.4.

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
5. **Adds an admin re-embed command** to migrate existing chunks to the active model incrementally (the `voyage-code-3 → voyage-code-4` story).
6. **Makes the reranker configurable** — 8 curated local models, a custom-ONNX path, and the Voyage reranker (BYOK), since reranking is always client-side.

### Goals

- Materially better retrieval quality via `voyage-code-3`.
- Cost control on server-performed embeddings without billing-grade precision (soft caps; Voyage's 200M free tokens/month gives large headroom).
- A clean, incremental model-migration path that keeps chunk rows (no destructive re-ingest required) and is dogfooded on the initial cutover.
- Configurable, laptop-friendly reranking with an escape hatch to a custom model and to Voyage's hosted reranker.

### Non-goals (explicit)

- Multi-dimension / multi-model **coexistence** in the live corpus, and **zero-gap** cross-dimension migration (dual columns / per-model embedding table).
- **Durable/exact** (Postgres-per-charge) token accounting, sliding-log accounting, or cross-instance shared counters.
- `int8` / `binary` / `halfvec` embedding **storage** optimisation.
- **Server-side reranking.**
- Auto-converting arbitrary HuggingFace rerankers to ONNX (we ship a curated set + a custom-path only).

## 2. Findings about the current system (verified by source inspection)

These shaped the design and answer two questions raised during brainstorming.

- **The model name *is* stored next to each chunk.** `chunk.embedding_model_id` is a `NOT NULL` FK into an `embedding_model` registry (`name`, `revision`, `dim`, `provider`, `UNIQUE(name, revision)`), with the wire format `{name}@{revision}` already implemented in `mn-core/src/model_id.rs` (e.g. `bge-base-en-v1.5@1`). A trigger currently enforces that **all chunks in a `source_version` share one model**. "Find chunks still on the old model" is therefore a clean query.
- **Reranking is always client-side.** `mn-server` never reranks — it does pgvector + FTS + RRF (k=60) + confidence scoring and returns candidates. Reranking happens **only in the MCP client** (`mn-mcp`), lazily loading `bge-reranker-base`; the plain CLI `mnm search` skips reranking for latency.
- **Embedding is already client-side.** Both CLI and MCP embed locally and POST `{text, vector}` + `client_embedding_model` to `POST /v1/search`; the server validates the model matches the corpus's active model (409 on mismatch) and validates the vector dimension (currently hard-coded `768`). The server never embeds for search.
- **The embedding column is fixed-dimension.** `chunk.embedding vector(768)` with an HNSW index (`vector_cosine_ops`, m=16, ef_construction=64) bound to it; GIN index on the generated `tsvector`.
- **Rate limiting is in-memory per-second token buckets** keyed by IP (anon) / JWT `sub` (uplift, admin), with CIDR overrides stored in `rate_limit_override` (Postgres) refreshed to memory ~every 30s. Tiers: anon 10 rps, uplift 60 rps, admin 1000 rps. There is **no** hourly/daily accounting today.
- **`fastembed` 4.9.1** ships **4 native rerankers** and a **`TextRerank::try_new_from_user_defined(UserDefinedRerankingModel { onnx_source, tokenizer_files }, …)`** path for custom ONNX (and the equivalent for embedders). The native rerankers are `BGERerankerBase` (default, EN+ZH, MIT), `BGERerankerV2M3` (multilingual, via the `rozgo/bge-reranker-v2-m3` fork, Apache), `JINARerankerV1TurboEn` (English, Apache), `JINARerankerV2BaseMultiligual` (cc-by-nc-4.0 — non-commercial).

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
- **Privacy**: Voyage **retains and trains on inputs by default**; zero-retention requires an explicit opt-out (payment method + org admin + ToS acceptance). See §8.

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

### 4.4 The "active model" concept

A single **`active` flag** on `embedding_model` (partial-unique index so exactly one row is active). The existing 409 mismatch check + dimension check read from it. An admin command activates a model (e.g. `mnm models activate voyage-code-3@1`).

### 4.5 Schema migration (clean cutover; keeps rows per 1(c))

Search filters `embedding_model_id = <active>`, so chunks on any non-active model are excluded from results but **not removed**. Physical constraint: a `vector(1024)` column cannot hold the old 768-dim vectors, so "keep chunks" cannot mean "keep old vectors" once the column is re-typed.

Migration:

1. Register `voyage-code-3@1`; set it `active`.
2. Add chunk `status = 'stale'` (alongside existing `ready`/`embed_failed`/`deprecated`); for existing (old-model) chunks set `embedding = NULL`, `status = 'stale'`, keep the row (id, content, content_hash, old `embedding_model_id`).
3. `ALTER chunk.embedding TYPE vector(1024)`; **drop & recreate** the HNSW index (it skips NULLs).
4. `search.rs` stops hard-coding `768` and reads the expected dim from the active model.

**Accepted consequence:** immediately after activation every existing chunk is stale → search returns nothing until re-embedded. For the initial cutover (throwaway test data) this is fine and **dogfoods the re-embed tool**. Zero-gap cross-dimension migration on a populated corpus is out of scope (§1 non-goals).

## 5. Re-embed command — `mnm embeddings update`

The incremental model-migration tool (and the path the active-model switch relies on).

### 5.1 Data-model change: document-level model invariant

Replace the `source_version`-level model trigger with a **document-level** one ("all chunks in a *document* share one model + dim"). Per-document granularity is required because a half-migrated `source_version` would otherwise violate the trigger. (Alternative considered: migrate whole `source_version`s — coarser progress, simpler trigger — rejected in favour of document-level, matching the desired UX.)

### 5.2 Command surface (admin-only)

```
mnm embeddings update [--max-docs N] [--token-budget N] [--source a,b,c]
```

- `--token-budget` is a **client-side session** counter summing Voyage `usage.total_tokens` from **every** embed call — server-path *and* BYOK. When the budget or `--max-docs` is reached it **stops gracefully after the current document**.
- The budget **never bypasses the server rate limit**: a server-path run can also stop early on a `429` (admin tier cap); a BYOK run is bounded only by the budget (+ Voyage's own limits).
- `--source` takes a comma-separated list of source **names** restricting which stale docs are targeted.

### 5.3 Protocol (document-at-a-time)

1. `GET /v1/admin/reembed/next?sources=…` → the next **stale** document and **all** its chunks needing update, selected **ordered by provenance** (Foundation → Partner → Community → …, then `trust_score`).
2. CLI embeds those chunks (server endpoint or BYOK), then `POST /v1/admin/reembed` uploads the new vectors → server updates `embedding`, `embedding_model_id`, `status='ready'` **atomically per document**.
3. Loop until no stale docs remain (within the `--source` filter) or a limit trips.

## 6. `POST /v1/embeddings` endpoint

```
POST /v1/embeddings   (bearer middleware: anon→per-IP, uplift→per-account, admin→per-account; anon allowed)

Request:
{
  "input": ["…", "…"],              // string or array; search = 1 short string
  "input_type": "query" | "document" // default "query"; ingest/re-embed pass "document"
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
- **Query text via the server endpoint reaches Voyage under the *server's* account.** Requirements: (1) the server's Voyage account **must enable zero-retention opt-out** (Voyage retains + trains by default) — an operational must, documented in deploy docs; (2) telemetry continues to **never log query text** — the new endpoint + accounting log only token *counts* + subject keys.
- **BYOK** sends text to the *user's* Voyage account under their own terms — surfaced in docs.
- README "Telemetry & Privacy" gains an **"Embeddings & third-party processing"** subsection; add a **privacy canary** asserting the embedding/accounting paths emit no query text (matches FR-112 / SC-061). Verify against `CONSTITUTION.md` during planning so the privacy invariants don't regress.

## 10. Config surface, rollout, testing

### 10.1 Config / flags

- `VOYAGE_API_KEY` env + `--voyage-api-key` (CLI + MCP); precedence **flag > env**; presence ⇒ BYOK path (embeddings + optional Voyage reranker).
- `mn-core [models]`: `embedding = "voyage-code-3"`, `reranker = "<id>"`, `reranker_path` (custom), `voyage_output_dimension = 1024`, `voyage_output_dtype = "float"`.
- Server: `VOYAGE_API_KEY` (Fly secret) + the token-limit env vars above.

### 10.2 Rollout

1. Schema migration (1024 column, `stale` status, `active` flag, document-level trigger, `token_limit_override`, `token_usage_snapshot`).
2. Voyage embedder client + `/v1/embeddings` endpoint + token limiter + `mnm tokenlimits` CLI.
3. Set server `VOYAGE_API_KEY` with **zero-retention opt-out** enabled.
4. Activate `voyage-code-3@1`; run `mnm embeddings update` (BYOK) to populate — dogfoods the migration.
5. Reranker catalog + CLI `--rerank`.

### 10.3 Testing

- **Mock Voyage HTTP** (embeddings + rerank) in CI; one opt-in **live smoke test** gated on a key (like the existing manifest smoke test).
- **Token limiter** unit tests: minute-ring / hour-bucket rollover, snapshot reload, override resolution (CIDR longest-prefix + exact user), 429 windows.
- **Endpoint** integration (testcontainers Postgres): tier resolution, pre-count gating, charge-actual, 413/429/502.
- **Re-embed**: document-level invariant trigger, provenance ordering, session-budget stop (server-path 429 + BYOK budget), atomic per-doc update.
- **Search**: dim now from active model; 409 mismatch still works; stale chunks excluded.
- `fmt` / `clippy -D warnings` / MSRV / canary gates.

## 11. Decisions log

- **Scope**: all-in-one (one design, phased implementation).
- **Token accounting**: bucketed in-memory (minute-ring + hour-buckets), DB snapshot only for restart durability. Rejected: Postgres-exact (rejected by user for DB load), pure in-memory soft (replaced), and a per-request-entry + 5-min-sweep hybrid (had a daily under-count bug and request-count-scaled memory).
- **1(c)**: keep chunk rows on model switch; exclude from search by `embedding_model_id = active`; NULL old vectors + `status='stale'` to satisfy the fixed-dim column.
- **Model invariant**: relaxed `source_version`-level → **document-level** to enable per-document re-embedding.
- **Encoding**: 1024 / `float` / `document`-vs-`query` input types.
- **BYOK** = your own Voyage key (same model), not a local fallback embedder.
- **Reranker default** stays `bge-reranker-base`; `jina-v2-multilingual` excluded (NC licence).

## 12. References

- VoyageAI embeddings: https://docs.voyageai.com/docs/embeddings.md · API ref: https://docs.voyageai.com/reference/embeddings-api.md
- VoyageAI reranker: https://docs.voyageai.com/docs/reranker · API ref: https://docs.voyageai.com/reference/reranker-api.md
- VoyageAI pricing: https://docs.voyageai.com/docs/pricing · rate limits: https://docs.voyageai.com/docs/rate-limits · tokenization: https://docs.voyageai.com/docs/tokenization.md · FAQ/privacy: https://docs.voyageai.com/docs/faq
- `voyage-code-3` tokenizer: https://huggingface.co/voyageai/voyage-code-3
- `fastembed` crate: https://crates.io/crates/fastembed (verified against installed 4.9.1 source)
- Rerankers: https://huggingface.co/BAAI/bge-reranker-base · https://huggingface.co/BAAI/bge-reranker-v2-m3 · https://huggingface.co/onnx-community/bge-reranker-v2-m3-ONNX · https://huggingface.co/jinaai/jina-reranker-v1-turbo-en · https://huggingface.co/cross-encoder/ms-marco-MiniLM-L6-v2 · https://huggingface.co/Xenova/ms-marco-MiniLM-L-6-v2 · https://huggingface.co/mixedbread-ai/mxbai-rerank-base-v1 · https://huggingface.co/mixedbread-ai/mxbai-rerank-base-v2

## 13. Open questions / risks to resolve during planning

- **Confirm the existing "active model" mechanism** in `mn-server` (the 409 check references one) — formalise as the `active` flag if not already present.
- **Confirm a Rust-loadable `tokenizer.json`** exists for `voyage-code-3` (HF fast-tokenizer format) for pre-flight token counting; otherwise ship the `remaining > 0` fallback.
- **Confirm provenance/attribution ordering values** (Foundation → Partner → …) and the exact column(s) used for the re-embed sort.
- **`mxbai-rerank-base-v2`** ONNX/seq-classification compatibility with fastembed's rerank path — validate before listing as supported (else mark experimental).
- **Verify `CONSTITUTION.md` privacy principles** are not regressed by sending query text to Voyage; design the canary accordingly.

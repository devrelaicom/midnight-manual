# Contextualized + Dual Code Embeddings — Design

**Date**: 2026-06-10
**Status**: Approved (brainstorm session 2026-06-10); amended same day after codebase verification review (factual corrections — see git history)
**Supersedes/amends**: parts of `2026-06-02-voyage-embeddings-token-limits-design.md` (voyage-code-3 as the single corpus model)

## 1. Summary

Move the corpus to **voyage-context-3 contextualized chunk embeddings** as the general embedding model for *all* document kinds, and add a **second, code-specialized embedding (voyage-code-3)** for source-code files only. Remove all chunk overlap and switch both chunkers to greedy coalescing toward 90% of a raised 1024-token budget. Search gains a `code_mode` argument (`on | off | exclusive`) that fuses a code-vector ranked list into the existing RRF pool. The embeddings endpoint gains a `type` argument (`general | code`) so clients never pick models directly.

Hard cutover: full corpus re-ingest, no back-compat shims (project is pre-1.0).

## 2. Decisions (from brainstorm)

| # | Decision |
|---|----------|
| D1 | **Storage**: one chunk row with a nullable `code_embedding vector(1024)` column — NOT separate rows, NOT a side table. Chunk identity is load-bearing (FTS tsvector, `chunk_index` navigation, MCP `get_chunk*` tools, overlap dedup). Both models emit 1024-dim vectors, so the column type matches. |
| D2 | **Chunk budget**: raise max from 400 → **1024 tokens**; greedy coalescing target = **90% (~920 tokens)** for both Markdown sections and code symbols. |
| D3 | **No overlap anywhere**: the primary Markdown heading-split and AST paths are already non-overlapping; both overlapping *fallbacks* — the Markdown token-window fallback (`fallback_overlap_lines=20`) and the code line-window fallback (60 lines / 20-line overlap) — become token-budgeted, non-overlapping window chunkers. |
| D4 | **"Other" files use our own chunker, NOT Voyage server-side auto-chunking.** Rationale: deterministic chunk boundaries (stable `content_hash` across re-ingests), byte offsets preserved (`start_byte`/`end_byte`), one ingestion code path. The contextualization benefit is identical — the whole document still goes up as one context group. `enable_auto_chunking` is explicitly out of scope. |
| D5 | **Result fusion**: code-vector results join the **same RRF pool** (k=60) as a third ranked list. One ranked result list; dedup, confidence scoring, rerank unchanged. |
| D6 | **code_mode is an explicit parameter** on search (server, CLI, MCP). No heuristic query sniffing. "Automatic switching" in CLI/MCP means: ingestion uses `input_type=document` and embeds per content kind; search uses `input_type=query` and embeds with the model(s) the chosen mode/code_mode require. |
| D7 | **Extension allowlist**: keep allowlist-gated discovery, but extend `language.rs` to cover every language the code chunker supports (fixes the latent bug where glob-included `.py`/`.go`/etc. bypass tree-sitter and fall into the line-window path). |
| D8 | **Balanced document splitting** applies to ALL document kinds at the *context-group* level (voyage-context-3's 32K-token per-document limit), using minimum-group-count, roughly-equal-size splits where no group exceeds 90% of the limit. |
| D9 | Per-source opt-out of code embeddings via manifest option + `mnm ingest run --no-code-embeddings` flag (flag overrides manifest). |

## 3. Background: current state (for the implementing session)

- **Chunkers** (`mn-content`): Markdown heading-split + coalesce (max 400 tokens, min 280) in `src/markdown.rs`; tree-sitter symbol-aware code chunker + Compact CST chunker in `src/code/` (budget 400, coalesce min 64, breadcrumbing); line-window fallback 60 lines / 20-line overlap in `src/code/line_window.rs`.
- **Kind classification** (`src/manifest/resolve.rs:234`): `markdown` → Markdown; any other known language → Code; unknown (glob-included) → Plaintext. Note `.txt`/`.json` currently classify as **Code** and fall through to the line-window chunker.
- **Embedding** (`mn-embedding/src/voyage.rs`): reqwest HTTP/1.1-only client for `POST /v1/embeddings`, voyage-code-3, `input_type` query/document, batch ≤1,000 texts / ≤120K tokens.
- **Schema** (migrations 0002/0003/0007/0008): `chunk.embedding` (vector(768) in 0002, altered to 1024 in 0008) + HNSW and generated-`tsvector` GIN indexes (0003), `embedding_model_id` FK with a trigger (`check_chunk_embedding_model_match()`, defined in 0002) enforcing it matches `source_version.embedding_model_id`. `embedding_model` registry keyed `{name}@{revision}` (provider string `voyageai`); `voyage-code-3@1` registered in 0008.
- **Search** (`mn-server/src/routes/search.rs`): modes hybrid/vector/fts; client supplies query vector(s) + `client_embedding_model`; RRF (k=60) fuses per-mode lists; confidence = trust × relevance; overlap dedup; reranking is client-side.
- **Embeddings endpoint** (`mn-server/src/routes/embeddings.rs`): `POST /v1/embeddings` `{input, input_type, model?}`, token-limited per tier (migration 0009 tables), 409 on model mismatch, 413 oversize, 429 with `Retry-After`.
- **Ingest CLI** (`mn-cli/src/commands/ingest/run.rs`): manifest walk → chunk → embed (BYOK direct-to-Voyage or server-proxy) → batched document upload → finalize.

## 4. Voyage contextualized embeddings API (verified 2026-06-10)

`POST https://api.voyageai.com/v1/contextualizedembeddings`

- `inputs`: **list of lists** — each inner list is one document's chunks, embedded together so each chunk vector carries document-level context.
- `model`: `voyage-context-3`. `input_type`: `query` | `document` | null. `output_dimension`: 1024 (default; also 256/512/2048). `output_dtype`: float (default).
- **Limits**: ≤ **32,000 tokens per inner list** (per document); per request: ≤ 1,000 inputs, ≤ 120K total tokens, ≤ 16K total chunks.
- Response: one embedding list per input, each item `{embedding, index}`; `usage.total_tokens`.
- A **query** is embedded as a single-chunk document: `inputs=[["<query text>"]]`, `input_type="query"`.
- Voyage docs state embeddings made with/without `input_type` prompts remain compatible. Overlap is permitted but not recommended; overlapping tokens bill normally — we use none.

## 5. Chunking changes (`mn-content`)

### 5.1 Budgets

- `max_tokens`: 400 → **1024** (CLI flag `--code-chunk-tokens` default updated; same budget for Markdown).
- Coalesce target: **90% of max** (~920). Greedy packing: append the next sibling unit (Markdown section / top-level symbol) while the running total stays ≤ target; emit and start a new chunk when the next unit would exceed it. A single unit larger than `max_tokens` still splits (existing splitter/window-expansion machinery), with breadcrumbing preserved for code.
- Remove the Markdown `min_tokens=280` floor and the code `code_min_tokens=64` coalesce minimum as separate concepts — greedy-to-90% subsumes both. The `mnm ingest run --md-min-tokens` flag is deleted (pre-1.0, no deprecation shim); `--code-chunk-tokens` is renamed/generalized to `--chunk-tokens` (default 1024) since the budget now applies to all kinds. The line-based `--code-chunk-lines` (60) and `--code-chunk-overlap` (20) flags are deleted too — the windowing they configure becomes token-budgeted (§5.2), with no overlap knob.
- Greedy packing across *unrelated* top-level symbols is intentional: today's `coalesce_code` only merges chunks sharing a non-empty enclosing scope; the new coalescer packs adjacent siblings regardless (symbol facets handled in §5.3).

### 5.2 Overlap removal

- `line_window.rs`: replace 60-line/20-line-overlap windows with **token-budgeted non-overlapping windows** targeting 90% of `max_tokens`. Used for: Plaintext kind, unparseable code fallback, oversized-single-unit fallback.
- Markdown rolling-window fallback: drop `fallback_overlap_lines=20` → 0.
- After this change, **no chunker emits overlapping chunks**. The retrieval-side overlap-dedup pass stays (it also handles cross-version duplication) but should see near-zero same-document trims.

### 5.3 Symbol facet integrity

Coalesced code chunks will routinely contain several top-level symbols. Today `symbol_path` is a single root→deepest ancestor path per chunk (`Vec<SymbolSegment>`; `code/symbols.rs`), and `coalesce_code` keeps only the first chunk's path. It must instead carry **every symbol contained in the chunk**, not just the first.

Encoding: a **flat JSONB array of symbol entry objects**, one per contained symbol, each `{"kind": ..., "name": ..., "path": [ancestor names]}` (union of the coalesced units). The existing facet query builds `@> '[{"kind":..,"name":..}]'` containment (`search.rs::symbol_json`); JSONB containment ignores extra keys on stored objects, so flat entries with an added `path` key keep that query working unchanged — do NOT encode as an array-of-paths (nested arrays), which would break `@>`. Breadcrumb comments remain for interior chunks of *split* symbols only.

### 5.4 Kind classification + allowlist (D7)

- Extend `language.rs::from_path` to all code-chunker languages: add `py/pyi → python`, `go → go`, `sol → solidity`, `sh/bash → bash`, `scm/ss/sld → scheme`, `java → java`, `swift → swift`, `rb → ruby`, `kt/kts → kotlin`, `cs → csharp`, `hs → haskell`, `html/htm → html`, `xml/csproj/nuspec/plist → xml`, `mjs/cjs → javascript`. These join default discovery.
- Fix `kind_for`: `txt` → **Plaintext** (today it lands in Code via the `Some(_)` arm). `json/yaml/toml` stay Code (config-as-code; voyage-code-3 handles them).
- **Dual embedding applies to `DocumentKind::Code` only.** Markdown and Plaintext get voyage-context-3 only.

## 6. Context grouping & balanced splitting (D8)

Applies between chunking and embedding, for the voyage-context-3 call only (voyage-code-3 batches stay flat):

1. For each document, sum chunk `token_count`. If ≤ 90% of 32,000 (28,800), the document is one context group.
2. Otherwise compute `n = ceil(total_tokens / 28_800)` and partition the **contiguous chunk sequence** into `n` groups with roughly equal token totals (greedy fill to `total/n`, never exceeding 28,800). Example: a 220%-of-limit document → 3 groups ≈ 73/73/74%, never 90/90/40.
3. Each group is one inner list in `inputs`. Chunk rows are unaffected — grouping only changes what context Voyage sees.
4. Request packing: combine many documents' groups per request, respecting ≤1,000 inputs, ≤120K tokens, ≤16K chunks; restore order via response `index`. Reuse the HTTP/1.1-only reqwest pattern (the HTTP/2 stall fix applies here too).

Property tests: minimal `n`; every group ≤ 28,800; group sizes within one chunk's tokens of each other where chunk granularity allows; concatenation of groups reproduces the original chunk order.

Edge case: a *single chunk* can never exceed 28,800 tokens given `max_tokens=1024` — assert this invariant rather than handling it.

Tokenizer caveat: budgets are computed with **our BPE token counts** (2026-05-27 design), not Voyage's tokenizer. The 10% headroom (28,800 vs 32,000) is the buffer that absorbs count divergence — same posture as the existing 120K-per-request margin handling.

## 7. Embedding layer (`mn-embedding`)

- New `ContextualizedVoyageEmbedder` targeting `/v1/contextualizedembeddings`: `embed_document_groups(groups: &[Vec<String>]) -> Vec<Vec<Vec<f32>>>` for ingest (`input_type=document`) and `embed_query(text) -> Vec<f32>` as `[[text]]` with `input_type=query`.
- Existing `voyage.rs` flat client remains for voyage-code-3 (ingest `document`, search `query`).
- Model wire IDs: register **`voyage-context-3@1`** (provider `voyageai`, matching the existing `voyage-code-3` row; dim 1024). `voyage-code-3@1` already registered.
- Roles: `voyage-context-3@1` = **general/corpus model**; `voyage-code-3@1` = **code model**. Resolution mirrors the existing boot-resolved `corpus_model`, adding a `code_model`.

## 8. Schema (new migration 0011 — never edit applied migrations)

```sql
ALTER TABLE chunk ADD COLUMN code_embedding vector(1024);
CREATE INDEX chunk_code_embedding_hnsw ON chunk
  USING hnsw (code_embedding vector_cosine_ops)
  WITH (m = 16, ef_construction = 64)
  WHERE code_embedding IS NOT NULL;

ALTER TABLE source_version ADD COLUMN code_embedding_model_id uuid REFERENCES embedding_model(id);
-- Extend the model-invariant trigger: when chunk.code_embedding IS NOT NULL,
-- its source_version.code_embedding_model_id must be set and match.

INSERT INTO embedding_model (name, revision, dim, provider) VALUES ('voyage-context-3', 1, 1024, 'voyageai');
-- Truncate/clear existing chunk embeddings (corpus is re-ingested; pre-1.0 hard cutover).
```

Notes:

- Numbering: **0010 is taken** by the in-flight `0010_telemetry_search_daily.sql` (commit `0ab3e4e` on `mcp-response-format`, not yet on main). 0011 assumes that lands first — renumber if it doesn't.
- The model-invariant trigger function (`check_chunk_embedding_model_match()`) was created in 0002; extend it via `CREATE OR REPLACE FUNCTION` in this migration (never edit the applied 0002 file).
- Partial HNSW index keeps the code ANN graph restricted to code chunks.
- `source_version.embedding_model_id` now means the **general** model. `code_embedding_model_id IS NULL` ⇔ code embeddings disabled (or no code files) for that version.
- Search continues to gate on the active corpus models, excluding not-yet-migrated source versions.

## 9. `POST /v1/embeddings` endpoint

Request gains `type`:

```jsonc
{
  "input": ["..."] ,            // flat list, OR nested list-of-lists (general type only)
  "type": "general" | "code",  // optional, default "general"
  "input_type": "query" | "document",
  "model": "voyage-context-3@1" // optional pin; 409 if it mismatches the type-resolved model
}
```

- `type=general` → voyage-context-3 via `/v1/contextualizedembeddings`. Flat `input` = each string is its own single-chunk document (correct for queries). Nested `input: [[...]]` = caller-provided context groups (used by server-proxy ingestion); each inner list ≤ 28,800 tokens, else 413.
- `type=code` → voyage-code-3 via flat `/v1/embeddings`. Nested input with `type=code` → 400.
- Response unchanged in shape: `embeddings` is flattened row-per-chunk in input order; `model` reports the resolved wire ID. Token limiter (hour/day tiers, 429 + headers) applies identically to both types.

## 10. Search

### 10.1 Request

```jsonc
{
  "query": "...",
  "vector": [/* 1024, voyage-context-3 query embedding */],
  "code_vector": [/* 1024, voyage-code-3 query embedding; required iff code_mode != off */],
  "client_embedding_model": "voyage-context-3@1",
  "client_code_embedding_model": "voyage-code-3@1",
  "mode": "hybrid" | "vector" | "fts",
  "code_mode": "on" | "off" | "exclusive",   // optional
  // ...existing fields (queries[], filters, limit, sort_by, min_confidence, include_scores)
}
```

Multi-query form: each entry in `queries[]` carries `vector` + `code_vector` the same way.

### 10.2 Semantics (D5)

| mode | code_mode default | ranked lists fused by RRF (k=60) |
|---|---|---|
| hybrid | `on` | general vector + code vector + FTS |
| hybrid + `off` | — | general vector + FTS (today's behavior) |
| hybrid + `exclusive` | — | code vector + FTS |
| vector | `on` | general vector + code vector |
| vector + `off` | — | general vector |
| vector + `exclusive` | — | code vector |
| fts | `off` (forced) | FTS only; `code_mode` `on`/`exclusive` → **400** with explicit error message |

- Code-vector ANN runs against the partial `code_embedding` index only — chunks without code embeddings simply can't appear in that list.
- Validation: `vector` required & model-checked when the general list is in play; `code_vector` required & checked against the code model when `code_mode != off`. Dimension checks as today.
- Dedup, confidence (trust × relevance), sorting, `min_confidence`, and client-side reranking are unchanged — they operate on the fused list. `search_metadata.per_query` gains `code_vector_candidates` / `code_vector_latency_ms`; top-level metadata gains effective `code_mode`. Search telemetry events record `code_mode`.

## 11. CLI / MCP

### 11.1 Ingestion (`mnm ingest run`)

- Per document: chunk → context-group → embed general (context-3, `input_type=document`); if `kind == Code` and code embeddings enabled → also embed flat with code-3.
- BYOK (`VOYAGE_API_KEY`): call both Voyage endpoints directly. Server-proxy: `POST /v1/embeddings` twice (`type=general` with nested groups; `type=code` flat).
- Opt-out (D9): manifest top-level `code_embeddings: false` (default true); CLI `--no-code-embeddings` overrides manifest. Note: this is the **first manifest-level option** — `Manifest` is currently just `{manifest_version, root}` with all configuration CLI-flag-only, so this adds a new (optional, defaulted) field to the manifest schema and establishes the flag-overrides-manifest precedent. Recorded on `source_version` via `code_embedding_model_id = NULL`; document upload omits code vectors.
- Upload payload: each chunk carries `embedding` and optional `code_embedding`.

### 11.2 Search (CLI + MCP)

- `mnm search` gains `--code-mode on|off|exclusive`; the MCP `search` tool's input schema gains `code_mode` (enum, optional, documented defaults). (There is no `advanced_search` MCP *tool* — advanced search is a skill installed via `install_search_skill`; its docs are updated, see below.) Defaults inherit the server rules (D6) — no client-side query sniffing.
- Client embedding flow: embed query with context-3 always (unless fts); additionally with code-3 when effective `code_mode != off`. BYOK embeds locally; otherwise two `type=`-tagged calls to `/v1/embeddings`. MCP advanced-search skill docs updated to describe `code_mode`.

## 12. Out of scope / consequences noted

- **Voyage `enable_auto_chunking`**: rejected (D4). Revisit only if a future corpus adds large unstructured documents where Voyage's chunker demonstrably wins.
- **Reranker truncation**: bge-reranker-base truncates ~512 tokens, so 1024-token chunks rerank on their first half. Acceptable for now; the configurable-reranker work (2026-06-02 design) already allows longer-context rerankers (e.g. Voyage rerank-2.5). Flag in docs.
- **Cost**: code-file tokens are billed twice (context-3 + code-3). Per-source opt-out is the lever.
- **`mnm models migrate`** (2026-06-02 §5, unimplemented): its design must account for dual models when picked up; not built here. Full re-ingest covers this cutover.
- Facet registry, chunk-context enrichment, and manifest-generation workstreams are unaffected except where noted (`symbol_path` enrichment in §5.3 helps the `symbol` facet).

## 13. Testing

- **Unit/property** (`mn-content`): greedy 90% coalescing (md + code + Compact); no-overlap invariant across all chunkers (assert disjoint byte ranges); balanced-split properties (§6); multi-symbol `symbol_path` correctness; new extension → language → kind mappings (incl. `txt → Plaintext`).
- **Unit** (`mn-embedding`): contextualized request shaping (nested inputs, limits, order restoration via `index`), query-as-single-chunk-document, mocked-server tests for both endpoints (run with `VOYAGE_API_KEY=` cleared — sandbox sets it and breaks BYOK-path tests).
- **Contract** (`mn-server`): `code_mode` × `mode` matrix incl. fts+on/exclusive → 400; missing/mismatched `code_vector` → 4xx; `/v1/embeddings` `type` routing, nested-input validation, 409/413 paths; token-limit accounting across both types.
- **Integration** (CI-only; sandbox has no Docker/DATABASE_URL): dual-embedding ingest → migration 0011 schema → hybrid search with each `code_mode`; opt-out source has `code_embedding IS NULL` and is absent from code-vector results.
- **Recall** (`tests/recall/`): this directory is planned in CLAUDE.md but **does not exist yet** — create the harness and fixtures sized for the new chunk budgets; include code-query cases exercising `code_mode=on` vs `off`.

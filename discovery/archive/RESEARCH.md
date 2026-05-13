# Research Log: rag-platform

*Chronological record of all research conducted during discovery.*

---

[Research entries will be added as research is conducted]

## R1: Local embedding library for Rust — 2026-05-13

**Purpose**: Decide whether ingest-time and query-time embedding can be done locally in Rust, and which crate to use.

**Approach**: [Approach not provided]

**Findings**:
fastembed-rs (https://github.com/Anush008/fastembed-rs, fastembed on crates.io) is the mature production choice. ONNX Runtime backend; 3-5x faster than Python equivalents per public benchmarks; 60-80% lower memory than equivalent PyTorch. Supports BGE, jina, MiniLM, nomic-embed families. Candle is more flexible (any HF model directly from safetensors) but slower on GPU and less battle-tested for embedding-only use cases. embed_anything is a thinner wrapper, less mature. Both ingest CLI and local MCP server can use the same crate, guaranteeing model parity between corpus and query embeddings.

**Industry Patterns**:
ONNX-based Rust embedding pipeline. Pre-quantized int8 models for laptop inference. Same model used at ingest and query time to guarantee vector-space compatibility.

**Relevant Examples**:
[Examples not provided]

**Implications**:
Story 1 (schema): vector column dimension must match the chosen model (e.g. 384 for bge-small-en-v1.5, 768 for bge-base, 1024 for bge-large). Lock the model choice in Story 1 and treat any change as a re-embedding migration. Story 2 and 3 (ingest): CLI generates embeddings locally and POSTs them with chunks. Story 5 (MCP server): embeds queries locally — no remote embedding endpoint needed.

**Stories Informed**: 1,2,3,5

**Related Questions**: [Questions not specified]

---

## R2: Cross-encoder reranking in Rust — 2026-05-13

**Purpose**: Determine whether reranking is feasible inside the local MCP server under the <1s p95 budget.

**Approach**: [Approach not provided]

**Findings**:
Yes. fastembed-rs ships reranker support natively (project tagline: 'generating vector embeddings, reranking locally'). BGE-reranker-v2-m3 is the current open SOTA and is available as ONNX. 2026 benchmarks: CPU ~8ms/pair, ~130ms for a 16-pair batch on a modern laptop; GPU 50-100ms total. For a top-K=20 candidate set (typical for RAG): ~160ms reranking on CPU. Combined with query embedding (~30-80ms), cloud round-trip (~50-200ms), and Postgres hybrid query (~50-150ms), total budget is roughly 290-590ms — fits comfortably under the 1s p95 constitutional target.

**Industry Patterns**:
Two-stage retrieval: (1) hybrid retrieval returns top-K candidates from RRF-merged FTS + vector; (2) cross-encoder reranks the K candidates against the original query. K is tunable; 20-30 is typical.

**Relevant Examples**:
[Examples not provided]

**Implications**:
Story 5 (MCP server): reranker is a server-side responsibility. Add a tool flag to disable reranking for ultra-low-latency callers (skip stage 2). Story 6 (confidence): the reranker score is one of several inputs to the final confidence score — provenance metadata also feeds in. Story 1 (schema): no DB schema impact; reranking happens after retrieval.

**Stories Informed**: 5,6

**Related Questions**: [Questions not specified]

---

## R3: Query rewriting (HyDE / multi-query) for MCP RAG — 2026-05-13

**Purpose**: Decide whether the local MCP server should rewrite or expand queries server-side, or delegate to the calling agent.

**Approach**: [Approach not provided]

**Findings**:
Recommendation: delegate to the caller, not the server. HyDE and multi-query rewriting each require an LLM call which adds 200-800ms of latency and forces either a bundled local model (against Constitution IV frictionless-setup) or an outbound API call (against telemetry/privacy posture). The MCP client is itself an LLM and can perform rewriting natively for free. 2026 best practice (DEV Community, Alhena, Redis blog): apply rewriting *adaptively*, triggered when initial retrieval is weak — not unconditionally. Sophisticated callers (agents) can detect weak retrieval and re-call the tool with rewritten queries.

**Industry Patterns**:
Multi-query input on the tool signature (queries: string[]) — caller may pass 1-N queries. Server runs hybrid retrieval against each, RRF-merges across queries plus across retrieval modes. Server stays fast; quality scales with caller sophistication.

**Relevant Examples**:
[Examples not provided]

**Implications**:
Story 5 (MCP server): tool input takes a queries: string[] array, not a single string. Document HyDE/multi-query patterns in the MCP tool description so calling agents discover them. Story 7 (query enhancement): scope shrinks — keep the story for documentation/cookbook plus server-side support for the multi-query pattern (RRF across queries), but drop any plan to run an LLM inside the server.

**Stories Informed**: 5,7

**Related Questions**: [Questions not specified]

---

## R4: Hybrid FTS + vector search on Postgres / pgvector — 2026-05-13

**Purpose**: Determine the right pattern for combining lexical and semantic search on the Fly.io managed Postgres backend.

**Approach**: [Approach not provided]

**Findings**:
2026 industry-standard pattern: run BM25-style FTS and pgvector cosine/L2 similarity in parallel, then merge results with Reciprocal Rank Fusion (RRF): score = sum over methods of 1/(k + rank), k=60. RRF ignores raw scores and only uses ranks — robust across heterogeneous score scales. Published benchmarks: recall@10 rises from 65-78% (single mode) to ~91% with hybrid + RRF. Two Postgres implementation flavors: (a) native tsvector + ts_rank_cd + pgvector — built-in, no extra extensions, works on any managed Postgres; (b) pg_search (ParadeDB) or VectorChord BM25 — true BM25, faster on larger corpora, requires the extension to be available on Fly.io managed Postgres (not guaranteed).

**Industry Patterns**:
Stage 1: parallel queries to (FTS index, vector index) each returning top-N. Stage 2: RRF merge in app code (not in Postgres) to keep it testable. Stage 3: reranker on the merged top-K.

**Relevant Examples**:
[Examples not provided]

**Implications**:
Story 1 (schema): chunks table has both tsvector (FTS) and vector (embedding) columns; both indexed. Story 4 (read API): hybrid query + RRF merge lives in the cloud server. Start with native FTS for portability, benchmark, swap in pg_search later only if FTS quality is a measured bottleneck. Story 9 (ops): confirm at deploy time whether pg_search is available on Fly.io managed Postgres — if yes, it's a future optimization path.

**Stories Informed**: 1,4,6,9

**Related Questions**: [Questions not specified]

---

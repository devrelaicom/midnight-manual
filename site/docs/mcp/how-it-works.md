---
title: How it works
sidebar_label: How it works
description: What the Midnight Manual MCP server is, what it exposes, and how its hybrid retrieval and reranking work.
---

# How the MCP server works

`mnm mcp serve` runs a small MCP server that speaks JSON-RPC 2.0 over stdio — or, with `--http`, over stateless Streamable HTTP (`POST /mcp`, loopback-bound by default; see [Add to your AI client](/docs/add-to-ai-client#serving-over-http)). It starts in well under half a second because there are no local models to load; embedding and reranking are both remote VoyageAI calls.

## What it exposes

The server exposes **13 tools** grouped into four categories:

- **Search** — `search` and `advanced_search` for querying the corpus
- **Read a hit in context** — seven tools for walking from a search result to its surrounding text
- **Corpus & diagnostics** — three tools for inspecting what's in the corpus and whether the service is healthy
- **Local install** — `install_search_skill` to write the retrieval playbook into your AI harness

Every tool is read-only except `install_search_skill`. The full tool surface is documented in the pages that follow.

## The hosted read API

The server talks to a hosted corpus: a continuously ingested snapshot of Midnight Network documentation and code. You do not run a database locally. The server embeds your queries (using VoyageAI) and posts them to the hosted search endpoint; results come back as ranked, scored excerpts.

The server sends only your read-uplift token, if you have one. See [Rate limits](./rate-limits.md) for how tiers work and how to get a free 6× lift with `mnm auth github`.

## How retrieval works

Retrieval is hybrid. Lexical results (PostgreSQL full-text) and semantic results (pgvector) are fused with Reciprocal Rank Fusion, so exact-term matches and conceptual matches both surface. A pure-vector search misses exact identifiers; a pure keyword search misses paraphrases. Hybrid runs both passes.

`advanced_search` then re-scores the candidate set with VoyageAI's cross-encoder reranker (`rerank-2.5` by default), which sharpens precision on hard queries. Reranking is on by default. If a rerank call fails, the server falls back to RRF order and flags the reason in the response; it does not silently fail the search.

Each result carries a **trust score** alongside its relevance score. The server blends source attribution, verification, freshness, deprecation, and version-match into that score and returns the per-factor breakdown inline. A client reads the breakdown from the result without a second call.

Errors come back as structured envelopes. Each failure carries remediation guidance and `suggested_next_actions`. When the corpus embedding model has rolled forward, `search` returns an `embedding_model_mismatch` envelope that names both models and the fix.

The server reads from the indexed corpus rather than from model training memory. When the Midnight SDK ships a new package or the Compact language changes, the corpus is re-ingested and the results change with it, with no model retrain.

## What a connected client can do

A client connected to the server can:

- Search the Midnight corpus with a single natural-language question
- Walk from a search hit to its surrounding document context, chunk by chunk
- Filter results by source, language, package, SDK version, and more
- Discover what material exists in the corpus before constructing a query
- Check service health and rate-limit state before a long session

The [Advanced Search skill](./advanced-search-skill.md) teaches a client to combine these tools into a deliberate retrieval strategy.

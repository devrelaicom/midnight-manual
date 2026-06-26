---
title: How it works
sidebar_label: How it works
description: What the Midnight Manual MCP server is, what it exposes, and why hybrid retrieval gives an AI client better answers than raw model memory.
---

# How the MCP server works

`mnm mcp serve` is a hand-rolled MCP server — JSON-RPC 2.0 framed over stdio. It starts in well under half a second because there are no local models to load: both embedding and reranking are remote VoyageAI calls. Adding it to your AI client costs nothing at idle.

## What it exposes

The server exposes **13 tools** grouped into four categories:

- **Search** — `search` and `advanced_search` for querying the corpus
- **Read a hit in context** — seven tools for walking from a search result to its surrounding text
- **Corpus & diagnostics** — four tools for inspecting what's in the corpus and whether the service is healthy
- **Local install** — `install_search_skill` to write the retrieval playbook into your AI harness

Every tool is read-only except `install_search_skill`. The full tool surface is documented in the pages that follow.

## The hosted read API

The server talks to a hosted corpus — a continuously ingested snapshot of Midnight Network documentation and code. You do not run a database locally. The server embeds your queries (using VoyageAI) and posts them to the hosted search endpoint; results come back as ranked, scored excerpts.

Authentication uses only your read-uplift token — never admin credentials. See [Rate limits](./rate-limits.md) for how tiers work and how to get a free 6× lift with `mnm auth github`.

## Why it produces better answers

**Hybrid retrieval, not just vectors.** Lexical (PostgreSQL full-text) and semantic (pgvector) results are fused with Reciprocal Rank Fusion, so exact-term matches and conceptual matches both surface. A pure-vector search misses exact identifiers; a pure keyword search misses paraphrases. Hybrid catches both.

**VoyageAI reranking.** `advanced_search` re-scores the candidate set with VoyageAI's cross-encoder reranker (`rerank-2.5` by default) for precision on hard queries. It is on by default. On any rerank failure the server degrades gracefully to RRF order and flags the reason — it never silently fails the search.

**Confidence you can reason about.** Each result blends a **trust score** (source attribution, verification, freshness, deprecation, version-match) with relevance, and returns the factor breakdown. Your assistant can explain *why* a passage is trustworthy without an extra round-trip.

**Structured errors that self-correct.** Failures come back as machine-readable envelopes with remediation guidance and `suggested_next_actions`. If the corpus's embedding model has rolled forward, `search` returns an `embedding_model_mismatch` envelope naming both models and the fix — no cryptic failures.

**Grounded in the actual corpus.** The server answers from indexed source material, not from whatever the underlying model memorized at training time. When the Midnight SDK ships a new package or the Compact language changes, the corpus is updated and the server's answers change accordingly — without any model retrain.

## What your AI client gains

An AI client connected to this MCP server can:

- Search the Midnight corpus with a single natural-language question
- Walk from a search hit to its surrounding document context, chunk by chunk
- Filter results by source, language, package, SDK version, and more
- Discover what material exists in the corpus before constructing a query
- Check service health and rate-limit state before a long session

The [Advanced Search skill](./advanced-search-skill.md) teaches your client the technique for combining these tools like a seasoned researcher, rather than firing one query and hoping.

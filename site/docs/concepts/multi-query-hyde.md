---
title: Multi-query & HyDE
sidebar_label: Multi-query / HyDE
description: How advanced_search fuses multiple query formulations with RRF to improve recall — including hypothetical document embeddings and step-back rephrases.
---

# Multi-query & HyDE

A single query rarely captures all the ways a corpus might phrase the answer you want. `advanced_search` accepts an array of queries and fuses them into one ranked result list, using the same [Reciprocal Rank Fusion](./hybrid-retrieval.md) mechanism. This supports two patterns: **multi-query retrieval** and **HyDE** (hypothetical document embeddings).

## How multi-query works

`advanced_search` accepts 1–10 queries in a single call. The server:

1. Runs each query independently across both retrieval modes (lexical and semantic).
2. Fuses all the resulting candidate lists together with RRF (`k = 60`), in a single pass.
3. De-duplicates hits that appear in multiple lists (they earn a higher fused score from multiple lists rather than appearing twice).
4. Returns a unified ranked result with diagnostics showing which queries contributed.

The rate-limit cost is `max(1, N)` distinct queries, so a two-query call costs two query credits, not one. This is deliberate: the server does real work per formulation and bills for it.

## HyDE: hypothetical document embeddings

HyDE works because embedding a hypothetical answer to your question often retrieves better results than embedding the question itself.

Questions and answers live in different linguistic registers. A question ("how do I check authorization in a Compact circuit?") may embed poorly against a passage that directly answers it ("the `ownPublicKey()` function returns the caller's public key; compare it against a stored authorized key"). A hypothetical answer written in the same register as the documentation embeds far better.

**Example: pairing a literal query with a HyDE answer**

```json
{
  "queries": [
    "how do I restrict a circuit to a single authorized caller",
    "To restrict a circuit to a single authorized caller, store the authorized key on the ledger and compare it with ownPublicKey() inside the circuit body. Assert equality to reject unauthorized calls."
  ]
}
```

The literal query catches any passage that uses those exact words. The hypothetical answer catches passages that describe the pattern in documentation-style prose. RRF fuses both, and passages that appear in both lists rank higher.

## Step-back rephrasing

A step-back rephrase moves from the specific to the general. If your literal query targets a specific error message, a step-back asks about the underlying concept, which raises the chance of finding background documentation that explains it.

**Example: specific query + step-back**

```json
{
  "queries": [
    "mnm search exits with code 1 and no error message",
    "exit codes and error handling in mnm CLI commands"
  ]
}
```

The specific query finds the exact passage if it exists. The step-back finds the broader section on error handling, which may not mention the specific exit code but contains the context needed to diagnose it.

## Reading the diagnostics

Every `advanced_search` response includes `search_metadata.per_query` diagnostics, one entry per formulation, showing how many candidates each query contributed and their score distribution. Each result also carries `scores.matched_queries`, an array of indices marking which of your queries matched it.

Use these diagnostics to see whether all your formulations are pulling their weight. A formulation with zero matches can be revised or dropped. A formulation that matches but ranks lower than you expected may point to a corpus gap.

## When to use multi-query

Reach for multi-query on a few specific shapes of question:

- You're unsure how the corpus phrases a concept. Send both the user's words and the documentation's likely words.
- You have a narrow literal question and a broad contextual one. Pair them to catch the exact hit and its surrounding context.
- You want more recall before reranking. More candidates in the RRF pool gives the reranker ([`rerank-2.5`](./models.md)) more chances to surface the right passage.
- You're working through the [Advanced Search skill](../mcp/advanced-search-skill.md), which bundles these techniques into a reusable retrieval playbook for your AI client.

A single well-formed query is often enough. Multi-query pays off on hard questions where coverage matters.

## Related pages

- [Hybrid retrieval & RRF](./hybrid-retrieval.md) — the fusion mechanism that combines all the query lists.
- [Models](./models.md) — the reranker that sharpens the fused candidate set.
- [Advanced Search skill](../mcp/advanced-search-skill.md) — the bundled skill that teaches your AI client these techniques.

---
title: Hybrid retrieval & RRF
sidebar_label: Hybrid retrieval & RRF
description: How Midnight Manual fuses lexical and semantic candidate lists with Reciprocal Rank Fusion to catch both exact-term and conceptual matches.
---

# Hybrid retrieval & RRF

No single retrieval mode is best for every query. A pure keyword search misses paraphrases and conceptual synonyms. A pure vector search misses exact identifiers, function names, and error codes. Midnight Manual runs both in parallel and fuses the results, so exact-term hits and conceptual hits land in one ranked list.

## Two candidate lists

Every `search` call (with `mode=hybrid`, the default) produces two independent ranked lists:

1. **Lexical**: PostgreSQL full-text search over chunk text. It finds passages that contain the exact terms in your query, or close morphological variants, and it's fast. Strong for API names, error codes, compiler flags, and any term with a specific meaning the embedder might blur.

2. **Semantic**: pgvector cosine similarity over chunk embeddings. It finds passages conceptually close to your query even when the surface wording differs, catching "how do I check if a user is authorized" when the documentation says "access control" or "permission check" instead.

For code-heavy queries there is a third list: the **code vector** list from `voyage-code-3`. The `code_mode` parameter (`on` / `off` / `exclusive`) controls whether this list participates in fusion: `on` adds it to the hybrid pool; `exclusive` replaces the general semantic list with it for highly identifier-shaped queries. See [Models](./models.md) for details on the two embedding models.

## Reciprocal Rank Fusion

The candidate lists are merged with **Reciprocal Rank Fusion** (RRF). For each document `d` across lists `L`:

```
RRF(d) = Σ_L  1 / (k + rank_L(d))
```

where `k = 60` is the canonical smoothing constant from the original RRF paper. Documents that don't appear in a list contribute nothing from that list. The scores are then normalized into `[0, 1)`.

Using `1 / (k + rank)` rather than the raw score is deliberately rank-agnostic: a document ranked 1st gets the same treatment whether its raw score was 0.99 or 0.51. This makes the fusion stable across retrieval modes with incompatible score scales.

### Choosing k = 60

The value 60 is not arbitrary; it appears across the retrieval literature as the constant that best balances the influence of high-ranked items against the long tail. A smaller `k` would concentrate weight on the top-1 result; a larger one would dilute the benefit of a strong first-place ranking. Midnight Manual uses the same constant for both the per-mode fusion and the multi-query fusion in `advanced_search`.

## Reranking on top

After RRF, the merged candidate list is optionally re-scored by the VoyageAI reranker (`rerank-2.5` by default). The reranker is a cross-encoder: it reads the query and each candidate passage together and predicts relevance, which is more accurate than embedding distance alone. Reranking is on by default; it can be placed locally (BYOK) or server-side, or turned off entirely.

On any rerank failure the server **degrades gracefully to RRF order** and flags the reason in the response. The search never silently fails: it either returns reranked results or explains why it fell back to RRF order.

See [Models](./models.md) for placement options and the `rerank-2.5-lite` alternative.

## Retrieval modes

| Mode | What it uses |
|---|---|
| `hybrid` (default) | Lexical + semantic vector (+ code vector if `code_mode=on`) |
| `vector` | Semantic vector only (+ code vector if `code_mode=on`) |
| `fts` | Lexical only; `code_mode` is forced `off` |

Switch modes with `--mode <mode>` on the CLI or the `mode` parameter in MCP tool calls.

## Confidence after retrieval

The RRF score feeds into the [confidence formula](./confidence.md): relevance × trust. The final ranking you see is not pure retrieval rank; it is retrieval rank modulated by source attribution, freshness, verification, and version match.

## Related pages

- [Confidence = trust × relevance](./confidence.md) — how provenance signals multiply the retrieval score.
- [Models](./models.md) — the two embedding models and the reranker that sharpen the list.
- [Multi-query / HyDE](./multi-query-hyde.md) — running multiple queries and fusing them with this same RRF mechanism.

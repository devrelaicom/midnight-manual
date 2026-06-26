---
title: Searching
sidebar_label: Searching
description: The search and advanced_search tools — query modes, filters, code_mode, and what each result contains.
---

# Searching the corpus

The MCP server exposes two search tools: `search` for quick lookups and `advanced_search` for full control. Both query the same hosted corpus and return the same result shape.

## `search` — the simple surface

`search` covers the common case. It takes a single query string, always reranks, and applies no filters.

```
search(query, mode?, code_mode?, limit?)
```

| Parameter | Default | Notes |
|---|---|---|
| `query` | required | Natural language or code terms. |
| `mode` | `hybrid` | `hybrid` fuses keyword + semantic; `fts` is keyword-only (lowest latency); `vector` is semantic-only. |
| `code_mode` | `on` (for hybrid/vector) | `on` fuses a `voyage-code-3` ranked list alongside general results; `off` = general retrieval only; `exclusive` = code vectors replace the general vector list (best for API-shaped or code-identifier queries). Incompatible with `mode=fts`. |
| `limit` | `10` | Max results returned, capped at 50. |

For multi-query strategies, facet filters, or rerank control, use `advanced_search`.

## `advanced_search` — full control

`advanced_search` exposes every retrieval knob. Use it when basic search comes up short, or when the [Advanced Search skill](./advanced-search-skill.md) prescribes a specific pattern.

```
advanced_search(queries, mode?, code_mode?, limit?, rerank?, rerank_instructions?, filters?, version_match?)
```

| Parameter | Default | Notes |
|---|---|---|
| `queries` | required | Array of 1–10 query strings fused with RRF. One query = one-element array. Rate-limit cost is one token per distinct query. |
| `mode` | `hybrid` | Same three modes as `search`. |
| `code_mode` | `on` (for hybrid/vector) | Same three values as `search`. |
| `limit` | `10` | Max results returned, capped at 50. |
| `rerank` | `true` | Apply VoyageAI reranking. Set `false` for lowest latency. |
| `rerank_instructions` | derived | Optional instruction (max 400 chars) to guide the reranker — emphasize aspects, filter document kinds, or disambiguate terms. |
| `filters` | none | Per-facet filters; see below. |
| `version_match` | `permissive` | `permissive` biases ranking and drops only breaking mismatches; `strict` hard-filters to satisfying content only. |

### Filters

`filters` restricts results by corpus metadata. Keys are ANDed; values within `any_of` are ORed; `none_of` excludes. Call [`facets`](./corpus-diagnostics.md) first to discover valid values for open-set facets like `source_slug`, `language`, `package`, `language_target`, and `sdk_dependency`.

Fixed-set facets you can use directly:

| Facet key | Valid values |
|---|---|
| `kind` | `docs_site`, `code_repo`, `standalone`, `mixed` (source kind) |
| `attribution` | corpus-defined attribution tiers |
| `content_type` | corpus-defined content type values |
| `verified` | boolean |
| `deprecated` | boolean |

Version-aware filters (`language_target`, `sdk_dependency`) accept a `version_satisfies` semver range, matched against the version constraints extracted from Compact pragmas and package manifests.

## Query modes explained

| Mode | What it does | When to use |
|---|---|---|
| `hybrid` | Fuses full-text (PostgreSQL FTS) and semantic (pgvector) via RRF | Default — balances exact-match and conceptual recall |
| `fts` | Keyword-only, no embedding call | Exact identifiers, error codes, package names; lowest latency |
| `vector` | Semantic-only, no lexical pass | Conceptual questions where vocabulary mismatch is likely |

## What a result contains

Every search result includes:

- **`chunk_id`** — UUID for use with chunk navigation tools
- **`document_id`** — UUID for use with document tools
- **`content`** — the excerpt text
- **`confidence`** — blended retrieval score
- **`trust_score`** — a composite of attribution, verification, freshness, deprecation, and version-match factors
- **`confidence_factors`** — per-factor breakdown so your assistant can explain why a result is (or isn't) trustworthy
- **`source`** — slug, display name, and kind
- **`symbol_path`** — structured location within the source (useful for code hits)

For multi-query searches, a `rerank_score` field is added when reranking ran.

## Tips

- For code-shaped queries (function names, API signatures, error strings from code), set `code_mode=exclusive`.
- For conceptual questions, keep the default (`hybrid` + `code_mode=on`).
- For exact version strings or error codes, use `mode=fts`.
- When results are thin, check [`status`](./corpus-diagnostics.md) and consider using the [Advanced Search skill](./advanced-search-skill.md) for HyDE or multi-query expansion.

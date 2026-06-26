---
title: Models
sidebar_label: Models
description: The VoyageAI embedding and reranking models used by Midnight Manual — what they are, where they run, and how to bring your own key.
---

# Models

Everything runs remotely on VoyageAI: **nothing is downloaded, run, or cached on your machine**. No Python, no ONNX, no model files, no GPU. The CLI starts fast because there is nothing local to load.

## The three models

| Role | Model | Where it runs | Notes |
|---|---|---|---|
| General embedder | `voyage-context-3` | VoyageAI (remote) | Contextualized embeddings: each document's chunks are embedded together, so every chunk vector carries document-level context. 1024-dimensional by default; Matryoshka-configurable at 256 / 512 / 1024 / 2048. A query is embedded as a single-chunk document. |
| Code embedder | `voyage-code-3` | VoyageAI (remote) | A second vector on code chunks. At query time, `code_mode` (`on` / `off` / `exclusive`) decides whether this code-vector list joins the RRF fusion. `on` is the hybrid and vector default; `exclusive` swaps in the code list for API-shaped or identifier-heavy queries. Forced `off` when `mode=fts`. |
| Reranker | `rerank-2.5` (or `rerank-2.5-lite`) | VoyageAI (remote) | Used when reranking is requested (on by default). `rerank-2.5-lite` has lower latency and is billed at half tokens server-side. `rerank: none` keeps RRF order. |

### Dual contextualized embeddings

General corpus chunks use `voyage-context-3`. Code chunks get an additional `voyage-code-3` vector. Both ranked lists fuse via [RRF](./hybrid-retrieval.md), gated per request by `code_mode`. The `models.cache_dir` config setting only governs a (now-empty) cache directory; nothing is fetched there.

### Version-aware embedding

The corpus advertises its active embedding model as `name@revision` (for example, `voyage-context-3@1`). If the corpus rolls forward to a new model revision, clients are told to re-embed against the new model rather than silently returning mis-scored results. The CLI's `mnm models active` command shows which model the corpus is currently on.

## Reranking: placement and models

Reranking is a VoyageAI call. When it runs (on by default), the query, any `rerank_instructions`, and the candidate passages reach Voyage. Where the call originates depends on your placement:

| `--rerank` | When `auto` picks it | What happens |
|---|---|---|
| `server` | No Voyage key set | The hosted server reranks inline in `/v1/search` under its own Voyage key, charged to your token budget. |
| `local` | A Voyage key is set | Your client calls Voyage's `/v1/rerank` directly under your own account (BYOK). |
| `off` | — | No rerank anywhere; results stay in RRF order. |

`--rerank auto` is the default. Pick the model with `--rerank-model rerank-2.5` (default) or `rerank-2.5-lite` (lower latency, billed at half tokens server-side). Steer relevance with `--rerank-instructions "<text>"` (maximum 400 characters). The same knobs live in config under `[rerank]` (`location`, `model`) and in the `MIDNIGHT_MANUAL_RERANK` / `MIDNIGHT_MANUAL_RERANK_MODEL` environment variables.

On any rerank failure the server **degrades gracefully to RRF order and flags the reason** rather than failing the search. A rerank outage never breaks retrieval.

## Bringing your own key (BYOK)

With a `VOYAGE_API_KEY` configured, your client embeds queries and reranks results directly against your own Voyage account, bypassing the server's token budget for both operations. Set it any of these ways:

```bash
export VOYAGE_API_KEY=…                  # environment variable
mnm search "…" --voyage-api-key …        # per-invocation flag
```

```toml
# config file (lowest precedence)
[models]
voyage_api_key = "…"
```

Precedence is the standard ladder: flag beats env beats config.

BYOK embedding applies during ingest too. For bulk runs, setting `VOYAGE_API_KEY` routes embedding through your own Voyage account rather than consuming the server's token budget. See [Running an ingest](/docs/self-hosting/running-an-ingest) for bulk ingest guidance.

## Inspecting the active model

```bash
mnm models active    # shows which embedding model the corpus is on
mnm models pull      # ensures the model-cache directory exists (nothing is fetched)
```

`mnm models pull` is a no-op for the remote-only embedding setup; it just creates the cache directory. Its presence keeps the command safe to run in CI pipelines without changes.

## Config reference

```toml
[models]
embedding      = "voyage-context-3"  # remote VoyageAI general embedder
code_embedding = "voyage-code-3"     # remote VoyageAI code embedder
# voyage_api_key = "…"               # optional — BYOK embedding + reranking
# voyage_timeout_secs = 120          # optional — per-request Voyage embed timeout

[rerank]
location = "auto"          # auto | local | server | off
model    = "rerank-2.5"    # rerank-2.5 | rerank-2.5-lite
```

## Related pages

- [Hybrid retrieval & RRF](./hybrid-retrieval.md) — how the two vector lists are fused.
- [Confidence = trust × relevance](./confidence.md) — how reranked results are further adjusted by trust signals.
- [Smart chunker](./smart-chunker.md) — what gets embedded in the first place.

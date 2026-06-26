---
title: Models
sidebar_label: Models
description: mnm models — inspect the corpus's active embedding model and understand VoyageAI remote processing.
---

# Models

`mnm models` lets you inspect the corpus's embedding model and prime the local cache directory. Both the embedder and the reranker run remotely on VoyageAI — **nothing is downloaded, run, or cached on your machine** (no Python, no ONNX, no model files, no GPU).

## The model landscape

| Role | Model | Where it runs | Notes |
|---|---|---|---|
| General embedder | `voyage-context-3` | VoyageAI (remote) | Contextualized embeddings: each document's chunks are embedded together, so every chunk vector carries document-level context. 1024-dimensional by default (Matryoshka-configurable). A query is embedded as a single-chunk document. |
| Code embedder | `voyage-code-3` | VoyageAI (remote) | A second vector on code chunks. At query time `code_mode` (`on`/`off`/`exclusive`) decides whether this code-vector list joins the RRF fusion. Forced `off` for `mode=fts`. |
| Reranker | `rerank-2.5` (or `rerank-2.5-lite`) | VoyageAI (remote) | Used when reranking is requested (on by default). `rerank-2.5-lite` is lower latency and billed at half tokens server-side. |

The corpus advertises its active embedding model as `name@revision` (e.g. `voyage-context-3@1`). If the corpus rolls the model forward, clients are told to re-embed against the new model rather than silently returning mis-scored results.

## `mnm models pull`

Ensures the local model-cache directory exists. Because both models are remote VoyageAI, nothing is fetched — this subcommand only primes the directory.

```bash
mnm models pull
mnm models pull --cache-dir /path/to/cache
mnm models pull --json
```

| Flag | Notes |
|---|---|
| `--cache-dir <path>` | Override the local cache directory. Precedence: this flag > config `[models].cache_dir` > `MIDNIGHT_MANUAL_MODEL_CACHE_DIR` env > `XDG_DATA_HOME/midnight-manual/models` > `HOME/.local/share/midnight-manual/models`. |

## `mnm models active`

Fetches the corpus's currently active embedding model from the server. Use this to verify that your local configuration matches what the corpus is embedded with.

```bash
mnm models active
mnm models active --json
```

Example output:

```
corpus active embedding model:
  wire id:   voyage-context-3@1
  name:      voyage-context-3
  revision:  1
  dim:       1024
  provider:  voyageai
```

The `wire_id` field in `--json` output is the `name@revision` string that labels search requests. The CLI resolves this automatically at search time (a `GET /v1/models/active` call), so you only need this subcommand when diagnosing embedding-model mismatch errors.

## Embedding paths

Which account processes your query text depends on whether you supply a Voyage key:

| Path | When it applies | What happens |
|---|---|---|
| **BYOK** | `VOYAGE_API_KEY` set (or `--voyage-api-key`) | Your client embeds directly against your own Voyage account. Reranking also runs locally under your account. |
| **Server-proxy** | no Voyage key | Your client POSTs query text to the hosted server's `/v1/embeddings`, which calls Voyage under the operator's platform account. |

Either way the query text reaches Voyage; the only question is whose Voyage account processes it. There is no on-device embedding path.

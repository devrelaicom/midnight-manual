---
title: Embeddings & third-party processing
sidebar_label: Embeddings
---

# Embeddings and third-party processing

Telemetry is the easy half of the privacy story: it carries no content at all. Embedding is the harder half, because a search query *is* content, and turning it into a vector means a model has to read it. This page documents exactly where your text goes.

Source: README `## Embeddings & third-party processing` section.

---

## The corpus is public

The indexed corpus is built from **public Midnight repositories**: the docs site and open-source code. Nothing private is in it, and nothing you search reveals anything to other users. What follows is only about where *your query text* travels on its way to a vector.

---

## Two embedding paths

Query embedding uses VoyageAI's contextualized `voyage-context-3` model (1024-dimensional), with a second `voyage-code-3` vector for code chunks. Reranking, when enabled (on by default), also goes through VoyageAI. Which path your query text takes depends on whether you supply your own Voyage key:

| Path | When it applies | What text reaches Voyage |
|---|---|---|
| **BYOK** (bring your own key) | A Voyage key is configured | Your client embeds directly against **your own** Voyage account; query text is sent to Voyage under your account. |
| **Server-proxy** | No Voyage key configured | Your client POSTs raw query text to the hosted server's `/v1/embeddings`, which calls Voyage under the **operator's** platform account. Your query text reaches Voyage under their account. |

Either way the query text reaches Voyage; the only question is *whose* Voyage account processes it. There is no path that embeds entirely on your machine; the embedder is remote by design.

The server records only **token counts** against a **subject key** (the client IP or your SSO user id) for budget accounting. It never logs or persists the submitted query text.

---

## Server-side reranking

When server-side reranking is enabled (the default), the search query (plus any `rerank_instructions`) and the text of candidate result chunks are sent to VoyageAI's rerank API. This is the same third-party exposure class as the embeddings proxy. To opt out:

- Send `rerank: false` in `advanced_search` (MCP tool)
- Pass `--rerank off` on the CLI
- Rerank locally with your own `VOYAGE_API_KEY` (BYOK path)

---

## BYOK setup

Set a Voyage key any one of these ways and your client embeds directly, bypassing the server proxy:

```bash
# Environment variable
export VOYAGE_API_KEY=…

# Per-invocation flag (CLI)
mnm search "…" --voyage-api-key …
```

```toml
# Config file (lowest precedence)
[models]
voyage_api_key = "…"
```

Precedence is the standard **flag › env › config**.

---

## Reranking — placement and models

Reranking is a VoyageAI call. When it runs (on by default), the query, any `rerank_instructions`, and the candidate passages reach Voyage; this is the same third-party exposure class as embedding. Where the call originates depends on your placement:

| Placement | When `auto` picks it | What happens |
|---|---|---|
| `server` | No Voyage key set | The hosted server reranks inline in `/v1/search` under its Voyage key, charged to your token budget. |
| `local` | A Voyage key is set | Your client calls Voyage's `/v1/rerank` directly under your own account (BYOK). |
| `off` | — | No rerank anywhere; results stay in RRF order. |

`auto` is the default placement. Configure the model and placement:

- Model: `rerank-2.5` (default) or `rerank-2.5-lite` (lower latency, billed at half tokens server-side)
- Steer relevance with `--rerank-instructions` (max 400 chars)
- Config keys: `[rerank].location` and `[rerank].model`
- Env vars: `MIDNIGHT_MANUAL_RERANK` and `MIDNIGHT_MANUAL_RERANK_MODEL`

See the [Configuration reference](/docs/reference/configuration) for the full key list.

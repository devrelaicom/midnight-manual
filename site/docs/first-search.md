---
id: first-search
title: Your first search
sidebar_position: 4
---

# Your first search

Once `mnm` is [installed](/docs/install), you can search the Midnight corpus from your terminal without any client setup:

```bash
mnm search "how do I mint a shielded token?"
```

## What you get back

`mnm search` returns ranked, source-attributed results straight away. Each result includes:

- A **confidence score** so you know how well the passage matches your query.
- A **provenance breakdown** — a one-line summary of where the result came from and why it was ranked where it was.
- The **source path** — the exact doc or source file the passage was pulled from.

Both embedding and reranking run through VoyageAI, proxied by the hosted server, so no API key is needed and no local model is downloaded. Reranking is on by default (`rerank-2.5`); pass `--rerank off` for lowest latency.

## Next steps

- [Add mnm to your AI client](/docs/add-to-ai-client) to give your assistant live Midnight search in every conversation.
- Not installed yet? See [Install mnm](/docs/install).

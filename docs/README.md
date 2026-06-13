# docs/

This directory contains **operator and service-provider documentation** for
midnight-manual — the guides for people deploying and maintaining the server
or ingesting corpus content.

| Document | Audience | Purpose |
| --- | --- | --- |
| [README-deploy.md](README-deploy.md) | Operators | First-time Fly.io deploy runbook: provision Postgres, set secrets, deploy, smoke-test, and ingest. |
| [cookbook/ingesting-content.md](cookbook/ingesting-content.md) | Corpus maintainers | Day-to-day ingestion: author a manifest, run `mnm ingest`, re-run idempotently, override source defaults. |

## What lives elsewhere

- **End-user search guidance** — the project README covers installing the CLI
  and MCP server, connecting to the hosted corpus, and running searches.
- **Advanced retrieval techniques** (HyDE, multi-query, version-matched search,
  `code_mode`) — bundled as the `midnight-advanced-search` skill; install it
  with `mnm skills add` or the MCP `install_search_skill` tool.
- **Landing page** — [https://manual.midnightntwrk.expert](https://manual.midnightntwrk.expert)
- **Hosted search API** — [https://midnight-manual.midnightntwrk.expert](https://midnight-manual.midnightntwrk.expert)

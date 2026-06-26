---
title: Searching
sidebar_label: Searching
description: mnm search — query modes, filters, reranking, and machine-readable output.
---

# Searching with `mnm search`

`mnm search` runs an ad-hoc retrieval against the hosted corpus. It supports the same retrieval modes and filter knobs as the [MCP search tools](../mcp/searching.md), and works with `--json` for scripting.

## Basic usage

```bash
mnm search "<query>"
mnm search "<query>" --limit 5
mnm search "<query>" --json
```

A positional query is required unless you use `--queries-stdin` (see below).

## Search flags

| Flag | Default | Notes |
|---|---|---|
| `<query>` | required | The primary query string. |
| `--query <text>` | — | Repeatable; adds extra queries for multi-query RRF fusion. |
| `--queries-stdin` | off | Read `{ "queries": ["…", …] }` from stdin instead of positional args. Mutually exclusive with the positional query and `--query`. |
| `--limit <n>` | `10` | Maximum results. Capped at 100 server-side. |
| `--mode <mode>` | `hybrid` | `hybrid`, `vector`, or `fts`. |
| `--code-mode <mode>` | — | `on`, `off`, or `exclusive`. Incompatible with `--mode fts`. |
| `--rerank <placement>` | `auto` | `auto`, `local`, `server`, or `off`. `auto` picks local BYOK reranking when a Voyage key is present, otherwise server-side. |
| `--rerank-model <model>` | — | `rerank-2.5` (default) or `rerank-2.5-lite` (lower latency, billed at half tokens server-side). |
| `--rerank-instructions <text>` | derived | Natural-language rerank instruction (max 400 chars). |
| `--version-match <mode>` | — | `permissive` or `strict`. Only meaningful with a version-bearing `--filter-json`. |
| `--embedding-model <id>` | `auto` | Override the embedding model wire id. Leave as `auto` to use the corpus's active model. |

## Filters

Use the granular filter flags to restrict results by corpus metadata:

| Flag | Notes |
|---|---|
| `--kind <kind>` | Repeatable; restrict to chunk kinds (`markdown`, `code`, `plaintext`). |
| `--language <lang>` | Repeatable; restrict to programming languages. |
| `--exclude-language <lang>` | Repeatable; exclude these languages. |
| `--tag <tag>` | Repeatable; restrict to these tags. |
| `--exclude-tag <tag>` | Repeatable; exclude these tags. |
| `--symbol <kind:name>` | Repeatable; match symbols as `kind:name`, either side optional (e.g. `circuit:` or `:deployContract`). |
| `--source <slug>` | Repeatable; restrict to these source slugs. |
| `--content-type <type>` | Repeatable. |
| `--attribution <tier>` | Repeatable. |
| `--no-deprecated` | Exclude deprecated content. |
| `--verified` | Restrict to verified content. |
| `--ingested-after <YYYY-MM-DD>` | Only chunks ingested on or after this date. |
| `--ingested-before <YYYY-MM-DD>` | Only chunks ingested on or before this date. |
| `--min-tokens <n>` | Minimum chunk token count. |
| `--max-tokens <n>` | Maximum chunk token count. |
| `--filter-json <json>` | Full filter object as JSON (mutually exclusive with the granular flags; use `mnm facets` to discover valid values). |

Run `mnm facets` to see the corpus's valid facet keys and values before filtering.

## Machine-readable output

Add `--json` to any search invocation for structured output suitable for scripts and pipelines. Each call emits a JSON document to stdout. The shape includes a `results` array; each result carries a `chunk_id` (use it with [`mnm chunks`](./reading.md)), a `document_id`, the `content` text, a `confidence` score, and source metadata.

```bash
# Search and pipe to jq
mnm search "nullifier double-spend prevention" --limit 5 --json \
  | jq '.results[] | .chunk_id'

# Multi-query search via stdin
echo '{"queries":["Compact ledger types","Compact ADT counter"]}' \
  | mnm search --queries-stdin --json
```

## Reranking

By default, reranking runs automatically: `--rerank auto` picks local BYOK reranking when a Voyage key is present (`VOYAGE_API_KEY` or `--voyage-api-key`), and falls back to server-side reranking otherwise. Both paths use VoyageAI; the only difference is whose account is billed.

- `--rerank off` skips reranking and returns results in RRF order, the lowest-latency path.
- `--rerank local` requires a Voyage key and is rejected with an error if one is absent.
- `--rerank-model rerank-2.5-lite` is faster and billed at half tokens server-side.

When reranking degrades (e.g. the Voyage API is unavailable), the server falls back to RRF order and flags the reason in the response rather than failing the search.

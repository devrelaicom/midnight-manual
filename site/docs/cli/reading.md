---
title: Reading content
sidebar_label: Reading content
description: mnm chunks and mnm documents — follow neighbours, size a document, and read window by window.
---

# Reading content

Two command groups let you read beyond a search hit: `mnm chunks` walks the chunk graph by ID, and `mnm documents` gives you a document overview and a windowed view of its full chunk list.

## `mnm chunks` — walk the chunk graph

Every search result carries a `chunk_id`. Use these subcommands to read around it.

### `mnm chunks show <chunk-id>`

Fetch and render one chunk with its document and source context.

```bash
mnm chunks show 019682f0-1234-7abc-8def-0123456789ab
mnm chunks show 019682f0-1234-7abc-8def-0123456789ab --json
```

### `mnm chunks next <chunk-id>`

Fetch the next N chunks after the anchor in the same document.

```bash
mnm chunks next <chunk-id>              # default: 5 chunks
mnm chunks next <chunk-id> --count 10  # up to 100 server-side
mnm chunks next <chunk-id> --full      # full content (default is a 240-char preview)
```

| Flag | Default | Notes |
|---|---|---|
| `--count <n>` | `5` | Number of chunks to fetch (clamped to `1..=100` server-side). |
| `--full` | off | Show full chunk content instead of a 240-character preview. |

### `mnm chunks prev <chunk-id>`

Same flags as `next`, but fetches chunks before the anchor.

```bash
mnm chunks prev <chunk-id> --count 3
```

### `mnm chunks neighbors <chunk-id>`

Fetch prev + anchor + next in one call. Equivalent to running `prev`, `show`, and `next` separately but with a single round-trip.

```bash
mnm chunks neighbors <chunk-id>
mnm chunks neighbors <chunk-id> --count 5 --full
```

## `mnm documents` — read documents

Every search result also carries a `document_id`. Use these subcommands to understand the document's shape and read it in windows.

### `mnm documents show <doc-id>`

Show the document overview: metadata, total chunk count, and the ordered chunk skeleton (index, heading path, and preview for each chunk). This is the right starting point when you want to understand a document's structure before reading it in depth.

```bash
mnm documents show <doc-id>
mnm documents show <doc-id> --json
```

### `mnm documents chunks <doc-id>`

Render a windowed slice of the document's chunks — useful for reading long documents section by section.

```bash
mnm documents chunks <doc-id>                      # default: first 20 chunks
mnm documents chunks <doc-id> --from 20 --limit 20 # second window
```

| Flag | Default | Notes |
|---|---|---|
| `--from <n>` | `0` | Starting chunk index offset (0-based). |
| `--limit <n>` | `20` | Maximum chunks to return in this window. |

## Typical workflow

A common pattern is to search for a topic, identify a relevant hit, then read the surrounding context:

```bash
# 1. Search — grab a chunk_id and document_id from the results
mnm search "Compact ledger Map ADT" --limit 3 --json \
  | jq '{chunk: .results[0].chunk_id, doc: .results[0].document_id}'

# 2. Follow the chunk's neighbours to read around the hit
mnm chunks next <chunk-id> --count 10

# 3. Size the document to understand how much there is
mnm documents show <doc-id>

# 4. Read window by window
mnm documents chunks <doc-id> --from 0 --limit 20
mnm documents chunks <doc-id> --from 20 --limit 20
```

All of these commands support `--json` for scripting.

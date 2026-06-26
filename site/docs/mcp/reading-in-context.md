---
title: Reading in context
sidebar_label: Reading in context
description: The seven tools for walking from a search hit to its surrounding text — get_chunks, get_chunk_next/prev/neighbors/parents, get_document, get_document_chunks.
---

# Reading a hit in context

A search result is a **chunk**, a slice of a document. These tools let your assistant pull exactly as much surrounding context as it needs, instead of dumping whole files into the context window.

The typical flow is:

1. Search -> get `chunk_id` values in results
2. `get_chunks` — read the full text behind those ids
3. `get_chunk_neighbors` or `get_chunk_next`/`get_chunk_prev` — expand outward if the hit needs more context
4. `get_chunk_parents` — find which document and section a chunk belongs to
5. `get_document` + `get_document_chunks` — read a full document section by section when needed

## `get_chunks` — read search results

Fetch the full content of 1–20 chunks by id in one batched call. This is the standard next step after a search.

```
get_chunks(ids)
```

| Parameter | Notes |
|---|---|
| `ids` | Array of 1–20 chunk UUIDs (from search results or other chunk tools). One id = a one-element array. |

Do not fetch chunks one at a time; batch the top hits into a single call.

## `get_chunk_next` and `get_chunk_prev` — walk in reading order

Walk forward or backward `count` chunks from an anchor chunk, in document reading order. Skips `embed_failed` gaps.

```
get_chunk_next(id, count?)
get_chunk_prev(id, count?)
```

| Parameter | Default | Notes |
|---|---|---|
| `id` | required | Anchor chunk UUID. |
| `count` | `5` | Chunks to return; range 1–100. Walking past the document edge returns an empty list, not an error. |

Use `get_chunk_next` to continue reading past the end of a chunk you already have. Use `get_chunk_prev` to read the context leading up to a chunk.

## `get_chunk_neighbors` — both sides in one call

Fetch a chunk plus `count` neighbours on each side (`prev` + the chunk + `next`) in one round-trip.

```
get_chunk_neighbors(id, count?)
```

| Parameter | Default | Notes |
|---|---|---|
| `id` | required | Anchor chunk UUID. |
| `count` | `2` | Chunks to fetch on each side; range 1–100. A side that runs past the document edge comes back empty, not as an error. |

Use this when a search hit needs surrounding context to make sense. It saves two round-trips compared to calling `get_chunk_next` and `get_chunk_prev` separately.

## `get_chunk_parents` — orient a chunk in its source

Walk the parent chain from a chunk up to the source-version root: document, folders, source.

```
get_chunk_parents(id)
```

| Parameter | Notes |
|---|---|
| `id` | Chunk UUID. |

The response is a chain of parent nodes, each with `id`, `name`, `kind`, and (for document-kind parents) a `document_id` you can hand directly to `get_document`. Use this to answer "where does this chunk live?" before deciding whether to read more of the document.

## `get_document` — document overview

Fetch a document's metadata plus an ordered skeleton of its chunks: id, chunk position, and token count, but no chunk bodies.

```
get_document(id)
```

| Parameter | Notes |
|---|---|
| `id` | Document UUID (from search results or a `get_chunk_parents` response). |

Use `get_document` to size up a document before reading it. The skeleton tells you how many chunks there are and how large each one is, so you can plan how many `get_document_chunks` calls you need.

## `get_document_chunks` — read a document section by section

Fetch a windowed slice of a document's chunk bodies, paginated by position.

```
get_document_chunks(id, from?, limit?)
```

| Parameter | Default | Notes |
|---|---|---|
| `id` | required | Document UUID. |
| `from` | `0` | Zero-based chunk position to start from. A position past the end returns an empty window with accurate `total_chunks`, not an error. |
| `limit` | `20` | Chunk bodies to return; range 1–100. |

Use this after `get_document` to read a document section by section. Start at `from=0` and advance `from` by `limit` each call until you have what you need.

## Workflow example

```
# 1. Search for a topic
search("how does disclose work in Compact")
→ results with chunk_id values

# 2. Read the top hits
get_chunks(ids=["<chunk_id_1>", "<chunk_id_2>"])

# 3. The first hit cuts off mid-explanation — get what follows
get_chunk_next(id="<chunk_id_1>", count=3)

# 4. Find out which document this belongs to
get_chunk_parents(id="<chunk_id_1>")
→ document_id: "<doc_id>"

# 5. Size the document before reading it fully
get_document(id="<doc_id>")
→ 12 chunks, ~800 tokens total

# 6. Read the document in two passes
get_document_chunks(id="<doc_id>", from=0, limit=6)
get_document_chunks(id="<doc_id>", from=6, limit=6)
```

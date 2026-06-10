---
name: midnight-advanced-search
description: >-
  Advanced retrieval playbook for the Midnight Network documentation corpus.
  Use whenever searching, researching, or answering questions about Midnight,
  Compact, the Midnight SDK, or the corpus exposed by the midnight-manual MCP
  server (search, advanced_search, facets, get_chunks, get_document,
  list_sources). Teaches query modes (hybrid/vector/fts), multi-query fusion
  with advanced_search, per-facet filters with discovery and fail-fast
  recovery, version- and freshness-matched retrieval to avoid stale answers,
  plus HyDE, multi-query, step-back, symbol-aware code search, and
  trust-weighted selection — so you find authoritative, version-matched
  answers instead of firing one naive query.
metadata:
  source: midnight-manual
---

# Midnight advanced search

You have a hybrid retrieval surface over the Midnight corpus (full-text + vector,
RRF-fused, optional cross-encoder rerank, trust-aware scoring), per-request query
modes, a discoverable filter set, and chunk/document navigation. This is the
playbook for using it like a researcher.

## The tools you have

Thirteen tools, four groups:

**Search**
- `search` — quick lookups: `{query, mode?, limit?}` and nothing else. One
  query string, always reranked, no filters. Use when one plain question will
  do.
- `advanced_search` — the full-control surface and this skill's main subject:
  `{queries: [1–10 strings], mode?, limit?, rerank? (default true), filters?}`.
  Multi-query fusion (HyDE, expansion, step-back; RRF k=60), per-facet
  filters, and the rerank toggle all live here. One query = a one-element
  array.

**Chunk reads**
- `get_chunks` — fetch chunk bodies by id: `{ids: [1–20 uuids]}`. Feed it
  `chunk_id` values straight from search results, and batch the top hits into
  ONE call instead of fetching them one at a time. One id = a one-element
  array.
- `get_chunk_next` / `get_chunk_prev` — continue reading after / before a
  chunk in reading order (`{id, count?}`).
- `get_chunk_neighbors` — both sides of a hit in one call (`{id, count?}`).
- `get_chunk_parents` — where a chunk sits in its source's structure:
  `{parents: [{id, name, kind, document_id?}…], source}`. The document-kind
  parent carries the `document_id` you can hand to `get_document`.

**Document reads**
- `get_document` — a document's metadata plus an ordered chunk *skeleton*
  (ids, positions, token counts — no bodies). Size up a document before
  reading it.
- `get_document_chunks` — read the bodies window by window:
  `{id, from?, limit?}`. There is no document-size cap; just page through.

**Corpus discovery & diagnostics**
- `list_sources` — paginated source catalog: `{cursor?, limit?,
  created_after?, created_before?, kind?, retired?}` →
  `{sources, total, next_cursor}`. Use it for `source_slug` values and to see
  what material exists.
- `facets` — call with no args for the filter-dimension overview (open-set
  dimensions show value samples plus exact totals); call with
  `{facet, cursor?, limit?}` to page the full value list of `source_slug` /
  `language` / `tags` / `package`. **Call this before building `filters`.**
- `status` — cloud reachability, auth identity + permission level, both limit
  families (request rate, and embedding-token hourly/daily windows), Voyage
  key validity, and reranker state. Call it when searches fail or error.
- `install_search_skill` — (re)install this skill into the user's harness(es).

There is no model-pulling step: the reranker loads lazily on the first
reranked search (expect a one-time delay), and `status` reports its state.

For the exact filter shapes, the full facet catalog, and mode semantics, read
`references/filters-and-modes.md`. For combined recipes, read
`references/advanced-techniques.md`.

**Cost (D25):** an `advanced_search` call costs one rate-limit token per
distinct query in `queries` (basic `search` is always one token). Filters are
free, and `mode` changes work/latency, not token cost — `fts` skips embedding
entirely. Fan out deliberately, not reflexively.

## Default loop

1. If the question names a source / package / language / version, call `facets`
   (and/or `list_sources`) to learn the corpus's real values, then scope
   `filters`.
2. Pick a `mode` for the question shape (see *Pick a mode*).
3. Formulate 1–3 queries with the techniques below — no more than the question
   needs. One plain question → `search`; anything multi-query, filtered, or
   rerank-tuned → `advanced_search`.
4. Search.
5. Rank results by `trust_score` and `confidence_factors`; batch-read the top
   few with one `get_chunks` call.
6. If a hit is promising but partial, navigate (`get_chunk_next` /
   `get_chunk_parents` / `get_document` → `get_document_chunks`) instead of
   re-searching blindly.
7. Refine with terms you just learned and search again. Stop when the top
   results converge and are version-matched.

## Pick a mode

- `fts` — full-text only, **skips embedding** (fastest). Use for exact
  identifiers, CLI flags, or verbatim error strings. Escalate to `hybrid` if
  recall is thin.
- `vector` — semantic only. Use for purely conceptual questions with no literal
  anchor.
- `hybrid` (default) — fuses both. Use when unsure.

## Match the user's version & freshness

The corpus spans versions and eras; an unfiltered hit may be for the wrong
toolchain. To avoid stale, confidently-wrong answers, use `advanced_search`
filters:

- Pin to the user's toolchain with `version_satisfies` on `language_target` /
  `sdk_dependency` (e.g. `{ "name": "compact", "version_satisfies": ">=0.23" }`
  — confirm the real names/versions via `facets`).
- Add `"deprecated": false` to drop superseded guidance.
- Add a recency floor (`ingested_at` / `source_modified_at` `{ "after": "…" }`)
  to prefer current docs.

This version+freshness stack is the structural antidote to stale answers. See
technique B in `references/advanced-techniques.md` for the staleness-diff move.

## Filter for precision, and self-correct

Filters live only on `advanced_search` and are per-facet:
`{ "any_of": [...], "none_of": [...] }` for sets, bare bools, and
`{after,before}` / `{min,max}` ranges. AND across facets, OR within `any_of`,
exclude `none_of`.

- **Discover before you filter.** A value that isn't in the corpus returns an
  empty set that masquerades as "no answer". `facets` shows what's really there.
- **Recover from a rejected filter.** A bad facet key/value is rejected
  immediately with an error naming the offending facet. Loop: read it →
  `facets` → fix → retry.
- **Filter ladder.** Start tight; if results are too few, relax ONE facet at a
  time (least-load-bearing first — usually recency, then `verified`, then
  version) until results appear.

## Query techniques

All multi-query patterns go through `advanced_search` (`queries` is an array
even for one query). The reranker anchors on the FIRST query, so put the most
user-facing formulation first.

### HyDE — when the question is short or jargon-light
Draft a 1–2 sentence hypothetical answer and send it as an extra query beside
the question; it lands near the real docs in embedding space.
`queries: ["<question>", "<1–2 sentence hypothetical answer>"]`

### Multi-query — when your wording may not match the corpus
Send 2–3 paraphrases varying vocabulary and breadth in one call; RRF fuses them.
`queries: ["compile a contract", "build source into a deployable artifact", "smart-contract build step"]`

### Step-back — when the question is over-specific or a raw error
Pair the specific question with a more abstract framing.
`queries: ["why did this exact call fail?", "how does the platform validate calls?"]`

### Lexical anchoring — when an exact identifier / error matters
Put the exact symbol, flag, or error string verbatim in a query (and consider
`mode: fts`) so the full-text half nails the literal match.
`queries: ["how to fix this disclosure error", "potential witness-value disclosure must be declared"]`

### Symbol-aware code search — when you want a named circuit / function / type
Scope with `filters.symbol` (`{kind?, name?}`) + `filters.kind: code`, ideally in
`fts` mode, then land precisely by reading `symbol_path` and walking with
`get_chunk_parents` (scope) and `get_chunk_next` (body). `symbol.kind` is an open
vocabulary — discover the kinds from results, don't assume them.

### Retrieve-read-retrieve — when the first pass is close but partial
Broad search → batch-read the best hits (`get_chunks`) and their neighbours →
harvest precise terms → search again with them. Iterate; this is how you
converge.

### Trust-weighted selection — always
Prefer higher `trust_score`. Read `confidence_factors` (attribution,
verification, freshness, version-match) and prune sources that are unverified,
stale, or version-mismatched for the user's toolchain.

### Cross-source comparison — when sources may disagree
The server does NOT detect contradictions. When multiple sources answer the same
question, pull from each, compare, and surface disagreement (noting which is
more authoritative / version-matched) rather than silently picking one. See
technique D (differential search) for the filtered version of this.

## Reading results

- Every tool result carries `suggested_next_actions` — entries of
  `{description, tool?, arguments?}`. They are suggestions, not required next
  steps; trust the descriptions when deciding what (if anything) to run. An
  entry without a `tool` is an action for the USER (e.g. restart the harness)
  — relay it, don't attempt it.
- `search_metadata.total_candidates` signals recall: a low count means the
  corpus barely matched your wording — broaden with the multi-query techniques
  above before concluding "no answer".
- `search_metadata.per_query` reports per-query FTS / vector candidates and
  latency; each `advanced_search` result's `scores.matched_queries` lists which
  of your queries pulled it in. Use them to see which formulation is working
  and drop the rest. In `fts` mode only the full-text half runs, so vector
  candidate counts are absent.

## Going deeper

- `references/filters-and-modes.md` — exact filter shapes, the full 17-facet
  catalog, mode semantics, `facets` / `list_sources` pagination, the
  validation/error catalog, and the `mnm` CLI flags.
- `references/advanced-techniques.md` — mode-tiered cost escalation,
  version+freshness precision, the discovery/self-correction loop and filter
  ladder, trust-stratified / differential / symbol-anchored recipes, and
  efficient deep reading.

---
name: midnight-advanced-search
description: >-
  Advanced retrieval playbook for the Midnight Network documentation corpus.
  Use whenever searching, researching, or answering questions about Midnight,
  Compact, the Midnight SDK, or the corpus exposed by the midnight-manual MCP
  server (the search, get_chunk*, get_document*, and list_sources tools). Teaches
  when and how to combine HyDE, multi-query fan-out, step-back, lexical
  anchoring, symbol-aware code search, retrieve-read-retrieve, trust-weighted
  selection, and cross-source comparison to find authoritative, version-matched
  answers instead of firing one naive query.
metadata:
  source: midnight-manual
---

# Midnight advanced search

You have a hybrid retrieval surface over the Midnight corpus (full-text + vector,
RRF-fused, optional cross-encoder rerank, trust-aware scoring) plus chunk and
document navigation. This is the playbook for using it like a researcher.

## The tools you have

- `search` — hybrid retrieval. Pass a single `query`, or a `queries` array
  (1–10) the server fuses with Reciprocal Rank Fusion (k=60). Optional `rerank`
  (cross-encoder, default on) and a `filters` object (`source_slug`,
  `attribution`, `verified`, `content_type`, `language_target`,
  `sdk_dependency`, `package`). Every result carries `trust_score`,
  `confidence`, `confidence_factors`, and `scores.matched_queries`.
- `get_chunk`, `get_chunk_next`, `get_chunk_prev`, `get_chunk_neighbors`,
  `get_chunk_parents` — read around a hit in reading order, or walk up its
  heading / structure tree.
- `get_document`, `get_document_full`, `get_document_chunks` — pull a whole
  document or a windowed slice.
- `list_sources` — enumerate corpus sources so you can scope `filters`.

**Cost (D25):** a `search` call costs `max(1, distinct queries)` rate-limit
tokens. A 3-query fan-out spends 3. Fan out deliberately, not reflexively.

## Default loop

1. If the question names a source / package / language, call `list_sources`
   once and set `filters` to scope the search.
2. Formulate 2–3 queries with the techniques below — no more than the question
   needs.
3. `search` with `rerank: true`.
4. Rank results by `trust_score` and `confidence_factors`; read the top few.
5. If a hit is promising but partial, navigate (`get_chunk_next` /
   `get_chunk_parents` / `get_document_full`) instead of re-searching blindly.
6. Refine queries with terms you just learned and search again. Stop when the
   top results converge and are version-matched.

## Techniques

### HyDE — when the question is short or jargon-light
Draft a 1–2 sentence hypothetical answer and send it as an extra query beside
the question; it lands near the real docs in embedding space and pulls in
chunks the bare question misses.
`queries: ["<question>", "<1–2 sentence hypothetical answer>"]`

### Multi-query — when your wording may not match the corpus
Send 2–3 paraphrases varying vocabulary and breadth in one call; RRF fuses them,
beating synonym mismatch.
`queries: ["compile a contract", "build source into a deployable artifact", "smart-contract build step"]`

### Step-back — when the question is over-specific or a raw error
Pair the specific question with a more abstract framing.
`queries: ["why did this exact call fail?", "how does the platform validate calls?"]`

### Lexical anchoring — when an exact identifier / error matters
Put the exact symbol, flag, or error string verbatim in a query so the
full-text half nails the literal match the vector half would blur. Keep one
query natural-language and one verbatim.
`queries: ["how to fix this disclosure error", "potential witness-value disclosure must be declared"]`

### Symbol-aware code search — when you want a named circuit / function / type
Scope with `filters.package` and/or `filters.language_target`
(`{name, version_constraint_satisfies}`), then land precisely by reading hits'
`symbol_path` and walking with `get_chunk_parents` (enclosing scope) and
`get_chunk_next` (rest of the body).

### Retrieve-read-retrieve — when the first pass is close but partial
Broad search → read the best hit and its neighbours
(`get_chunk_next` / `get_chunk_parents`, or `get_document_full` for a short
doc) → harvest the precise terms you learned → search again with them. Iterate;
this is how you converge.

### Trust-weighted selection — always
Prefer higher `trust_score`. Read `confidence_factors` (attribution,
verification, freshness, version-match) and prune sources that are unverified,
stale, or version-mismatched for the user's toolchain. A lower-ranked but
verified, version-matched chunk often beats a higher-ranked stale one.

### Cross-source comparison — when sources may disagree
The server does NOT detect contradictions. When multiple sources answer the same
question, pull from each, compare, and surface disagreement to the user (noting
which is more authoritative / version-matched) rather than silently picking one.

## Reading the diagnostics
`search_metadata.per_query` reports per-query FTS / vector candidates and
latency; each result's `scores.matched_queries` lists which of your queries
pulled it in. Use them to see which formulation is working and drop the rest.

Full worked examples: `docs/cookbook/query-enhancement.md` in the
midnight-manual repo.

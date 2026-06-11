---
name: midnight-advanced-search
description: >-
  Advanced retrieval playbook for the Midnight Network documentation corpus.
  Use whenever searching, researching, or answering questions about Midnight,
  Compact, the Midnight SDK, or the corpus exposed by the midnight-manual MCP
  server (search, facets, get_chunk*, get_document*, list_sources). Teaches
  query modes (hybrid/vector/fts), per-facet filters with discovery and
  fail-fast recovery, version- and freshness-matched retrieval to avoid stale
  answers, plus HyDE, multi-query, step-back, symbol-aware code search, and
  trust-weighted selection — so you find authoritative, version-matched answers
  instead of firing one naive query.
metadata:
  source: midnight-manual
---

# Midnight advanced search

You have a hybrid retrieval surface over the Midnight corpus (full-text + vector,
RRF-fused, optional cross-encoder rerank, trust-aware scoring), per-request query
modes, a discoverable filter set, and chunk/document navigation. This is the
playbook for using it like a researcher.

## The tools you have

- `search` — retrieval. Pass a single `query`, or a `queries` array (1–10) the
  server fuses with Reciprocal Rank Fusion (k=60). Optional `mode`
  (`hybrid` default | `vector` | `fts`), `code_mode` (`on` | `off` |
  `exclusive`, see *code_mode*), `rerank` (default on), and a typed
  per-facet `filters` object. Every result carries `trust_score`, `confidence`,
  `confidence_factors`, and `scores.matched_queries`.
- `facets` — list the filterable facets, their types, whether they negate, and
  the values present in the live corpus. **Call this before building `filters`.**
- `get_chunk`, `get_chunk_next`, `get_chunk_prev`, `get_chunk_neighbors`,
  `get_chunk_parents` — read around a hit in reading order, or walk up its
  heading / structure tree.
- `get_document`, `get_document_full`, `get_document_chunks` — pull a whole
  document or a windowed slice.
- `list_sources` — enumerate corpus sources.

For the exact filter shapes, the full facet catalog, and mode semantics, read
`references/filters-and-modes.md`. For combined recipes, read
`references/advanced-techniques.md`.

**Cost (D25):** a `search` call costs `max(1, distinct queries)` rate-limit
tokens. Filters are free, and `mode` changes work/latency, not token cost —
`fts` skips embedding entirely. Fan out deliberately, not reflexively.

## Default loop

1. If the question names a source / package / language / version, call `facets`
   (and/or `list_sources`) to learn the corpus's real values, then scope
   `filters`.
2. Pick a `mode` for the question shape (see *Pick a mode*).
3. Formulate 1–3 queries with the techniques below — no more than the question
   needs.
4. `search` with your filters.
5. Rank results by `trust_score` and `confidence_factors`; read the top few.
6. If a hit is promising but partial, navigate (`get_chunk_next` /
   `get_chunk_parents` / `get_document_full`) instead of re-searching blindly.
7. Refine with terms you just learned and search again. Stop when the top
   results converge and are version-matched.

## Pick a mode

- `fts` — full-text only, **skips embedding** (fastest). Use for exact
  identifiers, CLI flags, or verbatim error strings. Escalate to `hybrid` if
  recall is thin.
- `vector` — semantic only. Use for purely conceptual questions with no literal
  anchor.
- `hybrid` (default) — fuses both. Use when unsure.

## code_mode

The corpus carries dual embeddings: every chunk has a general vector
(voyage-context-3), and code chunks additionally carry a code vector
(voyage-code-3). `code_mode` controls whether the code-vector ranked list joins
the RRF fusion:

- `on` (default for `hybrid`/`vector`) — fuse a code-vector ranked list
  alongside the general results.
- `off` — general retrieval only.
- `exclusive` — the code-vector list replaces the general vector list.

| mode | code_mode default | ranked lists fused by RRF (k=60) |
|---|---|---|
| hybrid | `on` | general vector + code vector + FTS |
| hybrid + `off` | — | general vector + FTS (today's behavior) |
| hybrid + `exclusive` | — | code vector + FTS |
| vector | `on` | general vector + code vector |
| vector + `off` | — | general vector |
| vector + `exclusive` | — | code vector |
| fts | `off` (forced) | FTS only; `code_mode` `on`/`exclusive` → **400** with explicit error message |

Code-heavy queries (function names, API signatures, error strings from code)
benefit from `code_mode=exclusive`; conceptual queries should keep the default.
Chunks without code embeddings can never appear in the code-vector list, so
`exclusive` also narrows retrieval toward code chunks. When code search ran,
`search_metadata.per_query` reports `code_vector_candidates` /
`code_vector_latency_ms` and the top-level metadata carries the effective
`code_mode`.

## Match the user's version & freshness

The corpus spans versions and eras; an unfiltered hit may be for the wrong
toolchain. To avoid stale, confidently-wrong answers:

- Pin to the user's toolchain with `version_satisfies` on `language_target` /
  `sdk_dependency` (e.g. `{ "name": "compact", "version_satisfies": ">=0.23" }`
  — confirm the real names/versions via `facets`).
- Add `"deprecated": false` to drop superseded guidance.
- Add a recency floor (`ingested_at` / `source_modified_at` `{ "after": "…" }`)
  to prefer current docs.

This version+freshness stack is the structural antidote to stale answers. See
technique B in `references/advanced-techniques.md` for the staleness-diff move.

## Filter for precision, and self-correct

Filters are per-facet: `{ "any_of": [...], "none_of": [...] }` for sets, bare
bools, and `{after,before}` / `{min,max}` ranges. AND across facets, OR within
`any_of`, exclude `none_of`.

- **Discover before you filter.** A value that isn't in the corpus returns an
  empty set that masquerades as "no answer". `facets` shows what's really there.
- **Recover from a 400.** A bad facet key/value returns a `400` with a
  remediation pointing back at `facets`. Loop: read it → `facets` → fix → retry.
- **Filter ladder.** Start tight; if results are too few, relax ONE facet at a
  time (least-load-bearing first — usually recency, then `verified`, then
  version) until results appear.

## Query techniques

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
Broad search → read the best hit and its neighbours → harvest precise terms →
search again with them. Iterate; this is how you converge.

### Trust-weighted selection — always
Prefer higher `trust_score`. Read `confidence_factors` (attribution,
verification, freshness, version-match) and prune sources that are unverified,
stale, or version-mismatched for the user's toolchain.

### Cross-source comparison — when sources may disagree
The server does NOT detect contradictions. When multiple sources answer the same
question, pull from each, compare, and surface disagreement (noting which is
more authoritative / version-matched) rather than silently picking one. See
technique D (differential search) for the filtered version of this.

## Reading the diagnostics

`search_metadata.per_query` reports per-query FTS / vector candidates and
latency; each result's `scores.matched_queries` lists which of your queries
pulled it in. Use them to see which formulation is working and drop the rest. In
`fts` mode only the full-text half runs, so vector candidate counts are absent.

## Going deeper

- `references/filters-and-modes.md` — exact filter shapes, the full 17-facet
  catalog, mode semantics, the validation/error catalog, and the `mnm` CLI flags.
- `references/advanced-techniques.md` — mode-tiered cost escalation,
  version+freshness precision, the discovery/self-correction loop and filter
  ladder, and trust-stratified / differential / symbol-anchored recipes.

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
- `search` — quick lookups: `{query, mode?, code_mode?, limit?}` and nothing
  else. One query string, always reranked, no filters. Use when one plain
  question will do.
- `advanced_search` — the full-control surface and this skill's main subject:
  `{queries: [1–10 strings], mode?, code_mode?, limit?, rerank? (default
  true), filters?}`. Multi-query fusion (HyDE, expansion, step-back; RRF
  k=60), per-facet filters, and the rerank toggle all live here. One query =
  a one-element array. Both search tools take `code_mode` (`on` | `off` |
  `exclusive`, see *code_mode* below).

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
- `facets` — call with no args for the filter-dimension overview (`language`,
  `source_slug`, `tags`, and `package` show value samples plus exact totals;
  `heading_path` and `symbol` advertise their type only); call with
  `{facet, cursor?, limit?}` to page the full value list of `source_slug` /
  `language` / `tags` / `package`. **Call this before building `filters`.**
- `status` — cloud reachability, auth identity + permission level, both limit
  families (request rate, and embedding-token hourly/daily windows), Voyage
  key validity, and reranker state. Call it when searches fail or error.
- `install_search_skill` — (re)install this skill into the user's harness(es).

Reranking is VoyageAI (`rerank-2.5`): server-side by default, or local when a
`VOYAGE_API_KEY` is configured. `advanced_search` exposes `rerank` (boolean) and
`rerank_instructions` to control and steer it; `status` reports the rerank state.

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

## Rerank instructions

`advanced_search` accepts `rerank_instructions` (max 400 chars): a natural-language
directive that guides how results are reranked. It REPLACES the built-in default
(code-focused when `code_mode=exclusive`; version-preferring when a
`language_target` filter has `version_satisfies`), so include those concerns
yourself if you override.

Three instruction shapes work well:

- **Emphasis** — name what matters in the match:
  "Prioritize chunks that show complete, compilable examples over fragments."
- **Filtering** — name what kind of document you want:
  "Prefer API reference material; deprioritize tutorials and blog posts."
- **Disambiguation** — pin ambiguous query terms:
  "'Witness' means the Compact private-input function, not a legal term."

Rules of thumb:

1. Keep it under ~25 words. Instruction tokens are multiplied by the candidate
   pool (~50 docs), so a long instruction is the single most expensive thing
   you can add to a search.
2. Don't restate the query — the model already sees it. Add only the
   preference the query can't express.
3. Don't stack contradictory goals ("prefer code" + "prefer conceptual
   overviews"); pick the one that decides ties.
4. Omit it entirely when the defaults fit: the derived defaults already handle
   code-heavy and version-pinned searches.
5. `rerank: false` is the cheap-exploration switch — use it for broad recon
   sweeps where ordering precision doesn't matter, then rerank the refined query.

See references/rerank-instructions.md for worked examples against this corpus.

## Match the user's version & freshness

The corpus spans versions and eras; an unfiltered hit may be for the wrong
toolchain. Two regimes handle the two ways content carries version information.

**Regime 1 — content that declares a target (mostly code).** Pin to the user's
toolchain with `version_satisfies` on `language_target` / `sdk_dependency`. The
value is a concrete version (e.g. `{ "name": "compact", "version_satisfies":
"0.31" }`) or a semver range (e.g. `{ "name": "compact", "version_satisfies":
">=0.23" }`). Confirm the real names/versions via `facets` first.

- **Default (`permissive`) is safe on any search.** It *biases* ranking rather
  than hard-gating: among content that declares the target, only *breaking*
  mismatches drop; near-misses are kept but penalized by distance; and
  version-silent content (which declares no target) is untouched. So you can add
  a version filter to almost any query without nuking recall.
- **For an exact pin, add `"version_match": "strict"`** at the request level —
  any candidate that doesn't satisfy the version is dropped, not just penalized.
  Use it when the user needs one precise toolchain, typically together with
  `code_mode` for code-shaped hunts.

**Regime 2 — prose (tutorials, guides, conceptual docs) rarely declares a
target.** A version filter can't bind to what isn't declared, so instead:

- Put the version in the **query text** ("deploy a contract with compact 0.31").
- Add freshness floors: `ingested_at` / `source_modified_at`
  `{ "after": "…" }`, plus `"deprecated": false` to drop superseded guidance.

Lexical anchoring of the version string is the reliable lever for prose today:
the full-text half matches the literal version characters that appear in the
text, so `fts`/`hybrid` discriminate version-qualified queries best. Do NOT
assume the contextualized embeddings semantically discriminate versions on their
own — that is an empirical question, not an assumed capability. If you need to
gate whether a corpus reliably distinguishes version-qualified prose, run the
manual probe in `references/advanced-techniques.md` (technique C) before relying
on semantic version discrimination.

**Support-matrix playbook (compatibility questions).** For "what SDK works with
node X?"-style questions, don't guess version pairings — retrieve the matrix
first: query `"support matrix"` scoped to `{ "source_slug": { "any_of":
["midnight-docs"] } }`, read the concrete versions off the matrix page, then
issue version-pinned follow-ups using those exact versions.

**Discovery.** Confirm the corpus actually covers a version before pinning to
it: `facets` with `{"facet": "language_target"}` lists the target names, then
`{"facet": "language_target", "within": "<name>"}` enumerates the declared
version constraints inside that name. Pin only to versions the corpus declares.

This version+freshness stack is the structural antidote to stale answers. See
technique C in `references/advanced-techniques.md` for the staleness-diff move
and the strict-mode variant.

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
  version) until results appear. The version rung is itself a sub-ladder: zero
  results under `version_match: "strict"` → retry `permissive` (which keeps
  penalized near-misses) → only then drop the version filter entirely.

## Query techniques

All multi-query patterns go through `advanced_search` (`queries` is an array
even for one query). The reranker anchors on the FIRST query, so put the most
user-facing formulation first. The thumbnails below are the quick reference; for
the exact LLM prompts that generate each pattern and worked `queries` arrays,
see technique B in `references/advanced-techniques.md`.

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
technique E (differential search) for the filtered version of this.

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
  candidate counts read zero.

## Going deeper

- `references/filters-and-modes.md` — exact filter shapes, the full 17-facet
  catalog, mode semantics, `facets` / `list_sources` pagination, the
  validation/error catalog, and the `mnm` CLI flags.
- `references/advanced-techniques.md` — mode-tiered cost escalation, the
  query-enhancement patterns with the LLM prompts that generate them (HyDE,
  multi-query expansion, step-back), version+freshness precision and the
  version-discrimination probe, the discovery/self-correction loop and filter
  ladder, trust-stratified / differential / symbol-anchored recipes, and
  efficient deep reading.
- `references/rerank-instructions.md` — worked `rerank_instructions` examples
  against this corpus: the emphasis / filtering / disambiguation shapes, when to
  override the derived defaults, and when to drop instructions or rerank at all.

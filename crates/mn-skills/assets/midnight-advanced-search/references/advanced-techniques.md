# Advanced search techniques

Recipes that combine `mode` + `filters` + multi-query. Everything multi-query
or filtered runs on `advanced_search` (`queries` is an array even for one
query); plain one-question lookups can stay on basic `search`. Shapes and the
facet catalog live in `filters-and-modes.md`. Every concrete value here is
illustrative — confirm real values with the `facets` tool.

## A. Mode-tiered cost escalation

Pick the cheapest mode that can answer, escalate only if it can't.

- **Known literal** (a symbol, CLI flag, error string)? Start in `fts` — it
  skips embedding entirely, so it is the fastest path and nails exact matches.
  Basic `search` is enough when it's one query:
  `{ "query": "potential witness-value disclosure must be declared", "mode": "fts" }`
- **Thin recall** from `fts`? Escalate the same query to `hybrid` to add
  semantic matches (watch `search_metadata.total_candidates` — a low count is
  the signal to broaden).
- **Purely conceptual** ("how does X relate to Y", no literal anchor)? Go
  straight to `vector` (or `hybrid`).

Rule of thumb: literal → `fts`; fuzzy/conceptual → `vector`; unsure → `hybrid`.

## B. Query enhancement (HyDE, expansion, step-back)

Multi-query fusion trades a little LLM work for a measurable recall lift: pass
several queries in one `advanced_search` call and the server runs hybrid
retrieval for each, then fuses every ranked list — across both retrieval halves
and across all your queries — with RRF (k=60) in a single pass. The three
generation patterns below each give the LLM prompt the agent emits and the
resulting `queries` array. Start with a single query; reach for these only when
recall on the bare question is weak, and use `scores.matched_queries` (the
0-based indices of the queries that pulled a result in) to confirm the extra
queries actually contributed before keeping them.

The `search` tool takes **text** — you never embed anything yourself. The server
embeds each query remotely (VoyageAI) and fuses the results.

- **HyDE (Hypothetical Document Embeddings)** — when the question is short,
  jargon-light, or phrased very differently from how the corpus phrases its
  answer. A hypothetical answer lands in the same embedding neighbourhood as the
  real docs, so adding it as a second query pulls in chunks the bare question
  misses. The answer does **not** need to be correct — it is embedded and
  discarded; only its position in vector space matters.

  Prompt the agent emits:

  ```text
  Write a short (1–2 sentence) hypothetical answer to the following question,
  as if it were an excerpt from the documentation. Do not hedge; state it
  plainly even if you are unsure — this text is used only to find similar real
  passages.

  Question: {user_question}
  ```

  Resulting call (original question + the hypothetical answer, cost 2 tokens):

  ```jsonc
  {
    "queries": [
      "how do I pay transaction fees on Midnight?",
      "Transaction fees are paid in the network's fee resource, which is derived from the staking token and consumed when a transaction is submitted."
    ]
  }
  ```

- **Multi-query expansion** — when synonyms or differing specificity matter (the
  user says "compile", the docs say "build"; the question is narrow but the
  answer lives on a broader page). Paraphrase 2–3 ways, varying vocabulary and
  breadth, and include the original.

  Prompt the agent emits:

  ```text
  Rewrite the following question as 2–3 alternative search queries. Vary the
  vocabulary (use synonyms) and the breadth (one narrower, one broader). Return
  one query per line, no numbering.

  Question: {user_question}
  ```

  Resulting call (cost = distinct count; an emitted paraphrase identical to the
  original is de-duplicated and not billed):

  ```jsonc
  {
    "queries": [
      "how do I compile a Compact contract?",
      "build source into a deployable artifact",
      "compactc command-line usage"
    ]
  }
  ```

- **Step-back prompting** — when the question is over-specific (a raw error, a
  "why did *this* fail?") whose real answer is a more general concept. Pair the
  specific question with a "stepped-back" abstract version to retrieve both the
  precise hit and the explanatory background.

  Prompt the agent emits:

  ```text
  Given the following specific question, write one more general "step-back"
  question whose answer would provide the background needed to answer it. Return
  only the step-back question.

  Question: {user_question}
  ```

  Resulting call (cost 2 tokens):

  ```jsonc
  {
    "queries": [
      "why did my contract call revert with a witness mismatch?",
      "how does Midnight validate contract calls and witnesses?"
    ]
  }
  ```

The patterns compose: a HyDE answer plus two expansion paraphrases is a 4-query
request costing 4 tokens (hard ceiling 10 queries / request, configurable lower
via `MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST`). `code_mode` composes with all of
them — each query is embedded with both models when code search runs, and the
per-query record gains `code_vector_candidates` / `code_vector_latency_ms`.

## C. Version & freshness precision (anti-staleness)

The corpus mixes versions and eras. To avoid handing the user advice for the
wrong toolchain, constrain to *their* version and to current material.

- **Pin to the user's toolchain** with `version_satisfies`:

  ```jsonc
  {
    "queries": ["how do I deploy a contract"],
    "filters": {
      "language_target": { "any_of": [{ "name": "compact", "version_satisfies": ">=0.23" }] },
      "sdk_dependency":  { "any_of": [{ "kind": "npm", "name": "@midnight-ntwrk/midnight-js", "version_satisfies": "^1.0" }] }
    }
  }
  ```

  (Confirm the real target/dependency names and the user's actual versions
  before trusting these — `facets` lists the names present; `version_satisfies`
  takes a concrete version like `"0.31"` or a range like `">=0.23"`.)

- **Bias vs. pin.** By default (`version_match` `permissive`) the filter only
  *biases* ranking — among content that declares the target, breaking
  mismatches drop and near-misses are kept-but-penalized; version-silent prose
  is untouched. Add `"version_match": "strict"` at the request level (usually
  with `code_mode`) to hard-drop anything that doesn't satisfy the version:

  ```jsonc
  {
    "queries": ["how do I deploy a contract"],
    "version_match": "strict",
    "code_mode": "exclusive",
    "filters": {
      "language_target": { "any_of": [{ "name": "compact", "version_satisfies": "0.31" }] }
    }
  }
  ```

- **Recovery rung.** Zero results under `strict` → retry `permissive` (keeps
  penalized near-misses) → only then drop the version filter entirely. Relax
  the version constraint *last* on the filter ladder, not first.
- **Drop superseded guidance**: add `"deprecated": false`.
- **Prefer current docs**: add a recency floor, e.g.
  `"source_modified_at": { "after": "2026-01-01" }` (or `ingested_at`).
- **Staleness check**: run the query once unfiltered and once with a recency
  floor; if the fresh-only top results differ materially from the unfiltered
  ones, the answer likely changed — surface that to the user instead of trusting
  the older chunk.

### Probe: does this corpus discriminate version-qualified prose?

For content that *declares* a target (Regime 1 in `SKILL.md`), the
`version_satisfies` / `version_match` machinery does the work. For prose that
states a version only in its text (Regime 2), whether retrieval actually
separates "Compact 0.31" from "Compact 0.23" is an empirical property of the
corpus and its embedding model — verify it before claiming semantic version
discrimination. A quick manual probe:

1. **Setup.** Ingest a probe source of paired docs: identical tutorial bodies
   whose first paragraph states a different target ("This tutorial targets
   Compact 0.23" vs "… Compact 0.31"), plus one no-statement control.
2. **Queries.** Run the version-qualified queries ("how to declare a ledger in
   compact 0.31", "… in compact 0.23") and the unqualified version.
3. **Measure.** For each query × mode (`hybrid`, `vector`, `fts`), note the rank
   order of the three docs —
   `mnm search --json | jq '.results[].source_path'`.
4. **Interpret.** If the version-stated docs out-rank the control for matching
   queries in `fts` but **not** in `vector`, keep the conservative "put the
   version in your query text" (lexical-anchoring) guidance and do NOT claim
   semantic version matching. If `vector` mode also discriminates, the corpus's
   contextualized embeddings carry version context and the skill may say so.

Record the outcome with the date and the corpus embedding model — the answer can
shift when either changes. The durable takeaway: **lexical anchoring of version
strings is the reliable lever today; semantic version discrimination is a
measured property, not an assumed one.**

## D. Discovery & self-correction

- **Discover before you filter.** Call `facets` first to learn the corpus's real
  languages, tags, sources, and package names. The overview samples open-set
  values (≤10 each, with exact totals); drill into `source_slug` / `language` /
  `tags` / `package` to page the full list when the sample isn't enough.
  Guessing a value that isn't there returns an empty set that *looks* like
  "no answer".
- **Recover from a rejected filter.** A bad filter is rejected immediately with
  an error naming the offending facet. Loop: read the message → call `facets`
  → fix the key/value → retry. Never silently give up on a filtered search.
- **Filter ladder (progressive relaxation).** Start tight (version + verified +
  recency + language). If you get too few results, relax ONE facet at a time,
  least-load-bearing first — typically recency, then `verified`, then version —
  re-searching after each drop until results appear. This finds the most
  precise answer the corpus can actually support.

## E. Trust-stratified, differential & symbol-anchored

Heavier patterns that spend several searches — use when correctness matters more
than latency. Each stratum is one `advanced_search` call.

- **Differential / trust-stratified search.** Run the *same* `queries` across
  strata and diff the result sets; the server does not detect contradictions
  for you.
  - authority: `attribution: { any_of: ["foundation"] }` vs
    `attribution: { any_of: ["community"] }`
  - vetting: `verified: true` vs unfiltered
  - era: recent (`ingested_at.after`) vs unfiltered
  Where the strata disagree, surface the disagreement and say which source is
  more authoritative / version-matched.
- **Symbol-anchored code landing.** To land on a named circuit/function/type:
  `{ "queries": ["deployContract"], "mode": "fts",
     "filters": { "kind": { "any_of": ["code"] },
                  "symbol": { "any_of": [{ "name": "deployContract" }] } } }`
  then read scope and body with `get_chunk_parents` (enclosing scope) and
  `get_chunk_next` (rest of the body). `symbol.kind` is an open vocabulary —
  discover the kinds present from the results rather than assuming them.
- **Content-type routing.** Match the *kind* of answer to the need:
  `content_type: { any_of: ["example"] }` + `kind: { any_of: ["code"] }` for
  "show me code"; `["reference"]` for API signatures; `["test"]` for usage
  patterns.
- **Heading-path scoping.** Narrow to a section with `heading_path`
  (e.g. a "Troubleshooting" or "API" heading) when you know where the answer
  lives.

## F. Efficient deep reading

Once a search has found the right neighbourhood, read it without burning calls:

- **Batch the top hits.** Collect the `chunk_id` of each promising result and
  fetch them in ONE `get_chunks` call (`ids` takes 1–20 uuids). Unknown ids
  come back in a `missing` list rather than failing the batch.
- **Skeleton first, bodies second.** For a whole document, `get_document`
  returns metadata plus the chunk skeleton (ids, positions, token counts — no
  bodies). Use the token counts to plan, then page the bodies with
  `get_document_chunks` (`{id, from?, limit?}`) window by window — there is no
  document-size cap.
- **Climb to the document.** From any chunk, `get_chunk_parents` returns the
  containing structure; the document-kind parent carries the `document_id` to
  feed `get_document`.

## Composition

These stack. A precision query is a funnel — version + `verified` + recency +
`language` + `content_type` — AND-ed together in one `advanced_search`. When a
funnel returns nothing, switch to the filter ladder (technique D) and relax it
one facet at a time.

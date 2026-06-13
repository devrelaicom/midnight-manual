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

## B. Version & freshness precision (anti-staleness)

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

## C. Discovery & self-correction

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

## D. Trust-stratified, differential & symbol-anchored

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

## E. Efficient deep reading

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
funnel returns nothing, switch to the filter ladder (technique C) and relax it
one facet at a time.

# Advanced-Search Skill Bundle (Facets + Query Modes) — Design

- **Date:** 2026-06-05
- **Status:** Approved (brainstorming) — ready for implementation planning
- **Driver:** Teach AI clients (and CLI users) to actually *use* the new search facets and query modes — when to reach for them, how to get accurate/efficient results, and the advanced techniques the primitives unlock.
- **Scope:** Documentation + the `mn-skills` installer. No retrieval *behavior* changes (those landed on `feat/search-facets-query-modes`).
- **Branch:** `feat/search-facets-query-modes`

## 1. Context & Motivation

The `feat/search-facets-query-modes` branch added three retrieval primitives to
`POST /v1/search` and the MCP `search` tool:

1. **Query modes** — `mode: hybrid | vector | fts` (default `hybrid`). `fts` skips
   the embedding round-trip entirely (lowest latency, no Voyage call); `vector` is
   semantic-only; `hybrid` fuses both.
2. **A 17-facet, per-request filter model** — `{any_of, none_of}` set-matches over
   strings and structured elements, bare bools, temporal/numeric ranges, and
   `version_satisfies` semver on `language_target`/`sdk_dependency`. AND across
   facets, OR within `any_of`, exclude `none_of`. Backed by a single facet registry
   (`crates/mn-retrieval/src/facets.rs`).
3. **Discovery + fail-fast** — a `facets` tool / `GET /v1/facets` endpoint that
   returns the corpus's real facet values, and strict validation that rejects an
   unknown key/value with a `400` + remediation (no more silent drops).

The **agent-facing playbook that teaches retrieval** —
`crates/mn-skills/assets/midnight-advanced-search/SKILL.md` — still documents the
*old* surface: an opaque `filters` object, no modes, no discovery. The capability
shipped; the guidance did not. This design closes that gap and develops the
advanced techniques the new primitives make possible.

### Two structural problems with the skill as it ships today

- **It is not self-contained.** The skill is delivered as a single `SKILL.md`
  (`include_str!`) copied verbatim into each detected harness's skills directory by
  the `install_search_skill` MCP tool / `mnm skills` CLI noun. Its final line points
  the agent at `docs/cookbook/query-enhancement.md` "in the midnight-manual repo" —
  a path that does not exist when the agent runs inside *someone else's* project. A
  test (`body_links_the_cookbook_for_dryness`) actively pins that link, so "be DRY by
  linking the repo cookbook" fights "be a self-contained installed skill."
- **The installer only writes `SKILL.md`.** There is no support for shipping bundled
  reference files, which is the natural lever for adding depth without bloating an
  always-loaded file.

## 2. Goals / Non-Goals

**Goals:**
- Rewrite the skill so an agent knows **when** to use modes/filters, **how** to get
  accurate, version-matched results, and how to search **efficiently** (cheapest
  mode that fits; discover-before-filter; self-correct on error).
- Make the shipped skill **self-contained** (no repo-path references in any shipped
  file).
- Give the advanced techniques room to breathe via **progressive disclosure** — a
  lean core plus on-demand reference files.
- Keep the documented facet set from **drifting** from the enforced registry.

**Non-Goals:**
- No change to server / MCP / CLI retrieval *behavior* — already implemented on this
  branch.
- No new facets or modes.
- Not rewriting the MCP tool *descriptions* in `crates/mn-mcp/src/tools.rs` — already
  typed on this branch.
- Not building the recall/eval harness (separate follow-up; see the facets/modes
  design's Phase 2).

## 3. Decisions (locked during brainstorming)

| # | Decision |
|---|----------|
| D1 | **Skill bundle**, not a single file: lean `SKILL.md` + `references/` read on demand. Teach the installer to ship the whole directory. |
| D2 | Promote to the always-loaded core: **B (version & freshness)**, **C (discovery & self-correction)**, and **A (mode tiering)** as a one-liner-plus-pointer. **D (trust-stratified / differential)** is reference-only behind a pointer. |
| D3 | The bundle is **self-contained**; the repo cookbook stays as human-facing docs (lightly refreshed). Drop the agent-facing repo-path link; replace the link-pinning test with a self-containment assertion. |
| D4 | Two reference files: `filters-and-modes.md` (API reference / lookup) and `advanced-techniques.md` (recipes). |
| D5 | Installer ships an embedded **manifest** of files; install is idempotent over the *set* and **prunes orphans** in the owned dir. |
| D6 | A **registry-drift guard** test asserts every `facets::facets()` key appears in the shipped facet catalog. |
| D7 | All example values (languages, tags, symbol kinds, versions) are framed as **illustrative — verify via `facets`**, never shipped as authoritative corpus facts. |

## 4. Design

### 4.1 Bundle layout

```text
crates/mn-skills/assets/midnight-advanced-search/
  SKILL.md                       # lean core — always loaded when relevant
  references/
    filters-and-modes.md         # API reference: facet catalog + exact wire shapes + mode/validation semantics
    advanced-techniques.md       # recipe book: A-deep, B-deep, C-deep, D
```

Split rationale: `filters-and-modes.md` answers *"what exists and what is the exact
shape"* (lookup); `advanced-techniques.md` answers *"how do I combine them"*
(recipes). `SKILL.md` teaches the default path plus the promoted techniques and
links into both on demand.

### 4.2 `SKILL.md` core (content outline)

Retain the existing query-side techniques (HyDE, multi-query, step-back, lexical
anchoring, symbol-aware, retrieve-read-retrieve, trust-weighted, cross-source),
condensed. Add/revise:

- **Tools** — add the `facets` tool; note `search` now accepts a `mode` and a typed
  per-facet `filters` (the catalog lives in the reference, not inline).
- **Cost** — keep `max(1, distinct queries)` rate-limit tokens, and clarify the
  nuance: **filters are free; `mode` changes latency/embedding work, not rate-limit
  tokens** (`fts` skips the Voyage round-trip).
- **Default loop** — revised: name a source/package/language/version → call `facets`
  to learn the corpus's real values and scope filters → pick a `mode` for the
  question shape → search with filters → rank by trust/confidence → navigate →
  refine.
- **Promoted sections (new):**
  - *Pick a mode (A, one-liner):* `fts` for exact identifiers/errors (no embedding),
    `vector` for purely conceptual, `hybrid` default; escalate `fts → hybrid` if
    recall is thin. Pointer to recipes.
  - *Match the user's version & freshness (B, emphasized):* `version_satisfies` on
    `language_target`/`sdk_dependency`, `deprecated: false`, `ingested_at` /
    `source_modified_at` ranges — the anti-staleness stack, with a worked example.
    This is the highest-leverage accuracy lever and directly serves the project's
    mission.
  - *Discovery & self-correction (C):* always `facets`-first; a bad filter returns a
    `400` with the offending key/value + remediation → fix via `facets`; the "filter
    ladder" (start tight, relax one facet at a time until results appear).
- **Going deeper** — pointers to `references/advanced-techniques.md` (mode
  escalation, differential / trust-stratified, symbol-anchored landing) and
  `references/filters-and-modes.md` (full catalog + exact shapes).

### 4.3 `references/filters-and-modes.md` (API reference)

- **Mode semantics + embedding contract:** what each mode runs; `fts` skips
  embedding (and so `vector`/`client_embedding_model` are optional/ignored);
  `vector`/`hybrid` require the vector; rerank is orthogonal; mode is per-request.
- **The five filter shapes:** enum-set, open-set, object-set, bool, range — each with
  a wire example.
- **Full v1 facet catalog** mirroring the registry exactly — one row per facet:
  key / type / backing / negatable / closed values. Call out the sharp edges:
  - `symbol.kind` is an **open, chunker-derived vocabulary** (e.g. `fn`, `struct`,
    `impl`, `method`, `class`), **not** a closed enum — discover kinds via results /
    `facets`, do not hard-code a list.
  - `version_satisfies` is evaluated in a Rust post-match (Postgres has no semver
    type); `language_target` / `sdk_dependency` are **not negatable**.
  - Ranges are inclusive, either bound optional.
  - Empty `any_of: []` means *absent* (no constraint), never "match nothing."
- **Discovery:** the `facets` / `GET /v1/facets` output shape; open-set values are
  corpus-derived; high-cardinality sets (`tags`, `symbol.name`, `package.name`) are
  top-N with `truncated` / `total` and are **examples, not the closed universe**.
- **Validation & errors:** the full failure catalog (unknown key, invalid closed-set
  value, wrong shape, negation on a non-negatable facet, contradictory/`min>max`
  range, bad date, malformed semver, mode/vector consistency) → each a `400` with
  remediation. This is the self-correction recipe in reference form.
- **CLI mapping:** the `mnm search` filter flags and the `mnm facets` subcommand.

### 4.4 `references/advanced-techniques.md` (recipes)

Each recipe documents *when / how (concrete mode + filter + query) / cost*:

- **A — Mode-tiered cost escalation.** Cheap `fts` literal probe for a known symbol,
  flag, or error string (zero embedding); escalate to `hybrid` only if recall is
  thin; `vector` for purely conceptual asks.
- **B — Version + freshness precision stack.** Worked example pinning retrieval to a
  user's compiler/SDK version via `version_satisfies`, plus `deprecated: false` and a
  recency range; plus **staleness detection** (re-run gated to recent and diff
  against the unfiltered set).
- **C — Discovery & self-correction.** `facets`-first discovery; the fail-fast
  recovery loop; the **filter ladder** (progressive relaxation) with a relax-ordering
  heuristic (drop the least-load-bearing facet first).
- **D — Trust-stratified / differential & symbol-anchored.**
  - Differential search: run the *same* query across strata (`attribution`
    foundation-vs-community, `verified`-vs-all, fresh-vs-all) and diff the result sets
    to surface disagreement/drift the server does not detect for you.
  - Symbol-anchored code landing: `symbol` object-set + `kind: code` + `fts` to land
    on a named circuit/fn, then `get_chunk_parents` / `get_chunk_next` to read scope
    and body.
  - Content-type routing (`example`/`reference`/`test`) and `heading_path` scoping.
  - Funnel composition: stacking the precision stack vs. the relaxation ladder.

### 4.5 Installer changes (`crates/mn-skills`)

- **`lib.rs`:** replace `skill_markdown()` with an embedded manifest —
  `skill_files() -> &'static [(&'static str /* rel path */, &'static str /* body */)]`
  built from `include_str!` for `SKILL.md` and both references. (Pre-1.0; clean
  cutover, no back-compat shim — a stale single-file consumer is not a concern.)
- **`install.rs`:** write every manifest entry, creating `references/`. Idempotency
  over the *set*: `Unchanged` iff every file is byte-identical, else `Updated`;
  `Created` when the owned dir is absent. **Prune** any file inside the owned skill
  dir that is not in the manifest (prevents orphans when a reference is later renamed
  or removed). Pruning is scoped strictly to the owned `midnight-advanced-search/`
  dir.
- **`status.rs`:** `up_to_date` iff all manifest files are present and byte-identical.
- **`remove`:** unchanged — already `remove_dir_all` on the owned dir.
- **Report shape:** unchanged. `HarnessInstall.path` continues to point at `SKILL.md`
  (the primary file); the `install_search_skill` MCP output schema stays valid. The
  per-harness action now reflects the bundle as a whole.

### 4.6 Repo cookbook (secondary)

Light refresh of `docs/cookbook/query-enhancement.md`: add a short "Modes & filters"
section and a cross-link to `mnm facets`. It remains human-facing repo documentation
and is no longer the agent's source of truth (the bundle is).

## 5. Testing

Correctness + drift, no DB needed (all in `mn-skills` unit tests):

- Update the single-file-assuming install/status tests to the bundle.
- Replace `body_links_the_cookbook_for_dryness` → **`bundle_is_self_contained`**: no
  shipped file contains a repo path (`docs/cookbook/`, absolute repo paths); `SKILL.md`
  links only `references/...`.
- **Manifest completeness:** every `references/...` path `SKILL.md` links exists in
  the manifest, and every manifest file is either `SKILL.md` or linked from it.
- **Registry-drift guard:** every key from `mn_retrieval::facets::facets()` appears in
  the shipped `filters-and-modes.md` catalog. (Keeps docs honest against the enforced
  registry.)
- Install writes all files; `up_to_date` flips when a single reference is made stale;
  prune removes an orphaned file in the owned dir.
- The existing frontmatter tests (parse `SKILL.md`, name matches folder, name regex)
  stay.

## 6. Accuracy guardrails

The skill describes the *search platform's own* facets/modes — grounded in the
registry (`facets.rs`) and the MCP contract (`specs/001-rag-platform/contracts/
mcp-tools.json`), not in recalled Midnight knowledge. Where recipes reference
Compact/SDK specifics (symbol kinds, `version_satisfies` strings, language names),
they are written as **illustrative placeholders the agent must confirm via `facets`**
— which both avoids shipping fabricated corpus facts and reinforces technique C.

## 7. Out of scope / follow-ups

- No new facets/modes; no retrieval-behavior change.
- The recall/eval harness that would *quantify* mode/facet quality remains a separate
  follow-up (see the facets/modes design, §5).
- Ingest-triggered `/v1/facets` cache invalidation (currently TTL-based) is unrelated
  and unchanged.

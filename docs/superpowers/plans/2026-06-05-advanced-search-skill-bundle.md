# Advanced-Search Skill Bundle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the single-file `midnight-advanced-search` skill into a self-contained bundle (lean `SKILL.md` + two `references/` files) that teaches the new query modes and per-facet filters, and teach the `mn-skills` installer to ship the whole directory.

**Architecture:** Author three markdown files under `crates/mn-skills/assets/midnight-advanced-search/`. Replace the installer's single-file write with an embedded file *manifest* (`skill_files()`); install writes every file and prunes orphans; status is up-to-date only when every file matches. Guard tests pin self-containment, manifest completeness, and registry↔docs drift.

**Tech Stack:** Rust (stable, MSRV 1.91), `std::fs`, `serde`, `thiserror`; markdown content; `cargo test -p mn-skills`.

---

## Background the implementer needs

- The skill ships today as ONE file. `crates/mn-skills/src/lib.rs` embeds it with `include_str!` via `skill_markdown()`. `install.rs` writes that one string to `<skills_root>/midnight-advanced-search/SKILL.md` for each detected harness; `status.rs` compares the installed file to the embedded body; `remove` deletes the whole owned dir (`remove_dir_all`).
- The owned dir name is `crate::SKILL_NAME` = `"midnight-advanced-search"`. Per-harness paths come from `Harness::skill_dir(scope, base)` and `Harness::skill_file(scope, base)` in `harness.rs`.
- The CLI (`crates/mn-cli/src/commands/skills/*`) and MCP (`crates/mn-mcp/src/tools.rs` `install_search_skill`) call `mn_skills::install/remove/status` and render the returned report. **They never call `skill_markdown()` directly** (verified by grep), so their code does not change — only the *content* of what install writes.
- The facet universe is defined once in `crates/mn-retrieval/src/facets.rs::facets()` — 17 descriptors. The wire keys are: `attribution`, `content_type`, `kind`, `source_kind`, `source_slug`, `language`, `tags`, `heading_path`, `symbol`, `package`, `verified`, `deprecated`, `language_target`, `sdk_dependency`, `ingested_at`, `source_modified_at`, `token_count`.
- Closed-enum facet values (authoritative, from `facets.rs`):
  - `kind`: `markdown`, `code`, `plaintext`
  - `source_kind`: `docs_site`, `code_repo`, `standalone`, `mixed`
  - `attribution`: `foundation`, `partner`, `third_party`, `community`, `unknown`
  - `content_type`: `doc`, `tutorial`, `reference`, `example`, `contract_source`, `sdk_source`, `test`, `readme`, `other`
  - `package.kind`: `rust`, `npm`, `compact`, `other`
- `symbol.kind` is an **open** chunker-derived vocabulary (`fn`, `struct`, `impl`, `method`, `class`, …), NOT a closed enum. `symbol.name` is open. `version_satisfies` (on `language_target`/`sdk_dependency`) is semver evaluated in a Rust post-match and those two facets are NOT negatable.
- Filter wire shapes (`crates/mn-retrieval/src/filters.rs`): `SetMatch<T> = {any_of?, none_of?}`; `SymbolMatch = {kind?, name?}`; `PackageMatch = {kind, name}`; `LanguageTargetMatch = {name, version_satisfies?}`; `SdkDependencyMatch = {kind, name, version_satisfies?}`; `TemporalRange = {after?, before?}` (ISO `YYYY-MM-DD`); `NumericRange = {min?, max?}`. `verified`/`deprecated` are bare bools. Empty `any_of: []` means *absent* (no constraint). Unknown keys are rejected (`deny_unknown_fields`).
- Modes: `hybrid` (default, fuses FTS+vector), `vector` (semantic only), `fts` (full-text only — **skips embedding entirely**). Rate-limit cost is `max(1, distinct queries)` tokens regardless of mode/filters; `fts` saves embedding *latency/work*, not tokens.

**Illustrative-values rule:** every example language/tag/symbol-kind/version string in the docs is a placeholder the agent must confirm via the `facets` tool. Never present them as authoritative corpus facts.

---

## File map

- Create: `crates/mn-skills/assets/midnight-advanced-search/references/filters-and-modes.md`
- Create: `crates/mn-skills/assets/midnight-advanced-search/references/advanced-techniques.md`
- Rewrite: `crates/mn-skills/assets/midnight-advanced-search/SKILL.md`
- Modify: `crates/mn-skills/src/lib.rs` (add `skill_files()` manifest; redefine `skill_markdown()` over it; replace the cookbook-link test with self-containment + manifest-completeness + catalog-keys tests)
- Modify: `crates/mn-skills/src/install.rs` (write all manifest files; prune orphans; update tests)
- Modify: `crates/mn-skills/src/error.rs` (no change expected — `Io` already covers prune; listed only so the implementer doesn't go hunting)
- Modify: `crates/mn-retrieval/src/facets.rs` (strengthen the registry test to an exact 17-key set — the other half of the drift guard)
- Modify: `docs/cookbook/query-enhancement.md` (add a human-facing "Modes & filters" section)

---

## Task 1: Author the API reference — `references/filters-and-modes.md`

**Files:**
- Create: `crates/mn-skills/assets/midnight-advanced-search/references/filters-and-modes.md`

- [ ] **Step 1: Create the reference file**

Create `crates/mn-skills/assets/midnight-advanced-search/references/filters-and-modes.md` with EXACTLY this content:

````markdown
# Filters & modes reference

The exact shapes for the `search` tool's `mode` and `filters`. For *recipes*
(how to combine them) see `advanced-techniques.md`. **Before building a
`filters` object, call the `facets` tool** to learn the values that actually
exist in the live corpus — every concrete value below is illustrative.

## Query modes

`mode` is one string per request; default `hybrid`.

| mode | runs | embedding | use it for |
|------|------|-----------|------------|
| `hybrid` | full-text **and** vector, RRF-fused | required | the default; best recall |
| `vector` | vector only | required | purely conceptual questions, paraphrase-heavy wording |
| `fts` | full-text only | **skipped entirely** | exact identifiers, flags, error strings; lowest latency |

- In `fts` mode you do not embed anything — it is the cheapest, lowest-latency
  mode. Reach for it when the literal characters matter (a symbol, a CLI flag, a
  verbatim error). `vector`/`hybrid` need an embedding.
- Rerank is orthogonal: it re-sorts whatever the fused set is, in any mode.
- Rate-limit cost is `max(1, distinct queries)` tokens in every mode. Filters
  are free. `mode` changes work/latency, not token cost.

## Filter model

`filters` is an object keyed by facet name. Across facets the combination is
**AND**; within a set facet's `any_of` it is **OR**; `none_of` excludes. A
missing facet means "no constraint". An empty `any_of: []` also means "no
constraint" (never "match nothing").

Five shapes, by facet type:

| Shape | Wire form | Notes |
|-------|-----------|-------|
| enum-set (closed) | `{ "any_of": [..], "none_of": [..] }` | values from a fixed list (below) |
| open-set | `{ "any_of": [..], "none_of": [..] }` | values are corpus-derived — discover via `facets` |
| object-set | `{ "any_of": [{..}], "none_of": [{..}] }` | structured element matchers |
| bool | `true` / `false` | direct |
| range | `{ "after": "YYYY-MM-DD", "before": "YYYY-MM-DD" }` or `{ "min": N, "max": N }` | inclusive; either bound optional |

A misspelled facet key or an invalid closed-set value is a **`400`**, not a
silent drop (see *Validation*).

## Facet catalog (v1)

All 17 facets. "Neg?" = supports `none_of`.

| key | type | filters on | Neg? | values |
|-----|------|-----------|------|--------|
| `attribution` | enum-set | who vouches for the doc | yes | `foundation`, `partner`, `third_party`, `community`, `unknown` |
| `content_type` | enum-set | the kind of content | yes | `doc`, `tutorial`, `reference`, `example`, `contract_source`, `sdk_source`, `test`, `readme`, `other` |
| `kind` | enum-set | chunk content kind | yes | `markdown`, `code`, `plaintext` |
| `source_kind` | enum-set | the kind of source | yes | `docs_site`, `code_repo`, `standalone`, `mixed` |
| `source_slug` | open-set | a specific source | yes | corpus-derived (`list_sources` / `facets`) |
| `language` | open-set | programming language | yes | corpus-derived (e.g. `compact`, `rust`, `typescript`) |
| `tags` | open-set | provenance tags | yes | corpus-derived |
| `heading_path` | open-set | a heading/section | yes | corpus-derived |
| `symbol` | object-set `{kind?, name?}` | named code symbols | yes | `kind` is an **open** chunker vocabulary (`fn`, `struct`, `impl`, `method`, `class`, …); `name` open |
| `package` | object-set `{kind, name}` | owning package | yes | `kind` ∈ `rust`/`npm`/`compact`/`other`; `name` corpus-derived |
| `verified` | bool | human-vetted flag | n/a | `true` / `false` |
| `deprecated` | bool | deprecation flag | n/a | `true` / `false` |
| `language_target` | object-set `{name, version_satisfies?}` | targeted language + version | no | `version_satisfies` is semver |
| `sdk_dependency` | object-set `{kind, name, version_satisfies?}` | an SDK dependency + version | no | `version_satisfies` is semver |
| `ingested_at` | range (temporal) | when we ingested it | no | `{after?, before?}` ISO dates |
| `source_modified_at` | range (temporal) | upstream last-modified | no | `{after?, before?}` ISO dates |
| `token_count` | range (numeric) | chunk size in tokens | no | `{min?, max?}` |

Sharp edges:

- `symbol.kind` is NOT a closed enum — do not hard-code a list; discover real
  kinds from results or `facets`. Either side of `{kind?, name?}` is optional.
- `version_satisfies` is a semver requirement (e.g. `">=0.23"`, `"^1.2"`)
  evaluated against the target/dependency's declared constraint. The two
  semver-bearing facets (`language_target`, `sdk_dependency`) cannot be negated.
- Ranges are inclusive; give one bound or both.

Example combining several:

```jsonc
"filters": {
  "kind":        { "any_of": ["code"] },
  "language":    { "any_of": ["compact"], "none_of": ["typescript"] },
  "symbol":      { "any_of": [{ "kind": "circuit" }, { "name": "deployContract" }] },
  "deprecated":  false,
  "ingested_at": { "after": "2026-05-01" },
  "token_count": { "min": 50 }
}
```

## Discovery: the `facets` tool

`facets` (cloud `GET /v1/facets`) returns the live facet universe:

```jsonc
{
  "modes": ["hybrid", "vector", "fts"],
  "filters": [
    { "key": "kind",        "type": "enum",       "negatable": true,  "values": ["markdown","code","plaintext"] },
    { "key": "language",    "type": "open_set",   "negatable": true,  "values": ["compact","rust","typescript"] },
    { "key": "tags",        "type": "open_set",   "negatable": true,  "values": ["quickstart","privacy"], "truncated": true, "total": 142 },
    { "key": "ingested_at", "type": "range_temporal", "negatable": false }
    // … one entry per facet
  ]
}
```

- Closed-enum facets carry their full value list. Open-set facets carry
  corpus-derived values.
- High-cardinality sets (`tags`, `symbol.name`, `package.name`) are top-N by
  frequency and flagged `truncated: true` with a `total`. Treat the listed
  values as **examples, not the closed universe** — a value not shown may still
  exist.

## Validation (fail-fast)

Invalid filters return a `400` naming the offending facet and a remediation that
points back at `facets`. The recovery loop is: bad filter → read the message →
call `facets` → fix the key/value → retry. Violations:

- Unknown facet key (lists valid keys).
- Invalid closed-set value (lists valid values).
- Wrong shape for the facet type.
- `none_of` on a non-negatable facet (`language_target`, `sdk_dependency`,
  `verified`, `deprecated`, ranges).
- Contradictory range (`min > max`, `after > before`) or a malformed date.
- Malformed `version_satisfies` semver.
- `mode=vector`/`hybrid` with no vector supplied. `mode=fts` with a vector is
  accepted and the vector ignored.

A *valid* filter that matches nothing returns an empty result set, not an error.

## CLI mapping (`mnm`)

For shell users, `mnm search` exposes the same surface as flags, and `mnm
facets` prints the discovery output:

```
--mode <hybrid|vector|fts>
--kind <markdown|code|plaintext>                 (repeatable → any_of)
--language <lang> / --exclude-language <lang>    (any_of / none_of)
--tag <tag> / --exclude-tag <tag>
--symbol <kind[:name]>                           e.g. --symbol circuit | --symbol :deployContract
--source <slug>  --content-type <t>  --attribution <a>
--no-deprecated  --verified
--ingested-after / --ingested-before <YYYY-MM-DD>
--min-tokens / --max-tokens <n>
--filter-json '<json>'                           escape hatch (mutually exclusive with the granular flags)
```
````

- [ ] **Step 2: Add the `mn-skills`-side drift guard (catalog lists all 17 keys)**

Append this test module to the END of `crates/mn-skills/src/lib.rs` (a sibling `#[cfg(test)] mod` is fine; place it after the existing `tests` module):

```rust
#[cfg(test)]
mod catalog_drift_tests {
    //! Half of the registry↔docs drift guard. The other half lives in
    //! `mn-retrieval`'s `facets.rs` (`registry_is_exactly_v1_keys`), which pins
    //! `facets()` to this same 17-key set. If a facet is added/removed in the
    //! registry, that test fails first; update both the registry pin and this
    //! list, and add the row to `filters-and-modes.md`. (We assert against a
    //! literal list rather than depending on `mn-retrieval`, which would drag
    //! `mn-store`/sqlx into this crate's test build.)

    /// The v1 facet wire keys, mirroring `mn_retrieval::facets::facets()`.
    const FACET_KEYS: &[&str] = &[
        "attribution", "content_type", "kind", "source_kind", "source_slug",
        "language", "tags", "heading_path", "symbol", "package", "verified",
        "deprecated", "language_target", "sdk_dependency", "ingested_at",
        "source_modified_at", "token_count",
    ];

    const FILTERS_AND_MODES: &str =
        include_str!("../assets/midnight-advanced-search/references/filters-and-modes.md");

    #[test]
    fn catalog_documents_every_facet_key() {
        for key in FACET_KEYS {
            let needle = format!("`{key}`");
            assert!(
                FILTERS_AND_MODES.contains(&needle),
                "filters-and-modes.md is missing facet `{key}` from its catalog"
            );
        }
    }
}
```

- [ ] **Step 3: Run the catalog drift test**

Run: `cargo test -p mn-skills catalog_documents_every_facet_key`
Expected: PASS (the file from Step 1 lists all 17 keys in backticks).

- [ ] **Step 4: Commit**

```bash
git add crates/mn-skills/assets/midnight-advanced-search/references/filters-and-modes.md crates/mn-skills/src/lib.rs
git commit -m "docs(skills): add filters-and-modes reference + catalog drift guard"
```

---

## Task 2: Add the exact-key-set pin in the registry (other half of the drift guard)

**Files:**
- Modify: `crates/mn-retrieval/src/facets.rs` (tests module)

- [ ] **Step 1: Write the failing exact-set test**

In `crates/mn-retrieval/src/facets.rs`, inside the existing `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn registry_is_exactly_v1_keys() {
        // Pins the registry to the v1 facet set. The skill docs mirror this list
        // in `mn-skills` (`catalog_documents_every_facet_key`). Changing the
        // registry must be a deliberate edit here AND in
        // `crates/mn-skills/assets/midnight-advanced-search/references/filters-and-modes.md`.
        let mut got: Vec<&str> = facets().iter().map(|f| f.key).collect();
        got.sort_unstable();
        let mut want = [
            "attribution", "content_type", "kind", "source_kind", "source_slug",
            "language", "tags", "heading_path", "symbol", "package", "verified",
            "deprecated", "language_target", "sdk_dependency", "ingested_at",
            "source_modified_at", "token_count",
        ];
        want.sort_unstable();
        assert_eq!(got, want, "registry facet set drifted from the documented v1 catalog");
    }
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p mn-retrieval registry_is_exactly_v1_keys`
Expected: PASS (the registry currently has exactly these 17 keys). If it FAILS, the registry already drifted — stop and reconcile before continuing.

- [ ] **Step 3: Commit**

```bash
git add crates/mn-retrieval/src/facets.rs
git commit -m "test(retrieval): pin facet registry to the documented v1 key set"
```

---

## Task 3: Author the recipes — `references/advanced-techniques.md`

**Files:**
- Create: `crates/mn-skills/assets/midnight-advanced-search/references/advanced-techniques.md`

- [ ] **Step 1: Create the recipes file**

Create `crates/mn-skills/assets/midnight-advanced-search/references/advanced-techniques.md` with EXACTLY this content:

````markdown
# Advanced search techniques

Recipes that combine `mode` + `filters` + multi-query. Shapes and the facet
catalog live in `filters-and-modes.md`. Every concrete value here is
illustrative — confirm real values with the `facets` tool.

## A. Mode-tiered cost escalation

Pick the cheapest mode that can answer, escalate only if it can't.

- **Known literal** (a symbol, CLI flag, error string)? Start in `fts` — it
  skips embedding entirely, so it is the fastest path and nails exact matches:
  `{ "query": "potential witness-value disclosure must be declared", "mode": "fts" }`
- **Thin recall** from `fts`? Escalate the same query to `hybrid` to add
  semantic matches.
- **Purely conceptual** ("how does X relate to Y", no literal anchor)? Go
  straight to `vector` (or `hybrid`).

Rule of thumb: literal → `fts`; fuzzy/conceptual → `vector`; unsure → `hybrid`.

## B. Version & freshness precision (anti-staleness)

The corpus mixes versions and eras. To avoid handing the user advice for the
wrong toolchain, constrain to *their* version and to current material.

- **Pin to the user's toolchain** with `version_satisfies`:

  ```jsonc
  "filters": {
    "language_target": { "any_of": [{ "name": "compact", "version_satisfies": ">=0.23" }] },
    "sdk_dependency":  { "any_of": [{ "kind": "npm", "name": "@midnight-ntwrk/midnight-js", "version_satisfies": "^1.0" }] }
  }
  ```

  (Confirm the real target/dependency names and the user's actual versions
  before trusting these — `facets` lists the names present.)

- **Drop superseded guidance**: add `"deprecated": false`.
- **Prefer current docs**: add a recency floor, e.g.
  `"source_modified_at": { "after": "2026-01-01" }` (or `ingested_at`).
- **Staleness check**: run the query once unfiltered and once with a recency
  floor; if the fresh-only top results differ materially from the unfiltered
  ones, the answer likely changed — surface that to the user instead of trusting
  the older chunk.

## C. Discovery & self-correction

- **Discover before you filter.** Call `facets` first to learn the corpus's real
  languages, tags, sources, and package names. Guessing a value that isn't there
  returns an empty set that *looks* like "no answer".
- **Recover from a 400.** A bad filter returns a `400` naming the facet + a
  pointer to `facets`. Loop: read the message → call `facets` → fix the
  key/value → retry. Never silently give up on a filtered search.
- **Filter ladder (progressive relaxation).** Start tight (version + verified +
  recency + language). If you get too few results, relax ONE facet at a time,
  least-load-bearing first — typically recency, then `verified`, then version —
  re-searching after each drop until results appear. This finds the most
  precise answer the corpus can actually support.

## D. Trust-stratified, differential & symbol-anchored

Heavier patterns that spend several searches — use when correctness matters more
than latency.

- **Differential / trust-stratified search.** Run the *same* query across strata
  and diff the result sets; the server does not detect contradictions for you.
  - authority: `attribution: { any_of: ["foundation"] }` vs
    `attribution: { any_of: ["community"] }`
  - vetting: `verified: true` vs unfiltered
  - era: recent (`ingested_at.after`) vs unfiltered
  Where the strata disagree, surface the disagreement and say which source is
  more authoritative / version-matched.
- **Symbol-anchored code landing.** To land on a named circuit/function/type:
  `{ "query": "deployContract", "mode": "fts",
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

## Composition

These stack. A precision query is a funnel — version + `verified` + recency +
`language` + `content_type` — AND-ed together. When a funnel returns nothing,
switch to the filter ladder (technique C) and relax it one facet at a time.
````

- [ ] **Step 2: Verify the file is valid UTF-8 and present**

Run: `cargo build -p mn-skills`
Expected: still compiles (the file is not yet referenced by code — this just confirms the path/encoding are fine).

- [ ] **Step 3: Commit**

```bash
git add crates/mn-skills/assets/midnight-advanced-search/references/advanced-techniques.md
git commit -m "docs(skills): add advanced-techniques recipes reference"
```

---

## Task 4: Rewrite the core — `SKILL.md`

**Files:**
- Rewrite: `crates/mn-skills/assets/midnight-advanced-search/SKILL.md`

- [ ] **Step 1: Replace the file contents**

Overwrite `crates/mn-skills/assets/midnight-advanced-search/SKILL.md` with EXACTLY this content:

````markdown
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
  (`hybrid` default | `vector` | `fts`), `rerank` (default on), and a typed
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
````

- [ ] **Step 2: Sanity-check frontmatter length**

Run: `cargo test -p mn-skills frontmatter_is_valid_and_name_matches_folder`
Expected: PASS (description is non-empty and ≤ 1024 chars; name matches the folder). If it FAILS on the length cap, trim the `description` block and re-run.

- [ ] **Step 3: Commit**

```bash
git add crates/mn-skills/assets/midnight-advanced-search/SKILL.md
git commit -m "docs(skills): rewrite SKILL.md core for modes + filters; link references"
```

---

## Task 5: Manifest + self-containment / completeness guards (`lib.rs`)

**Files:**
- Modify: `crates/mn-skills/src/lib.rs`

- [ ] **Step 1: Add the manifest and redefine `skill_markdown()` over it**

In `crates/mn-skills/src/lib.rs`, replace the existing `skill_markdown()` definition (the doc-comment + `pub const fn skill_markdown() -> &'static str { include_str!(...) }` block, around lines 26–31) with:

```rust
/// The three skill files, embedded at build time, as `(relative path, body)`.
/// This is the bundle the installer ships verbatim into every harness. `SKILL.md`
/// is always entry 0.
#[must_use]
pub const fn skill_files() -> &'static [(&'static str, &'static str)] {
    &[
        ("SKILL.md", SKILL_MD),
        ("references/filters-and-modes.md", REF_FILTERS_AND_MODES),
        ("references/advanced-techniques.md", REF_ADVANCED_TECHNIQUES),
    ]
}

/// The canonical `SKILL.md` body (bundle entry 0). Kept as a convenience for the
/// frontmatter tests and any single-file consumer.
#[must_use]
pub const fn skill_markdown() -> &'static str {
    SKILL_MD
}

const SKILL_MD: &str = include_str!("../assets/midnight-advanced-search/SKILL.md");
const REF_FILTERS_AND_MODES: &str =
    include_str!("../assets/midnight-advanced-search/references/filters-and-modes.md");
const REF_ADVANCED_TECHNIQUES: &str =
    include_str!("../assets/midnight-advanced-search/references/advanced-techniques.md");
```

Also update the `install::` re-export line if needed (it stays the same; `skill_files` is exported via `pub mod`/`pub use` already because it's a free function in `lib.rs` — no `pub use` needed).

- [ ] **Step 2: Replace the cookbook-link test with bundle guards**

In `crates/mn-skills/src/lib.rs`, DELETE the `body_links_the_cookbook_for_dryness` test (around lines 82–88) and add these three tests in its place inside the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn bundle_is_self_contained() {
        // No shipped file may point an installed agent at a path that only
        // exists in this repo. The bundle must stand alone in the user's harness.
        for (path, body) in skill_files() {
            assert!(
                !body.contains("docs/cookbook/"),
                "{path} references a repo-only path (docs/cookbook/)"
            );
            assert!(
                !body.contains("in the midnight-manual repo"),
                "{path} points the agent at the repo instead of being self-contained"
            );
        }
        // SKILL.md must link both bundled references (relative paths only).
        let skill = skill_markdown();
        assert!(skill.contains("references/filters-and-modes.md"));
        assert!(skill.contains("references/advanced-techniques.md"));
    }

    #[test]
    fn manifest_is_complete() {
        let keys: Vec<&str> = skill_files().iter().map(|(p, _)| *p).collect();
        assert_eq!(keys[0], "SKILL.md", "SKILL.md must be bundle entry 0");
        // Every reference SKILL.md links must be a real manifest entry.
        let skill = skill_markdown();
        for (path, _) in skill_files() {
            if *path == "SKILL.md" {
                continue;
            }
            assert!(
                skill.contains(path),
                "manifest ships `{path}` but SKILL.md never links it"
            );
        }
    }

    #[test]
    fn skill_files_are_nonempty() {
        for (path, body) in skill_files() {
            assert!(!body.trim().is_empty(), "{path} is empty");
        }
    }
```

- [ ] **Step 3: Run the lib tests**

Run: `cargo test -p mn-skills --lib`
Expected: PASS — `bundle_is_self_contained`, `manifest_is_complete`, `skill_files_are_nonempty`, `catalog_documents_every_facet_key`, and the existing frontmatter/name tests all pass. The deleted `body_links_the_cookbook_for_dryness` is gone.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-skills/src/lib.rs
git commit -m "feat(skills): embed bundle manifest; guard self-containment + completeness"
```

---

## Task 6: Install writes the whole bundle and prunes orphans (`install.rs`)

**Files:**
- Modify: `crates/mn-skills/src/install.rs`

- [ ] **Step 1: Write a failing test for multi-file install + prune**

In `crates/mn-skills/src/install.rs`, inside `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn install_writes_every_bundle_file() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, Scope::User, &env).unwrap();
        let dir = report.installed[0].path.parent().unwrap().to_path_buf();
        for &(rel, body) in crate::skill_files() {
            let mut p = dir.clone();
            for seg in rel.split('/') {
                p.push(seg);
            }
            assert!(p.exists(), "missing bundled file {rel}");
            assert_eq!(std::fs::read_to_string(&p).unwrap(), body, "{rel} content mismatch");
        }
    }

    #[test]
    fn reinstall_prunes_orphans_and_reports_updated() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, Scope::User, &env).unwrap();
        let dir = report.installed[0].path.parent().unwrap().to_path_buf();
        // Drop a stray file at the root and inside references/.
        std::fs::write(dir.join("stray.md"), "junk").unwrap();
        std::fs::write(dir.join("references").join("orphan.md"), "junk").unwrap();

        let again = install(None, Scope::User, &env).unwrap();
        assert_eq!(again.installed[0].action, InstallAction::Updated, "prune must mark Updated");
        assert!(!dir.join("stray.md").exists(), "root orphan not pruned");
        assert!(!dir.join("references").join("orphan.md").exists(), "nested orphan not pruned");
        // Manifest files survive the prune.
        assert!(dir.join("SKILL.md").exists());
        assert!(dir.join("references").join("filters-and-modes.md").exists());
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p mn-skills --lib install_writes_every_bundle_file reinstall_prunes_orphans_and_reports_updated`
Expected: FAIL — `install_writes_every_bundle_file` fails because only `SKILL.md` is written today; `reinstall_prunes...` fails because nothing prunes and the action is `Unchanged`.

- [ ] **Step 3: Rewrite the `install` loop to ship the manifest and prune**

In `crates/mn-skills/src/install.rs`, change the `use` line (currently `use crate::{skill_markdown, SkillEnv, SKILL_NAME};`) to:

```rust
use crate::{skill_files, SkillEnv, SKILL_NAME};
```

> This drops `skill_markdown` from the module's imports. One existing test still calls it — `install_overwrites_stale_content_as_updated` has `assert_eq!(std::fs::read_to_string(&path).unwrap(), skill_markdown());`. Change that one reference to `crate::skill_markdown()` (the fn still exists; it's just no longer imported here). Do this now so the crate keeps compiling.

Then replace the body of `pub fn install(...)` from `let body = skill_markdown();` through the end of the `for h in targets { ... }` loop with:

```rust
    let files = skill_files();
    let mut installed = Vec::with_capacity(targets.len());
    for h in targets {
        let dir = h.skill_dir(scope, &base);
        let dir_existed = dir.exists();
        let mut changed = false;

        // Write every manifest file, creating parent dirs as needed.
        for &(rel, body) in files {
            let file = join_rel(&dir, rel);
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent)
                    .map_err(|source| SkillError::Io { path: parent.to_path_buf(), source })?;
            }
            let up_to_date = fs::read_to_string(&file).map(|c| c == body).unwrap_or(false);
            if !up_to_date {
                write_file(&file, body)?;
                changed = true;
            }
        }

        // Prune any file in the owned dir that the manifest does not ship.
        if dir_existed && prune_orphans(&dir, files)? {
            changed = true;
        }

        let action = if !dir_existed {
            InstallAction::Created
        } else if changed {
            InstallAction::Updated
        } else {
            InstallAction::Unchanged
        };

        installed.push(HarnessInstall {
            harness: h.id().to_owned(),
            scope: scope.as_str().to_owned(),
            path: h.skill_file(scope, &base),
            action,
            reload_step: h.reload_step().to_owned(),
        });
    }
```

(Keep the trailing `Ok(InstallReport { ... })` exactly as it is.)

- [ ] **Step 4: Add the `join_rel` and `prune_orphans` helpers**

In `crates/mn-skills/src/install.rs`, next to `write_file`, add:

```rust
/// Join a manifest-relative path (which uses `/` separators) onto `dir`
/// component-by-component, so it is correct on every platform.
fn join_rel(dir: &std::path::Path, rel: &str) -> PathBuf {
    let mut p = dir.to_path_buf();
    for seg in rel.split('/') {
        p.push(seg);
    }
    p
}

/// Delete any regular file under `dir` whose path is not shipped by `files`.
/// Scoped strictly to the owned skill dir; leaves directories in place. Returns
/// `true` if anything was removed.
fn prune_orphans(
    dir: &std::path::Path,
    files: &[(&str, &str)],
) -> Result<bool, SkillError> {
    use std::collections::HashSet;
    let owned: HashSet<PathBuf> = files.iter().map(|&(rel, _)| join_rel(dir, rel)).collect();
    let mut removed = false;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = fs::read_dir(&d).map_err(|source| SkillError::Io { path: d.clone(), source })?;
        for entry in entries {
            let entry = entry.map_err(|source| SkillError::Io { path: d.clone(), source })?;
            let path = entry.path();
            let ft = entry
                .file_type()
                .map_err(|source| SkillError::Io { path: path.clone(), source })?;
            if ft.is_dir() {
                stack.push(path);
            } else if !owned.contains(&path) {
                fs::remove_file(&path).map_err(|source| SkillError::Io { path: path.clone(), source })?;
                removed = true;
            }
        }
    }
    Ok(removed)
}
```

- [ ] **Step 5: Fix the existing idempotency test's stale-write assertion**

The existing `install_overwrites_stale_content_as_updated` test writes `"stale body"` to the SKILL.md path and asserts the reinstall rewrites it to `skill_markdown()`. That still holds (SKILL.md is manifest entry 0). Leave it. The existing `install_then_reinstall_is_idempotent` test asserts the second install is `Unchanged` — that also still holds (no orphans, all files identical). No edits needed; just confirm they still compile against the new loop.

- [ ] **Step 6: Run the install tests**

Run: `cargo test -p mn-skills --lib install`
Expected: PASS — the two new tests plus all existing `install*` tests (`install_then_reinstall_is_idempotent`, `install_overwrites_stale_content_as_updated`, `explicit_harness_forces_install_even_when_undetected`, `autodetect_with_no_harness_errors`, `not_detected_lists_absent_harnesses_on_autodetect`, `install_propagates_non_notfound_read_error`, `install_project_scope_writes_under_repo_root`).

> Note: `install_propagates_non_notfound_read_error` makes `SKILL.md` a directory so `read_to_string` fails with a non-NotFound error. The new loop calls `fs::read_to_string(&file)` inside `matches!(...)`, which treats that error as "not up to date" and then `write_file` fails with `Io` — so the test's `SkillError::Io` expectation still holds. If it instead fails earlier in `create_dir_all`, that is still `Io`. Confirm it passes; if it does not, adjust the test's comment but keep the `Io` expectation.

- [ ] **Step 7: Commit**

```bash
git add crates/mn-skills/src/install.rs
git commit -m "feat(skills): install ships the whole bundle and prunes orphans"
```

---

## Task 7: Status is up-to-date only when every file matches (`install.rs`)

**Files:**
- Modify: `crates/mn-skills/src/install.rs` (the `status` fn + tests)

- [ ] **Step 1: Write a failing test for stale-reference detection**

In `crates/mn-skills/src/install.rs` tests, add:

```rust
    #[test]
    fn status_not_up_to_date_when_a_reference_is_stale() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        install(None, Scope::User, &env).unwrap();
        // Make ONLY a reference stale; SKILL.md is untouched.
        let dir = Harness::ClaudeCode.skill_dir(Scope::User, &env.home);
        std::fs::write(dir.join("references").join("advanced-techniques.md"), "stale").unwrap();

        let st = status(Scope::User, &env).unwrap();
        let cc = st.harnesses.iter().find(|h| h.harness == "claude-code").unwrap();
        assert!(cc.installed, "SKILL.md still present → installed");
        assert!(!cc.up_to_date, "a stale reference must make up_to_date false");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p mn-skills --lib status_not_up_to_date_when_a_reference_is_stale`
Expected: FAIL — `status` only compares `SKILL.md` today, so a stale reference is reported up-to-date.

- [ ] **Step 3: Make `status` compare every manifest file**

In `crates/mn-skills/src/install.rs`, in `pub fn status(...)`, replace the per-harness closure body. Change the `body`/`installed_content`/`up_to_date` computation so `installed` is keyed on `SKILL.md` presence but `up_to_date` requires every manifest file to match. Replace:

```rust
    let base = base_dir(scope, env)?;
    let body = skill_markdown();
    let harnesses = Harness::ALL
        .into_iter()
        .map(|h| {
            let file = h.skill_file(scope, &base);
            let detected = h.markers(scope, &base).iter().any(|m| m.exists());
            let installed_content = fs::read_to_string(&file).ok();
            let installed = installed_content.is_some();
            let up_to_date = installed_content.as_deref() == Some(body);
            HarnessStatus {
                harness: h.id().to_owned(),
                scope: scope.as_str().to_owned(),
                detected,
                installed,
                up_to_date,
                path: file,
            }
        })
        .collect();
```

with:

```rust
    let base = base_dir(scope, env)?;
    let files = skill_files();
    let harnesses = Harness::ALL
        .into_iter()
        .map(|h| {
            let file = h.skill_file(scope, &base);
            let dir = h.skill_dir(scope, &base);
            let detected = h.markers(scope, &base).iter().any(|m| m.exists());
            // `installed` keys on the primary file (SKILL.md); `up_to_date`
            // requires every bundled file to be present and byte-identical.
            let installed = file.exists();
            let up_to_date = installed
                && files.iter().all(|&(rel, body)| {
                    fs::read_to_string(join_rel(&dir, rel)).map(|got| got == body).unwrap_or(false)
                });
            HarnessStatus {
                harness: h.id().to_owned(),
                scope: scope.as_str().to_owned(),
                detected,
                installed,
                up_to_date,
                path: file,
            }
        })
        .collect();
```

- [ ] **Step 4: Run the status tests**

Run: `cargo test -p mn-skills --lib status`
Expected: PASS — the new `status_not_up_to_date_when_a_reference_is_stale` plus the existing `status_reports_installed_and_stale` (which makes `SKILL.md` itself stale and still expects `installed && !up_to_date`).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-skills/src/install.rs
git commit -m "feat(skills): status up_to_date spans every bundle file"
```

---

## Task 8: Refresh the human cookbook (secondary)

**Files:**
- Modify: `docs/cookbook/query-enhancement.md`

- [ ] **Step 1: Append a "Modes & filters" section**

At the END of `docs/cookbook/query-enhancement.md` (after the "Combining patterns" section), append (the four-backtick wrapper below is only the plan's quoting — the file gets a normal triple-backtick `bash` block):

````markdown

---

## Modes & filters

Query enhancement (above) shapes *what you ask*. Two newer controls shape *how
the server searches* and *what it searches over*:

- **`mode`** — `hybrid` (default, full-text + vector), `vector` (semantic only),
  or `fts` (full-text only, which skips embedding entirely and is the
  lowest-latency mode for exact identifiers and error strings).
- **`filters`** — a per-facet object: `{ "any_of": [...], "none_of": [...] }`
  for set facets, bare bools (`verified`, `deprecated`), and `{after,before}` /
  `{min,max}` ranges. Combination is AND across facets, OR within `any_of`. A
  misspelled facet or invalid value returns a `400` (not a silent drop).

The highest-value use is matching the reader's toolchain: pin `language_target`
/ `sdk_dependency` with `version_satisfies`, add `"deprecated": false`, and a
recency floor on `ingested_at` — so you retrieve current, version-matched
material instead of stale advice.

Discover the corpus's real facet values (languages, tags, sources, packages)
with the `facets` MCP tool, or from the shell:

```bash
mnm facets
mnm search "deployContract" --mode fts --kind code --symbol :deployContract
```

The agent-facing playbook (the `midnight-advanced-search` skill, installable via
`mnm skills install` or the `install_search_skill` MCP tool) carries the full
facet catalog and the advanced filter/mode techniques.
````

- [ ] **Step 2: Verify the doc still renders (no broken fences)**

Run: `grep -c '```' docs/cookbook/query-enhancement.md`
Expected: an EVEN number (every fence opened is closed).

- [ ] **Step 3: Commit**

```bash
git add docs/cookbook/query-enhancement.md
git commit -m "docs(cookbook): add Modes & filters section + facets pointer"
```

---

## Task 9: Full verification

**Files:** none (verification only)

- [ ] **Step 1: Full crate test + workspace build**

Run: `cargo test -p mn-skills -p mn-retrieval`
Expected: PASS (all skill bundle + facet registry tests).

Run: `cargo build -p mn-cli -p mn-server`
Expected: builds clean — confirms no caller broke from the `skill_markdown()` → `skill_files()` change.

- [ ] **Step 2: Lint + format**

Run: `cargo clippy -p mn-skills -p mn-retrieval --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --check`
Expected: clean (run `cargo fmt` and re-commit if not).

- [ ] **Step 3: Real install smoke test into a throwaway HOME**

Run:
```bash
TMPHOME="$(mktemp -d)"; mkdir -p "$TMPHOME/.claude"
HOME="$TMPHOME" cargo run -q -p mn-cli -- skills install --harness claude-code --scope user
find "$TMPHOME/.claude/skills/midnight-advanced-search" -type f | sort
```
Expected: three files printed —
`.../midnight-advanced-search/SKILL.md`,
`.../references/advanced-techniques.md`,
`.../references/filters-and-modes.md`.

(If the CLI subcommand name differs, discover it with `cargo run -q -p mn-cli -- skills --help`; the noun is `skills` with `install`/`status`/`remove` — see `crates/mn-cli/src/commands/skills/mod.rs`.)

- [ ] **Step 4: Eyeball the installed bundle**

Run: `sed -n '1,40p' "$TMPHOME/.claude/skills/midnight-advanced-search/SKILL.md"`
Expected: the new frontmatter + "The tools you have" listing `facets`; no `docs/cookbook` reference anywhere in the installed tree:
Run: `grep -rl 'docs/cookbook' "$TMPHOME/.claude/skills/midnight-advanced-search" || echo OK-self-contained`
Expected: `OK-self-contained`.

- [ ] **Step 5: Clean up**

Run: `rm -rf "$TMPHOME"`

- [ ] **Step 6: Final commit (if fmt/clippy required edits)**

```bash
git add -A
git commit -m "chore(skills): fmt/clippy pass for advanced-search bundle" || echo "nothing to commit"
```

---

## Self-review notes (for the author, not a task)

- **Spec coverage:** §4.1 layout → Tasks 1/3/4; §4.2 SKILL.md core (B,C,A promoted) → Task 4; §4.3 filters-and-modes → Task 1; §4.4 advanced-techniques (incl. D) → Task 3; §4.5 installer manifest+prune → Tasks 5/6/7; §5 tests (self-containment, manifest completeness, registry drift, install/status/prune) → Tasks 1/2/5/6/7; §4.6 cookbook → Task 8.
- **Drift-guard adaptation:** the design's single "every `facets()` key appears in the catalog" guard is implemented as a two-sided pin (Task 1 in `mn-skills`, Task 2 in `mn-retrieval`) to avoid making `mn-skills` depend on `mn-store`/sqlx via `mn-retrieval`. Both lists are the same 17 keys and reference each other in comments.
- **Type/name consistency:** `skill_files() -> &'static [(&'static str, &'static str)]`, `skill_markdown() -> &'static str`, `join_rel`, `prune_orphans` are used identically across Tasks 5–7. `InstallAction::{Created,Updated,Unchanged}` semantics: Created (dir absent) / Updated (any file written or any orphan pruned) / Unchanged (all identical, nothing pruned).
- **No behavior change** to server/MCP/CLI retrieval; only the skill content and the installer file-set change.

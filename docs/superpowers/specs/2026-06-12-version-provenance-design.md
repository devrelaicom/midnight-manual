# Version Provenance & Matching — Design

**Date**: 2026-06-12
**Status**: Approved for planning
**Replaces**: the `unsatisfied` version-match knob; the concrete-only `version_satisfies` semantics

## Summary

Version-aware retrieval gets data and better semantics. At ingest, version provenance is
**extracted per document** from code content: the Compact `language_version` pragma becomes
a `language_targets` entry, allowlisted Midnight dependencies from the nearest
`package.json`/`Cargo.toml` become `sdk_dependencies`, and the package's own version
finally populates `package.version`. At query time, `version_satisfies` accepts a
**concrete version or a semver range**, and version matching gains two modes:
**permissive (new default)** — a soft preference that boosts satisfying content, applies a
distance-scaled trust penalty to near misses, hard-drops breaking mismatches, and lets
version-silent content through at neutral — and **strict** (today's hard-filter
behavior, now opt-in). `/v1/facets` learns to enumerate version values via a two-level
drill-down, and the bundled `midnight-advanced-search` skill is rewritten around the new
semantics. No schema migration is needed; populating production is a routine re-ingest.

## Background

### Current state

A 2026-06-12 deep audit established that the version-matching machinery is fully built
and test-proven but operates on zero data:

- **No production content carries version provenance.** All 46 manifests under
  `manifests/midnight/` declare only attribution/verified/tags; upstream foundation repos
  (midnight-docs, compact, midnight-js; ~2,400 markdown files) have zero
  `language_targets`/`sdk_dependencies`/`deprecation` frontmatter keys. Live prod
  version-filtered searches return 0 candidates corpus-wide.
- **Extraction drops the signals it already touches.** The Compact
  `pragma language_version >= 0.23` is parsed by `compactp` and discarded
  (`mn-content/src/code/compact.rs:219`). Package detection reads `[package].name` /
  `.name` but never the adjacent `.version` (`mn-content/src/package.rs`), and the sole
  `package::upsert` caller hardcodes `None` (`mn-server/src/routes/admin_ingest.rs:915`).
- **Filter semantics**: SQL matches `language_target`/`sdk_dependency` *names* via
  `jsonb_array_elements` over `document.provenance`
  (`mn-server/src/routes/search.rs:1726-1752`); the semver refinement runs in Rust
  post-fetch (`SearchFilters::semver_post_match`, `mn-retrieval/src/filters.rs:279-313`)
  and **hard-drops** non-satisfying candidates before scoring. The request side accepts
  only a **concrete** version (`parse_version`, `mn-core/src/scoring.rs:277-292`); the
  declared side is a `semver::VersionReq` range.
- **Scoring**: `version_match_multiplier` (satisfies 1.15 / neutral 1.00 / unsatisfied
  0.70, `mn-core/src/scoring_policy.rs:89-95`) feeds the five-factor trust product. The
  0.70 branch is unreachable platform-wide because the hard filter removes those
  candidates first. Only the **first** `language_target.any_of` element feeds scoring and
  the derived rerank instruction (`mn-server/src/routes/search.rs:779-787`,
  `mn-core/src/rerank.rs:108-127`). `sdk_dependency` never affects scoring.
- **Doc bug**: the shipped skill assets and `openapi.yaml` teach range syntax
  (`">=0.23"`, `"^1.2"`) that all three validators reject with a 400 (shipped in commit
  03c1ede / PR #74). No drift guard checks filter-value semantics.
- **Facets**: `language_target`/`sdk_dependency` advertise `object_set` with no values;
  the drill-down hard-rejects them (`mn-server/src/routes/facets.rs:214-221`), so corpus
  version coverage is undiscoverable.

### Decisions (settled during brainstorming)

1. **Permissive is the default mode**; strict is opt-in via the request.
2. **Permissive is a soft preference**: the SQL name gate is dropped; version-silent
   content survives at neutral. This restores the original D24/README promise ("version
   match is a boost") as the default behavior.
3. **Dependency extraction uses a hardcoded Midnight allowlist** (`@midnight-ntwrk/*`
   npm scope, `midnight-*` cargo prefix, OpenZeppelin Compact packages — exact names
   confirmed at implementation time).
4. **Penalty curve is linear steps with a floor**: `max(floor, 1.0 − step × distance)`,
   defaults `patch_step = 0.05`, `minor_step = 0.15`, `floor = 0.30`.
5. **Bare versions stay concrete** (`"0.31"` ≠ `^0.31`); ranges require explicit
   operators.
6. **The 0.x role shift applies** (cargo/npm caret convention): when major == 0, a minor
   mismatch is breaking and a patch mismatch scales as minor; when major and minor are
   both 0, every mismatch is breaking. Without this, nothing in the Midnight ecosystem
   (Compact 0.23/0.31, many 0.x packages) would ever hard-drop.
7. **Compatibility-matrix modeling is out of scope.** The support matrix is corpus
   content; cross-component reasoning is agent work. The skill gains a retrieval playbook
   instead.
8. **No enrich command; manifest curation is opportunistic.** Machine-detectable
   signals belong in the pipeline (manifest snapshots go stale); manifests remain the
   authoring surface for prose subtrees we don't own.

## 1. Extraction at ingest

Scope: **code documents only** (`language ∈ {compact, rust, typescript, javascript}`).
Prose stays manifest/frontmatter-authored. Extraction runs caller-side in `mn-cli`'s
ingest path, next to the existing `detect_package_ref`
(`mn-cli/src/commands/ingest/run.rs:1453-1472`) — the planner remains I/O-free. The
extraction *logic* lives in `mn-content` so future consumers (e.g. server-side ingest)
reuse it.

### 1.1 Extractors

- **Compact pragma** → via `compactp_ast`'s `SourceFile::pragmas()` accessor (the crate
  already exposes `Pragma` nodes with name + version-expr; `compactp_ast` v0.1.0-beta.1,
  `nodes.rs:56-100`). The `language_version` pragma's version expression is captured
  verbatim (normalized whitespace) as
  `language_targets: [{name: "compact", version_constraint: ">=0.23"}]`. v1 extracts
  **only** the `language_version` pragma — the legacy `pragma compact X` form states a
  *compiler* version, and conflating it with language constraints would be wrong. Files
  with no pragma contribute nothing (absence stays the default).
- **Dependencies** → reuse the `package.rs` walk-up to the nearest manifest. From npm
  `dependencies` and cargo `[dependencies]` (dev-dependencies excluded — tooling is not
  what the code targets), entries matching the Midnight allowlist become
  `sdk_dependencies: [{kind, name, version_constraint}]` with the declared range
  verbatim. Workspace-inherited cargo deps (`workspace = true`) resolve against the
  workspace root's `[workspace.dependencies]`; if unresolvable, the entry is emitted
  without a constraint.
- **Own package version** → `PackageRef` (`mn-core/src/types.rs:241-248`) gains
  `version: Option<String>` read from `[package].version` / `.version`;
  `DocumentUpload.package` carries it; `admin_ingest.rs:915` passes it through to
  `package::upsert` instead of `None`.

### 1.2 Merge precedence

`merge_provenance` (`mn-content/src/ingest/plan.rs:333-370`) gains a middle layer:

```text
frontmatter  >  extracted  >  manifest node (inherited)
```

Same rules as today: per-field override, non-empty lists replace wholesale. Rationale:
frontmatter is a deliberate per-file human statement; extraction is per-file machine
truth from the ingested commit; manifest nodes are coarse human defaults that only fill
gaps (`manifest/resolve.rs:30-32` already documents this contract).

### 1.3 Opt-out and visibility

- Manifest nodes get an inheritable `no_extract: true` flag (default false) for subtrees
  with misleading signals (e.g. archived examples with stale pragmas).
- The ingest run summary and `--report` output gain extraction counts per source:
  documents with extracted `language_targets`, with extracted `sdk_dependencies`, and
  packages with versions.

No schema change. No new API surface beyond the `version` field on the upload's package
ref. Populating production = re-ingest after deploy (already routine).

## 2. Request-side ranges and classification

### 2.1 Parsing

`version_satisfies` accepts a concrete version **or** a semver range:

1. Try concrete via the existing `parse_version` (pads partials, strips leading `v`,
   drops pre-release/build suffixes) → a **degenerate interval** `[v, v]`.
2. Else parse as `semver::VersionReq` and desugar to an interval. The `semver` crate's
   grammar has no OR operator, so every requirement is a conjunction of comparators =
   one contiguous interval `[lo, hi)` (either bound may be open/unbounded). Comparators
   desugar per cargo rules (`^`, `~`, wildcards, `=`, inequalities).
3. Neither parses, or the desugared interval is empty (contradictory comparators) →
   `FilterError` → 400, message naming the value and both accepted forms.

The interval type and desugaring live in `mn-core` next to `parse_version` (shared by
filtering and scoring, as today). Hand-rolled — no new dependencies; correctness is
property-tested against `VersionReq::matches` as the oracle.

### 2.2 Classification

New shared function in `mn-core`:

```text
classify(requested: Interval, declared: Option<Interval>)
  -> Satisfies | NearMiss { class: Patch | Minor, distance: u32 } | Breaking
     (declared = None, i.e. no matching-name target → Silent, decided by the caller)
```

- **Satisfies**: intervals intersect (concrete request ⇒ today's `req.matches(v)`
  semantics, unchanged).
- **Disjoint intervals**: compare the nearest endpoints (requested side vs declared
  side). After the 0.x role shift, the highest differing component determines the class:
  major-role difference → **Breaking**; minor-role → `NearMiss { Minor, distance }`;
  patch-role → `NearMiss { Patch, distance }`. Distance = numeric difference of that
  component between the nearest endpoints.
- **0.x role shift**: if the nearest endpoints have major == 0, minor plays the major
  role and patch plays the minor role; if major == 0 and minor == 0, every mismatch is
  Breaking. A declared target with no `version_constraint` is `Satisfies` for any
  request (unchanged).
- An unparseable **declared** constraint classifies as `Unknown`: strict drops it (it
  never satisfies — unchanged from today), permissive scores it neutral (unknowable is
  not provably incompatible; treating it like Silent avoids punishing content for
  malformed metadata more than for absent metadata).

Multi-element requests: classification runs against **every** requested `any_of`
element with a matching name; the best class wins (fixes today's first-element-only
wart). The winning element also drives the derived rerank instruction.

## 3. Modes and scoring

### 3.1 Request surface

`SearchRequest` gains `version_match: Option<VersionMatchMode>` —
`"strict" | "permissive"`, default **permissive**. Applies to both `language_target` and
`sdk_dependency` facets. Exposed identically on `/v1/search`, the MCP `advanced_search`
tool, and `mnm search --filter-json` (which forwards the request body; no new granular
CLI flag in v1).

### 3.2 Strict (today's behavior, now opt-in)

- SQL name gate stays (`EXISTS … lt->>'name' = ANY(...)`).
- `semver_post_match` drops everything not `Satisfies`.
- Scoring sees only Satisfies (1.15) or Silent (1.00) — unchanged from today.

### 3.3 Permissive (new default)

- **No SQL name gate** for these two facets — the filter becomes a pure ranking signal.
  All other facets are unaffected.
- Post-fetch, each candidate is classified per facet:
  - `Breaking` → **dropped** (the only removal permissive performs).
  - `NearMiss { class, distance }` → multiplier
    `max(floor, 1.0 − step(class) × distance)`.
  - `Silent` (no matching-name target) → `neutral` (1.00).
  - `Satisfies` → `satisfies` (1.15).
- When both `language_target` and `sdk_dependency` filters are present, the combined
  version multiplier is `min(language_mult, sdk_mult)` — worst offender, so stacked
  filters don't compound multiplicatively.
- `sdk_dependency` thereby joins scoring for the first time (in strict mode it remains
  filter-only, as today).

### 3.4 Scoring policy

`[version_match]` in the policy TOML becomes:

```toml
[version_match]
satisfies  = 1.15
neutral    = 1.00
floor      = 0.30   # replaces `unsatisfied`
patch_step = 0.05
minor_step = 0.15
```

Hard cutover: the `unsatisfied` key is removed; `deny_unknown_fields` makes stale policy
files fail loudly at startup (acceptable pre-1.0). `schema_version` stays 1. Compiled-in
defaults updated to match.

### 3.5 Result exposure

`ConfidenceFactors` gains `version_match_class`
(`"satisfies" | "near_miss" | "silent" | "unknown"`; Breaking results never appear) and
`version_distance: Option<u32>`, alongside the existing `version_match_multiplier`,
`language_target_query`, and `language_targets_chunk`. Both new fields are present only
when the request carried a version-bearing filter; otherwise they are omitted (and the
multiplier stays 1.00, as today). The query echo extends to the
winning element and to `sdk_dependency` when that facet drove the multiplier.

### 3.6 Rerank interaction

The derived default instruction ("Prefer content applying to {name} version {ver};
deprioritize other versions.") is unchanged mechanically but now derives from the
best-classified element rather than blindly from the first. Agent-supplied
`rerank_instructions` still replace it wholesale.

## 4. Facet enumeration

Two-level drill-down keeping `FacetValuesPage` values scalar:

| Facet | Level 1 (`facet=`) | Level 2 (`facet=…&within=`) |
|---|---|---|
| `language_target` | distinct names (`compact`, …) | distinct `version_constraint` strings for that name |
| `sdk_dependency` | distinct `kind:name` composites (the `symbol` facet's existing syntax precedent) | distinct constraints for that composite |
| `package` | distinct names (today's behavior) | distinct `version` values for that name (non-NULL only) |

- `within` is a new optional drill parameter; supplying it for a facet without levels is
  a 400 naming the drillable facets.
- Aggregation queries scan `jsonb_array_elements` over `document.provenance` scoped to
  `sv.is_active = true`, keyset-paginated, behind the existing 60s TTL cache. Accepted
  at current corpus scale; a `package`-style side table is the documented future escape
  hatch if it slows.
- Registry metadata (`mn-retrieval/src/facets.rs`) marks both object-set facets
  drillable; the facets overview advertises the levels. mcp-tools.json regenerates via
  the contract-sync test; the MCP `facets` tool schema gains the two facets in its enum
  plus the `within` param.

## 5. Skill and docs

`crates/mn-skills/assets/midnight-advanced-search/` rewrite:

- **Permissive-by-default story**: version filters are safe on any search — they bias
  rather than restrict, dropping only breaking mismatches among version-declaring
  content. Strict is the opt-in for exact pinning (typically with `code_mode` searches,
  where extraction guarantees structured targets exist).
- **Prose guidance**: tutorials/guides mostly carry no structured targets; put the
  version in the query text (helps the FTS arm and contextualized embeddings) and use
  recency floors + `deprecated: false` for staleness control.
- **Support-matrix playbook**: for compatibility questions, retrieve the support matrix
  first, derive concrete versions, then issue version-pinned follow-ups.
- **Recovery ladder**: zero results under strict → retry permissive → drop the version
  filter, before concluding the corpus lacks the content.
- **Examples show both forms** — concrete (`"0.31"`) and range (`">=0.23"`), both now
  valid. The catalog rows in `references/filters-and-modes.md` and the worked examples
  in `references/advanced-techniques.md` / `references/rerank-instructions.md` update
  accordingly, as does facet discovery (now genuinely possible).

Also: README ranking section (the boost/penalty story is now accurate by default),
`docs/cookbook/query-enhancement.md` gains a worked version-filter example, an
authoring-guideline note ("state toolchain versions in tutorial intros") lands in the
ingestion cookbook, `openapi.yaml` is updated best-effort (explicitly unenforced), and
CLAUDE.md gets a Recent Changes entry.

## 6. Error handling

- Unparseable `version_satisfies` (neither concrete nor range, or empty interval) → 400
  `invalid_request` at all three validation points (server route, MCP boundary, CLI
  `--filter-json`), message naming the offending value and both accepted forms.
- Unknown `version_match` value → 400 listing `strict | permissive`.
- Extraction failures are never fatal to ingest: a pragma that fails to parse, an
  unreadable manifest, or an unresolvable workspace dep logs a warning, contributes
  nothing, and the document ingests with whatever the other layers provide (mirrors the
  malformed-frontmatter policy).
- Stale scoring-policy TOML (old `unsatisfied` key) → startup failure with the existing
  policy-validation error path naming the file.

## 7. Testing

- **mn-core**: proptest — interval desugar + intersection agree with
  `VersionReq::matches` on generated version/requirement pairs; classifier table tests
  (0.x role shift, 0.0.x, pre-release stripping, unbounded constraints, unparseable
  declared constraints per mode, distance at boundaries); penalty-curve pins (`floor` clamp, step math); policy TOML round-trip
  with the new knobs and rejection of `unsatisfied`.
- **mn-retrieval**: validation accepts concrete + range, rejects garbage/empty
  intervals; `semver_post_match` successor (`classify_post_match`) unit tests per mode.
- **mn-server (integration, CI)**: strict vs permissive end-to-end — breaking dropped,
  near-miss surfaced with multiplier and class in `confidence_factors`, silent at
  neutral, satisfies boosted; SQL name gate present in strict / absent in permissive;
  facet drill levels for all three facets; `min()` combination when both facets filter.
- **mn-content / mn-cli**: extraction fixtures — a test corpus with pragma'd `.compact`
  files, a `package.json` with allowlisted + non-allowlisted deps, a cargo workspace
  with inherited deps; three-layer merge precedence; `no_extract` opt-out; ingest report
  counts; `package.version` flows to the upload.
- **Drift guard (new)**: a test extracts the JSON filter examples from the three skill
  reference files and runs them through `SearchFilters::validate()` — the guard that
  would have caught the range-syntax bug.
- **Contract**: `contract_sync.rs` regeneration for the new `version_match` field and
  facet drill params.
- **Recall probe (manual, gates skill wording)**: seed documents with and without intro
  version statements, run version-qualified queries against real embeddings, and check
  whether semantic version discrimination actually works. The probe script and its
  outcome are documented; the skill's claims about semantic version matching are worded
  to match the evidence.

## 8. Out of scope

Compatibility-matrix modeling; a manifest `enrich` command; per-path pattern rules for
release-notes trees; prepending document context to rerank inputs; GIN/side-table
indexing for provenance; extraction from markdown code fences; granular CLI flags for
the version facets; backfilling provenance without re-ingest.

## 9. Operational notes

- Deploy order: server first (new request field defaults preserve behavior for old
  clients — note that the *default mode change* means an old client sending a version
  filter gets permissive semantics after deploy; this is the intended cutover, pre-1.0).
- Update any deployed scoring-policy TOML before or with the deploy (the `unsatisfied`
  key now fails startup).
- Re-ingest all sources after deploy to populate extraction (routine; carry-forward
  keeps unchanged embeddings).

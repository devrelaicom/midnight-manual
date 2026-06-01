# Design: Default Midnight ingestion manifests

- **Date:** 2026-06-01
- **Status:** Approved (brainstorming → ready for implementation plan)
- **Author:** Aaron Bassett (with Claude Code)
- **Topic:** A committed, curated set of `hierarchy.yaml` manifests defining the
  default corpus `midnight-manual` ingests for the Midnight ecosystem.

## 1. Goal

Produce a maintainable, non-brittle set of ingest manifests — one per source
repository — under a new `manifests/midnight/` directory. The manifests must:

- describe **what to ingest from each repo at the directory level** (top-level
  directories and glob filters), never by enumerating individual files;
- carry **owner/trust provenance** so the D24 confidence scorer treats Foundation,
  partner, hackathon, and community content appropriately;
- fit the **existing** manifest/ingest machinery (`mn-content::manifest`,
  `mnm manifest check`, `mnm ingest plan|run`) without schema changes.

Audience for the indexed corpus: Midnight **builders** — developers,
entrepreneurs building on Midnight, and technical product/project managers in the
ecosystem. A repo is in-scope when its content helps those people build, decide,
or manage on Midnight.

## 2. Existing system (constraints we build within)

- **Manifest format** (`crates/mn-content/src/manifest/mod.rs`): a `Manifest` is
  `{ manifest_version: 1, root: ManifestNode }`. A `ManifestNode` has `name`,
  `path` (directory pin), `file` (single-file leaf), `published_url`,
  `provenance` (free JSON merged into `mn_core::provenance::Provenance`),
  `include`/`exclude` glob filters (apply when `path:` is set), and `children`.
- **Directory pinning is native**: a node with `path:` + `include`/`exclude`
  ingests a whole subtree. Default include (when no globs) = files whose extension
  maps to a known `DocumentKind` (so binaries/unknown types are skipped). This is
  the mechanism for "top-level directories, not individual files."
- **Provenance inherits top-down** (`manifest/resolve.rs`): `merge_prov` applies
  ancestor provenance to descendants field-by-field; a child overrides only the
  fields it sets. So repo-level provenance is set **once** on `root`.
- **Trust is derived, not declared**: `mn_core::provenance::Provenance` has
  `attribution` (`foundation | partner | third_party | community | unknown`),
  `verified` (+ `verified_by/at`, `verification_notes`), `tags`, `content_type`,
  `language_targets`, `deprecation`. The D24 scoring policy blends `attribution`
  (dominant) with `verified` and `deprecation`. There is **no `trust` field**.
- **Git origin lives on the `Source`, not the manifest**: a `Source` is
  `(slug, kind ∈ docs_site|code_repo|standalone|mixed, origin_url, retention_count)`,
  created via `mnm sources create` / auto-created on `ingest run`. A manifest is
  path+provenance only, resolved against a checkout supplied at ingest time:
  `ingest run` takes `--source-root <dir>` (default: the manifest's parent dir);
  `ingest plan` / `manifest check` take `--base <dir>`. Because our manifests live
  in `manifests/midnight/` (not in the checkout), the checkout dir **must** be
  passed explicitly via `--source-root` / `--base`.
- **Path safety**: every `path:`/`file:` must be relative, no `..`, no scheme.

## 3. Decisions

1. **Layout — per-repo manifests, flat, no machine registry.**
   `manifests/midnight/<slug>.yaml`, one file per repo. Git URL/branch/slug are
   tracked outside the manifests (via `mnm sources create`). A human-readable
   `manifests/midnight/README.md` provides the slug↔repo↔branch↔kind↔owner↔trust
   index and the per-repo ingest recipe — documentation, not a parsed registry.

2. **Slug convention.** Foundation repos use the bare repo name
   (`midnight-ledger`). Third-party repos are owner-prefixed for uniqueness and
   clarity (`openzeppelin-compact-contracts`, `midnames-core`,
   `adavault-midnight-skill`). Slugs are reused verbatim as `Source.slug`.

3. **Trust mapping — faithful attribution + trust tags.** `attribution` reflects
   real authorship; `verified: true` only for Foundation; every root carries a
   `tags: [trust:<level>]`; low-trust repos additionally carry
   `verification_notes` stating the reason. See §5.

4. **Granularity — hybrid, scaled per repo.** Simple repos use one root
   `path: .` + the baseline exclude. Partial/large repos list `children:` with
   `path: <top-level-dir>/`. No `file:` leaves anywhere. See §6 and §7.

5. **README companion included** (confirmed). Tutorials rounded to `trust:medium`
   (confirmed). `olanetsoft-midnight-mcp` **excluded** from the set (confirmed).

6. **Out of scope (YAGNI).** No per-repo `language_targets`/Compact version
   constraints (compiler pins vary 0.16→0.30; a wrong constraint mis-scores —
   leave to frontmatter/inference). No ingestion of Aiken `.ak` / Haskell `.hs`
   source (no chunker yet — index their spec/`.md` only). No machine-readable
   source registry. No changes to the manifest schema or scorer.

## 4. The ingestion set (44 repos)

Derived from: all public, non-archived `midnightntwrk` repos updated in 2026, plus
the `midnight-awesome-dapps` Official Partners (🔹), Hackathon Winners (🏆),
Community Tutorials (English), and any other listed repo with ≥10★ + active + high
dev-alignment + English (low trust). Excluded after review: generic upstream forks,
empty stubs/templates, generic CI, `midnight-zk` (below builder abstraction), and
`olanetsoft-midnight-mcp` (per owner decision).

Counts: **32 Foundation + 8 Partner + 1 Hackathon Winner + 3 Other 3rd-party**.
Default branch is `main` for every repo in the set (verify at `sources create`).

## 5. Provenance → owner/trust mapping

Set on each manifest's `root` node (inherited by all descendants):

| Owner type | `attribution` | `verified` | `tags` | `verification_notes` |
|---|---|---|---|---|
| Foundation | `foundation` | `true` (`verified_by: midnight-foundation`) | `trust:high` | — |
| Partner — OpenZeppelin | `partner` | `false` | `trust:high` | — |
| Partner — bricktowers / eddalabs / midnames | `partner` | `false` | `trust:medium` | — |
| Hackathon Winner — kyc-midnight | `third_party` | `false` | `trust:medium` | "LATAM Hack winner; unaffiliated with Foundation; stale since 2025-08." |
| Other 3rd party — community tutorials | `community` | `false` | `trust:medium` | — |
| Other 3rd party — adavault-midnight-skill | `community` | `false` | `trust:low` | "Individual maintainer (ADAvault); unaudited; some 'gotchas' sourced from Discord/anecdote; examples need professional audit before mainnet." |

`content_type` is set **per directory only where unambiguous**: example/contract
repos' contract dirs → `contract_source`; `spec/` dirs → `reference`; tutorial
content → `tutorial`; otherwise omitted (parser/scorer infers).

## 6. Baseline `exclude` (every repo)

Applied on the root `path: .` node (simple repos) or replicated on `path:`
children where relevant:

```
**/node_modules/**, **/dist/**, **/build/**, **/target/**, **/managed/**,
**/.git/**, **/.github/**, **/.next/**, **/out/**, **/coverage/**,
**/__snapshots__/**, **/.turbo/**,
**/CODE_OF_CONDUCT.md, **/CONTRIBUTING.md, **/SECURITY.md
```

Lockfiles and binaries need no exclude — the recognized-`DocumentKind` default
already skips them. Fresh depth=1 clones generally lack `node_modules/`, `dist/`,
`target/`, `managed/` (gitignored); the excludes are belt-and-suspenders.

## 7. Per-repo manifest specification

Shape column: **simple** = `root` `path: .` + baseline exclude; **partial** =
`root` (provenance only) + listed `children`. Suggested `kind` is for
`sources create`; it does not appear in the manifest.

Mechanics for the partial shapes: "**+ root `*.md`**" means the `root` node *also*
sets `path: .` with `include: ["*.md"]` (top-level markdown only — non-recursive
glob) in addition to its `children`; the resolver walks a node's own `path:` and
then recurses its `children`, so both are ingested. "**child `path: X/` include
`*.md`**" means a single child node pinned to that subtree, markdown only.

### Foundation — Core protocol & specs
| Slug | Repo | Kind | Shape |
|---|---|---|---|
| midnight-ledger | midnightntwrk/midnight-ledger | mixed | partial: `spec/`, `docs/`, `integration-tests/` + root `*.md` |
| midnight-node | midnightntwrk/midnight-node | mixed | partial: `docs/`, `util/toolkit/`, `util/toolkit-js/`, `local-environment/` + root `*.md` |
| midnight-indexer | midnightntwrk/midnight-indexer | mixed | partial: `indexer-api/`, `docs/`, `indexer-tests/` + root `*.md` |

### Foundation — SDK & DApp framework
| Slug | Repo | Kind | Shape |
|---|---|---|---|
| midnight-js | midnightntwrk/midnight-js | code_repo | simple |
| midnight-wallet | midnightntwrk/midnight-wallet | code_repo | simple |
| midnight-sdk | midnightntwrk/midnight-sdk | code_repo | simple |
| midnight-dapp-connector-api | midnightntwrk/midnight-dapp-connector-api | code_repo | simple |
| midnight-local-dev | midnightntwrk/midnight-local-dev | code_repo | simple |

### Foundation — Docs, standards & discovery
| Slug | Repo | Kind | Shape |
|---|---|---|---|
| midnight-docs | midnightntwrk/midnight-docs | docs_site | partial: `docs/`, `blog/`, `academy/`, `api-reference/`, `sdks/` |
| midnight-improvement-proposals | midnightntwrk/midnight-improvement-proposals | docs_site | simple |
| midnight-architecture | midnightntwrk/midnight-architecture | docs_site | partial: `adrs/`, `overview/`, `specification/`, `consensus/`, `components/`, `user-flows/`, `proposals/`, `languages/`, `product/`, `glacier-drop/` + root `*.md` |
| midnight-awesome-dapps | midnightntwrk/midnight-awesome-dapps | docs_site | simple |

### Foundation — Example DApps & contracts
| Slug | Repo | Kind | Shape |
|---|---|---|---|
| example-counter | midnightntwrk/example-counter | mixed | simple |
| example-bboard | midnightntwrk/example-bboard | mixed | simple |
| example-battleship | midnightntwrk/example-battleship | mixed | simple |
| example-hello-world | midnightntwrk/example-hello-world | mixed | partial: `path: .` include `*.md` (contract is inline in README) |
| example-zkloan | midnightntwrk/example-zkloan | mixed | simple |
| example-kitties | midnightntwrk/example-kitties | mixed | simple |
| example-private-party | midnightntwrk/example-private-party | mixed | simple |
| example-nft-contracts | midnightntwrk/example-nft-contracts | mixed | simple |
| midnight-wallet-dapp | midnightntwrk/midnight-wallet-dapp | mixed | simple |
| midnight-leaderboard | midnightntwrk/midnight-leaderboard | mixed | simple |
| midnight-tip-jar | midnightntwrk/midnight-tip-jar | mixed | simple |
| midnight-dust-generator | midnightntwrk/midnight-dust-generator | mixed | simple |

### Foundation — Tooling, CLI & ops
| Slug | Repo | Kind | Shape |
|---|---|---|---|
| compact | midnightntwrk/compact | docs_site | partial: child `path: prerelease/` include `*.md` |
| create-mn-app | midnightntwrk/create-mn-app | mixed | simple |
| setup-compact-action | midnightntwrk/setup-compact-action | docs_site | simple |
| midnight-node-docker | midnightntwrk/midnight-node-docker | mixed | simple |
| contributor-hub | midnightntwrk/contributor-hub | docs_site | simple |
| servicedesk | midnightntwrk/servicedesk | docs_site | partial-via-exclude: `path: .` + extra excludes `_data/`, `_layouts/`, `_config.yml`, `index.html`, `APPLY.md` |

### Foundation — Tokenomics & governance contracts (Cardano-side)
| Slug | Repo | Kind | Shape |
|---|---|---|---|
| midnight-reserve-contracts | midnightntwrk/midnight-reserve-contracts | mixed | partial: `spec/` + root `*.md` (Aiken `.ak` skipped — no chunker) |
| night-token-distribution | midnightntwrk/night-token-distribution | mixed | partial: child `path: protocol-params/` include `*.md` + root `*.md` (Haskell/Aiken skipped) |

### Partner (🔹)
| Slug | Repo | Kind | Trust | Shape |
|---|---|---|---|---|
| openzeppelin-compact-contracts | OpenZeppelin/compact-contracts | mixed | high | simple |
| openzeppelin-compact-tools | OpenZeppelin/compact-tools | code_repo | high | simple |
| openzeppelin-midnight-apps | OpenZeppelin/midnight-apps | mixed | high | simple |
| eddalabs-midnight-starter-template | eddalabs/midnight-starter-template | mixed | medium | simple |
| bricktowers-midnight-rwa | bricktowers/midnight-rwa | mixed | medium | simple |
| bricktowers-midnight-identity | bricktowers/midnight-identity | mixed | medium | simple |
| bricktowers-midnight-seabattle | bricktowers/midnight-seabattle | mixed | medium | simple |
| midnames-core | midnames/core | mixed | medium | simple |

### Hackathon Winner (🏆)
| Slug | Repo | Kind | Trust | Shape |
|---|---|---|---|---|
| joacolinares-kyc-midnight | joacolinares/kyc-midnight | mixed | medium | simple |

### Other 3rd party
| Slug | Repo | Kind | Trust | Shape |
|---|---|---|---|---|
| olanetsoft-learn-compact | Olanetsoft/learn-compact | docs_site | medium | partial: `book/src/`, `exercises/`, `examples/` (a few "Coming Soon" stubs ingested — harmless) |
| olanetsoft-compact-by-example | Olanetsoft/compact-by-example | docs_site | medium | simple |
| adavault-midnight-skill | ADAvault/midnight-skill | docs_site | low | partial: `reference/`, `examples/` + root `SKILL.md` |

## 8. README companion (`manifests/midnight/README.md`)

A human index, not parsed by the loader. Contains:
- The purpose of the directory and the manifest conventions (this design in brief).
- A table: slug | repo URL | branch | suggested `kind` | owner | `attribution` |
  `verified` | trust tag.
- The per-repo recipe, e.g.:
  ```bash
  git clone --depth=1 -b main https://github.com/midnightntwrk/midnight-docs /tmp/clones/midnight-docs
  mnm sources create --slug midnight-docs --kind docs_site \
      --origin-url https://github.com/midnightntwrk/midnight-docs
  mnm ingest run manifests/midnight/midnight-docs.yaml \
      --source-slug midnight-docs \
      --source-root /tmp/clones/midnight-docs
  ```
  (`--kind` ∈ `docs_site | code_repo | standalone | mixed`. `ingest run` also
  auto-creates the source on 404 when `--yes` is passed, but explicit
  `sources create` is preferred so `kind`/`origin_url` are set correctly.)

## 9. Validation

For each generated manifest:
1. `mnm manifest check manifests/midnight/<slug>.yaml --base <fresh-clone>` —
   schema version, path-safety, and file-existence against a depth=1 clone (most
   clones already exist under `/tmp/review*`).
2. A repo-wide assertion that **no manifest contains a `file:` key** (grep), to
   enforce the directory-level rule.
3. A schema parse of every file via `Manifest::parse` (can be a small test or a
   `manifest check` loop in CI).

## 10. Acceptance criteria

- `manifests/midnight/` contains exactly **44** `<slug>.yaml` files matching the
  slug convention in §7, plus `README.md`. `olanetsoft-midnight-mcp.yaml` is
  **absent**.
- Every file parses (`manifest_version: 1`) and passes `mnm manifest check`
  against its fresh clone (no missing paths, no unsafe paths).
- No file contains a `file:` leaf (directory-level only).
- Each `root.provenance` matches the §5 mapping for its owner type
  (`attribution`, `verified`, `tags`, and `verification_notes` where required).
- The 12 partial repos in §7 follow exactly their specified shape (the listed
  `children` dirs / `include` globs / extra excludes); all other repos are
  `path: .` + baseline exclude.
- `README.md` carries the index table and a runnable ingest recipe.

## 11. Future work (not in this change)

- Aiken/Haskell chunkers → then add `.ak`/`.hs` source to reserve-contracts and
  night-token-distribution manifests.
- A `mnm sources apply <dir>` / machine-readable registry if the set grows enough
  to warrant automated bulk source creation.
- Per-content `language_targets` once a reliable per-repo Compact version signal
  exists.
- Re-evaluate `midnight-zk` if protocol/cryptography-depth coverage is wanted.

## 12. Addendum — operational mapping + CI smoke test (2026-06-01)

After the manifests landed, two operational needs surfaced that §3 deferred:

- **`manifests/midnight/sources.tsv`** — a machine-readable mapping
  (`slug  repo(owner/name)  branch  kind`, one row per manifest). §3 chose "no
  machine registry"; this file is the reconciliation: the manifests stay
  path+provenance only, while the repo/branch/kind that `mnm sources create`
  needs live in one parseable place. Consumed by the smoke workflow below and
  intended for a future bulk-ingest driver. The human `README.md` index remains;
  the smoke test asserts `sources.tsv` and the `*.yaml` set stay in 1:1 sync.

- **`.github/workflows/manifest-smoke.yml`** — a scheduled daily smoke test
  (`0 4 * * *` UTC, matching `embedder-smoke.yml`; also `workflow_dispatch`).
  Two jobs: (1) `repo-reachability` — every repo in `sources.tsv` is checked via
  `gh api repos/<owner>/<repo>` for existence (no 404) and `archived: false`,
  with retry/backoff so transient blips don't flake the run red; (2)
  `manifest-regression` — builds `mnm` from source and validates a random sample
  of manifests against fresh shallow clones (`manifest check` + `ingest plan`,
  asserting > 0 files walked) to catch upstream directory-structure drift. A
  failure just turns the run red (no issue is opened).

# Midnight Ingestion Manifests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Author the committed default Midnight ingestion set — 44 curated, directory-level `hierarchy.yaml` manifests + a README — under `manifests/midnight/`, validated against the existing `mn-content` manifest pipeline.

**Architecture:** Each repo gets one path+provenance manifest (no `file:` leaves). Repo-level owner/trust provenance is set once on `root` (it inherits down). 32 "simple" repos use a single `root: { path: ., exclude: <baseline>, provenance }`. 12 "partial" repos use `root` + `children:` pinning specific top-level directories. Git URL/branch/slug live outside the manifests (a human `README.md` indexes them; `mnm sources create` registers them).

**Tech Stack:** YAML; `mn-content::manifest` schema (`manifest_version: 1`); the `mnm`/`midnight-manual` CLI (`cargo run -p mn-cli -- …`) for `manifest check` / `ingest plan`; `git` for shallow clones during validation.

**Spec:** `docs/superpowers/specs/2026-06-01-midnight-ingestion-manifests-design.md`.

**Note on TDD adaptation:** the artifacts are static data files, not code. "Test-first" is realized as a reusable validation harness written in Task 1 (asserts count, no `file:` leaves, schema/path validity, and presence of `attribution` + `trust:` tag on every manifest) that fails before the manifests exist and goes green as they're authored. Partial-repo tasks additionally run `ingest plan` against a fresh clone to prove the pinned directories resolve to real files.

---

## Shared building blocks (referenced by every task)

### Baseline `exclude` (verbatim — used on every "simple" root, and on partial roots that themselves set `path: .`)

```yaml
  exclude:
    - "**/node_modules/**"
    - "**/dist/**"
    - "**/build/**"
    - "**/target/**"
    - "**/managed/**"
    - "**/.git/**"
    - "**/.github/**"
    - "**/.next/**"
    - "**/out/**"
    - "**/coverage/**"
    - "**/__snapshots__/**"
    - "**/.turbo/**"
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
```

For a partial root that sets `path: .` with `include: ["*.md"]` (top-level markdown only), the directory globs above are irrelevant; use only the boilerplate-markdown subset:

```yaml
  exclude:
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
```

### Provenance blocks (verbatim — pick one per repo per the spec §5 mapping)

`F` — Foundation:
```yaml
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
```

`PH` — Partner, high trust (OpenZeppelin):
```yaml
  provenance:
    attribution: partner
    verified: false
    tags: [trust:high]
```

`PM` — Partner, medium trust (bricktowers / eddalabs / midnames):
```yaml
  provenance:
    attribution: partner
    verified: false
    tags: [trust:medium]
```

`HK` — Hackathon winner (kyc-midnight):
```yaml
  provenance:
    attribution: third_party
    verified: false
    tags: [trust:medium]
    verification_notes: >-
      LATAM Hack winner; unaffiliated with the Midnight Foundation; repo stale
      since 2025-08.
```

`CM` — Community, medium trust (Olanetsoft tutorials):
```yaml
  provenance:
    attribution: community
    verified: false
    tags: [trust:medium]
```

`CL` — Community, low trust (adavault-midnight-skill):
```yaml
  provenance:
    attribution: community
    verified: false
    tags: [trust:low]
    verification_notes: >-
      Individual maintainer (ADAvault); unaudited; some "gotchas" sourced from
      Discord/anecdote; examples need professional audit before mainnet use.
```

### Simple-repo template (verbatim — substitute `<NAME>` and `<PROVENANCE>`)

```yaml
manifest_version: 1
root:
  name: <NAME>
  path: .
  exclude:
    - "**/node_modules/**"
    - "**/dist/**"
    - "**/build/**"
    - "**/target/**"
    - "**/managed/**"
    - "**/.git/**"
    - "**/.github/**"
    - "**/.next/**"
    - "**/out/**"
    - "**/coverage/**"
    - "**/__snapshots__/**"
    - "**/.turbo/**"
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
  <PROVENANCE>
```
(`<PROVENANCE>` = the chosen block above, already indented two spaces to sit under `root:`.)

### Validation commands

- Schema/path + harness: `bash scripts/validate-midnight-manifests.sh` (created in Task 1).
- Single file: `cargo run -q -p mn-cli -- manifest check manifests/midnight/<slug>.yaml`
- Pinned-dir resolution (partial repos): clone + plan:
  ```bash
  git clone --depth=1 https://github.com/<owner>/<repo> /tmp/mn-clones/<slug>
  cargo run -q -p mn-cli -- ingest plan manifests/midnight/<slug>.yaml \
      --source-slug <slug> --base /tmp/mn-clones/<slug>
  # Expect: "walked source  N files" with N > 0 and no error.
  ```

---

## Task 1: Scaffold + validation harness + canary manifest

**Files:**
- Create: `manifests/midnight/` (directory)
- Create: `scripts/validate-midnight-manifests.sh`
- Create: `manifests/midnight/midnight-js.yaml`

- [ ] **Step 1: Create the directory and the validation harness**

Create `scripts/validate-midnight-manifests.sh`:
```bash
#!/usr/bin/env bash
# Validates the manifests/midnight/ ingestion set.
set -uo pipefail
dir="manifests/midnight"
expected=44
fail=0

mapfile -t files < <(find "$dir" -maxdepth 1 -name '*.yaml' | sort)
count=${#files[@]}

# 1. Directory-level rule: no `file:` leaves anywhere.
if grep -rnE '^[[:space:]]*file:[[:space:]]' "$dir"/*.yaml 2>/dev/null; then
  echo "FAIL: a 'file:' leaf was found — manifests must be directory-level"; fail=1
fi

# 2. Each file: schema + path-safety via the real loader, plus required provenance.
for f in "${files[@]}"; do
  if ! cargo run -q -p mn-cli -- manifest check "$f" >/dev/null 2>&1; then
    echo "FAIL: 'manifest check' rejected $f"; fail=1
  fi
  grep -q 'attribution:' "$f" || { echo "FAIL: no attribution in $f"; fail=1; }
  grep -q 'trust:'       "$f" || { echo "FAIL: no trust tag in $f"; fail=1; }
done

# 3. Exactly the expected number of manifests, and no excluded repo present.
[ "$count" -eq "$expected" ] || { echo "FAIL: expected $expected manifests, found $count"; fail=1; }
[ -e "$dir/olanetsoft-midnight-mcp.yaml" ] && { echo "FAIL: excluded repo present"; fail=1; }

echo "checked $count manifest(s); fail=$fail"
exit "$fail"
```

Then:
```bash
mkdir -p manifests/midnight
chmod +x scripts/validate-midnight-manifests.sh
```

- [ ] **Step 2: Run the harness to verify it FAILS (nothing authored yet)**

Run: `bash scripts/validate-midnight-manifests.sh`
Expected: `FAIL: expected 44 manifests, found 0` and exit code 1.

- [ ] **Step 3: Author the canary manifest** `manifests/midnight/midnight-js.yaml` (simple template, name `midnight-js`, provenance `F`):

```yaml
manifest_version: 1
root:
  name: midnight-js
  path: .
  exclude:
    - "**/node_modules/**"
    - "**/dist/**"
    - "**/build/**"
    - "**/target/**"
    - "**/managed/**"
    - "**/.git/**"
    - "**/.github/**"
    - "**/.next/**"
    - "**/out/**"
    - "**/coverage/**"
    - "**/__snapshots__/**"
    - "**/.turbo/**"
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
```

- [ ] **Step 4: Prove the canary parses, checks, and resolves to real files**

```bash
cargo run -q -p mn-cli -- manifest check manifests/midnight/midnight-js.yaml
git clone --depth=1 https://github.com/midnightntwrk/midnight-js /tmp/mn-clones/midnight-js
cargo run -q -p mn-cli -- ingest plan manifests/midnight/midnight-js.yaml \
    --source-slug midnight-js --base /tmp/mn-clones/midnight-js
```
Expected: `manifest check` reports OK; `ingest plan` walks N files with N > 0 (hundreds of `.ts`/`.md`), no error.

- [ ] **Step 5: Commit**

```bash
git add scripts/validate-midnight-manifests.sh manifests/midnight/midnight-js.yaml
git commit -m "feat(manifests): scaffold midnight ingestion set + validation harness + canary"
```

---

## Task 2: Foundation — SDK & docs simple manifests

**Files (all simple template, provenance `F`):**
- Create: `manifests/midnight/midnight-wallet.yaml` (name `midnight-wallet`)
- Create: `manifests/midnight/midnight-sdk.yaml` (name `midnight-sdk`)
- Create: `manifests/midnight/midnight-dapp-connector-api.yaml` (name `midnight-dapp-connector-api`)
- Create: `manifests/midnight/midnight-local-dev.yaml` (name `midnight-local-dev`)
- Create: `manifests/midnight/midnight-improvement-proposals.yaml` (name `midnight-improvement-proposals`)
- Create: `manifests/midnight/midnight-awesome-dapps.yaml` (name `midnight-awesome-dapps`)

- [ ] **Step 1: Author all six files** using the simple-repo template verbatim, substituting each `<NAME>` above and provenance block `F`. (Identical body to the canary except `root.name`.)

- [ ] **Step 2: Validate schema/paths**

Run: `for s in midnight-wallet midnight-sdk midnight-dapp-connector-api midnight-local-dev midnight-improvement-proposals midnight-awesome-dapps; do cargo run -q -p mn-cli -- manifest check manifests/midnight/$s.yaml || echo "BAD $s"; done`
Expected: OK for all six; no `BAD` lines.

- [ ] **Step 3: Commit**

```bash
git add manifests/midnight/
git commit -m "feat(manifests): Foundation SDK + docs simple manifests"
```

---

## Task 3: Foundation — example DApps simple manifests

**Files (all simple template, provenance `F`; `<NAME>` = slug):**
- `example-counter.yaml`, `example-bboard.yaml`, `example-battleship.yaml`, `example-zkloan.yaml`, `example-kitties.yaml`, `example-private-party.yaml`, `example-nft-contracts.yaml`, `midnight-wallet-dapp.yaml`, `midnight-leaderboard.yaml`, `midnight-tip-jar.yaml`, `midnight-dust-generator.yaml` (all under `manifests/midnight/`)

- [ ] **Step 1: Author all 11 files** using the simple-repo template, provenance `F`, `root.name` = the slug.

- [ ] **Step 2: Validate schema/paths**

Run: `for s in example-counter example-bboard example-battleship example-zkloan example-kitties example-private-party example-nft-contracts midnight-wallet-dapp midnight-leaderboard midnight-tip-jar midnight-dust-generator; do cargo run -q -p mn-cli -- manifest check manifests/midnight/$s.yaml || echo "BAD $s"; done`
Expected: OK for all; no `BAD` lines.

- [ ] **Step 3: Commit**

```bash
git add manifests/midnight/
git commit -m "feat(manifests): Foundation example DApp manifests"
```

---

## Task 4: Foundation — tooling/CLI/ops simple manifests

**Files (simple template, provenance `F`; `<NAME>` = slug):**
- `create-mn-app.yaml`, `setup-compact-action.yaml`, `midnight-node-docker.yaml`, `contributor-hub.yaml`

- [ ] **Step 1: Author all four files** using the simple-repo template, provenance `F`.

- [ ] **Step 2: Validate schema/paths**

Run: `for s in create-mn-app setup-compact-action midnight-node-docker contributor-hub; do cargo run -q -p mn-cli -- manifest check manifests/midnight/$s.yaml || echo "BAD $s"; done`
Expected: OK for all four.

- [ ] **Step 3: Commit**

```bash
git add manifests/midnight/
git commit -m "feat(manifests): Foundation tooling/ops manifests"
```

---

## Task 5: Foundation — core protocol partial manifests

**Files:**
- Create: `manifests/midnight/midnight-ledger.yaml`
- Create: `manifests/midnight/midnight-node.yaml`
- Create: `manifests/midnight/midnight-indexer.yaml`

- [ ] **Step 1: Author `midnight-ledger.yaml`**

```yaml
manifest_version: 1
root:
  name: midnight-ledger
  path: .
  include: ["*.md"]
  exclude:
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
  children:
    - name: spec
      path: spec/
      provenance: { content_type: reference }
    - name: docs
      path: docs/
    - name: integration-tests
      path: integration-tests/
```

- [ ] **Step 2: Author `midnight-node.yaml`**

```yaml
manifest_version: 1
root:
  name: midnight-node
  path: .
  include: ["*.md"]
  exclude:
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
  children:
    - name: docs
      path: docs/
    - name: toolkit
      path: util/toolkit/
    - name: toolkit-js
      path: util/toolkit-js/
    - name: local-environment
      path: local-environment/
```

- [ ] **Step 3: Author `midnight-indexer.yaml`**

```yaml
manifest_version: 1
root:
  name: midnight-indexer
  path: .
  include: ["*.md"]
  exclude:
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
  children:
    - name: indexer-api
      path: indexer-api/
    - name: docs
      path: docs/
    - name: indexer-tests
      path: indexer-tests/
```

> Note: `.graphql` may not be a recognized `DocumentKind`; if so the raw schema is skipped, but the prose API docs under `docs/` (which describe the schema) are ingested. Acceptable for v1.

- [ ] **Step 4: Validate each resolves to real files (pinned dirs exist)**

```bash
for s in midnight-ledger midnight-node midnight-indexer; do
  owner=midnightntwrk
  git clone --depth=1 https://github.com/$owner/$s /tmp/mn-clones/$s 2>/dev/null || true
  cargo run -q -p mn-cli -- manifest check manifests/midnight/$s.yaml
  cargo run -q -p mn-cli -- ingest plan manifests/midnight/$s.yaml --source-slug $s --base /tmp/mn-clones/$s
done
```
Expected: each `ingest plan` walks N > 0 files (the pinned dirs exist and contain content). If any walks 0, the pinned directory name is wrong — fix it against the clone's actual top-level layout before continuing.

- [ ] **Step 5: Commit**

```bash
git add manifests/midnight/
git commit -m "feat(manifests): Foundation core protocol partial manifests"
```

---

## Task 6: Foundation — docs/architecture partial manifests

**Files:**
- Create: `manifests/midnight/midnight-docs.yaml`
- Create: `manifests/midnight/midnight-architecture.yaml`

- [ ] **Step 1: Author `midnight-docs.yaml`**

```yaml
manifest_version: 1
root:
  name: midnight-docs
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
  children:
    - name: docs
      path: docs/
    - name: blog
      path: blog/
    - name: academy
      path: academy/
    - name: api-reference
      path: api-reference/
    - name: sdks
      path: sdks/
```

- [ ] **Step 2: Author `midnight-architecture.yaml`**

```yaml
manifest_version: 1
root:
  name: midnight-architecture
  path: .
  include: ["*.md"]
  exclude:
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
  children:
    - name: adrs
      path: adrs/
    - name: overview
      path: overview/
    - name: specification
      path: specification/
    - name: consensus
      path: consensus/
    - name: components
      path: components/
    - name: user-flows
      path: user-flows/
    - name: proposals
      path: proposals/
    - name: languages
      path: languages/
    - name: product
      path: product/
    - name: glacier-drop
      path: glacier-drop/
```

- [ ] **Step 3: Validate pinned dirs resolve**

```bash
for s in midnight-docs midnight-architecture; do
  git clone --depth=1 https://github.com/midnightntwrk/$s /tmp/mn-clones/$s 2>/dev/null || true
  cargo run -q -p mn-cli -- manifest check manifests/midnight/$s.yaml
  cargo run -q -p mn-cli -- ingest plan manifests/midnight/$s.yaml --source-slug $s --base /tmp/mn-clones/$s
done
```
Expected: both walk N > 0 files. For any child dir that walks 0 (a renamed/absent dir), correct it against the clone's actual top-level directory names, then re-run.

- [ ] **Step 4: Commit**

```bash
git add manifests/midnight/
git commit -m "feat(manifests): Foundation docs + architecture partial manifests"
```

---

## Task 7: Foundation — misc partial manifests (md-scoped + sub-tree pins)

**Files:**
- Create: `manifests/midnight/example-hello-world.yaml`
- Create: `manifests/midnight/compact.yaml`
- Create: `manifests/midnight/servicedesk.yaml`
- Create: `manifests/midnight/midnight-reserve-contracts.yaml`
- Create: `manifests/midnight/night-token-distribution.yaml`

- [ ] **Step 1: Author `example-hello-world.yaml`** (the contract is inline in the README; index markdown only)

```yaml
manifest_version: 1
root:
  name: example-hello-world
  path: .
  include: ["**/*.md"]
  exclude:
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
```

- [ ] **Step 2: Author `compact.yaml`** (only the prerelease install guide is useful; source lives at LFDT-Minokawa/compact)

```yaml
manifest_version: 1
root:
  name: compact
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
  children:
    - name: prerelease
      path: prerelease/
      include: ["**/*.md"]
```

- [ ] **Step 3: Author `servicedesk.yaml`** (drop the Jekyll site; keep operator docs)

```yaml
manifest_version: 1
root:
  name: servicedesk
  path: .
  exclude:
    - "_data/**"
    - "_layouts/**"
    - "_config.yml"
    - "index.html"
    - "APPLY.md"
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
```

- [ ] **Step 4: Author `midnight-reserve-contracts.yaml`** (Aiken `.ak` skipped — no chunker; index specs/docs)

```yaml
manifest_version: 1
root:
  name: midnight-reserve-contracts
  path: .
  include: ["*.md"]
  exclude:
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
  children:
    - name: spec
      path: spec/
      include: ["**/*.md"]
      provenance: { content_type: reference }
```

- [ ] **Step 5: Author `night-token-distribution.yaml`** (Haskell/Aiken skipped; index README + protocol-params spec)

```yaml
manifest_version: 1
root:
  name: night-token-distribution
  path: .
  include: ["*.md"]
  exclude:
    - "**/CODE_OF_CONDUCT.md"
    - "**/CONTRIBUTING.md"
    - "**/SECURITY.md"
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
    tags: [trust:high]
  children:
    - name: protocol-params
      path: protocol-params/
      include: ["**/*.md"]
```

- [ ] **Step 6: Validate**

```bash
for s in example-hello-world compact servicedesk midnight-reserve-contracts night-token-distribution; do
  git clone --depth=1 https://github.com/midnightntwrk/$s /tmp/mn-clones/$s 2>/dev/null || true
  cargo run -q -p mn-cli -- manifest check manifests/midnight/$s.yaml
  cargo run -q -p mn-cli -- ingest plan manifests/midnight/$s.yaml --source-slug $s --base /tmp/mn-clones/$s
done
```
Expected: each walks N > 0 files. (`compact` should walk the single `prerelease/README.md`; `example-hello-world` the README.)

- [ ] **Step 7: Commit**

```bash
git add manifests/midnight/
git commit -m "feat(manifests): Foundation misc partial manifests (compact, servicedesk, reserve, night-token, hello-world)"
```

---

## Task 8: Partner + Hackathon + community-tutorial simple manifests

**Files (simple template; `<NAME>` = slug; provenance per the id in parens):**
- `openzeppelin-compact-contracts.yaml` (PH)
- `openzeppelin-compact-tools.yaml` (PH)
- `openzeppelin-midnight-apps.yaml` (PH)
- `eddalabs-midnight-starter-template.yaml` (PM)
- `bricktowers-midnight-rwa.yaml` (PM)
- `bricktowers-midnight-identity.yaml` (PM)
- `bricktowers-midnight-seabattle.yaml` (PM)
- `midnames-core.yaml` (PM)
- `joacolinares-kyc-midnight.yaml` (HK)
- `olanetsoft-compact-by-example.yaml` (CM)

- [ ] **Step 1: Author all 10 files** using the simple-repo template, `root.name` = slug, substituting the provenance block named in parens (PH / PM / HK / CM from "Shared building blocks").

- [ ] **Step 2: Validate schema/paths**

Run: `for s in openzeppelin-compact-contracts openzeppelin-compact-tools openzeppelin-midnight-apps eddalabs-midnight-starter-template bricktowers-midnight-rwa bricktowers-midnight-identity bricktowers-midnight-seabattle midnames-core joacolinares-kyc-midnight olanetsoft-compact-by-example; do cargo run -q -p mn-cli -- manifest check manifests/midnight/$s.yaml || echo "BAD $s"; done`
Expected: OK for all 10.

- [ ] **Step 3: Commit**

```bash
git add manifests/midnight/
git commit -m "feat(manifests): partner, hackathon, and community-tutorial manifests"
```

---

## Task 9: Other 3rd-party partial manifests

**Files:**
- Create: `manifests/midnight/olanetsoft-learn-compact.yaml`
- Create: `manifests/midnight/adavault-midnight-skill.yaml`

- [ ] **Step 1: Author `olanetsoft-learn-compact.yaml`**

```yaml
manifest_version: 1
root:
  name: learn-compact
  provenance:
    attribution: community
    verified: false
    tags: [trust:medium]
  children:
    - name: book
      path: book/src/
      provenance: { content_type: tutorial }
    - name: exercises
      path: exercises/
    - name: examples
      path: examples/
```

- [ ] **Step 2: Author `adavault-midnight-skill.yaml`**

```yaml
manifest_version: 1
root:
  name: midnight-skill
  path: .
  include: ["SKILL.md"]
  provenance:
    attribution: community
    verified: false
    tags: [trust:low]
    verification_notes: >-
      Individual maintainer (ADAvault); unaudited; some "gotchas" sourced from
      Discord/anecdote; examples need professional audit before mainnet use.
  children:
    - name: reference
      path: reference/
      provenance: { content_type: reference }
    - name: examples
      path: examples/
      provenance: { content_type: example }
```

- [ ] **Step 3: Validate pinned dirs resolve**

```bash
git clone --depth=1 https://github.com/Olanetsoft/learn-compact /tmp/mn-clones/olanetsoft-learn-compact 2>/dev/null || true
git clone --depth=1 https://github.com/ADAvault/midnight-skill /tmp/mn-clones/adavault-midnight-skill 2>/dev/null || true
cargo run -q -p mn-cli -- manifest check manifests/midnight/olanetsoft-learn-compact.yaml
cargo run -q -p mn-cli -- ingest plan manifests/midnight/olanetsoft-learn-compact.yaml --source-slug olanetsoft-learn-compact --base /tmp/mn-clones/olanetsoft-learn-compact
cargo run -q -p mn-cli -- manifest check manifests/midnight/adavault-midnight-skill.yaml
cargo run -q -p mn-cli -- ingest plan manifests/midnight/adavault-midnight-skill.yaml --source-slug adavault-midnight-skill --base /tmp/mn-clones/adavault-midnight-skill
```
Expected: both walk N > 0 files. (If `book/src/` walks 0, inspect the clone — the mdBook source dir may be `src/` at root; correct the pin.)

- [ ] **Step 4: Commit**

```bash
git add manifests/midnight/
git commit -m "feat(manifests): other third-party partial manifests (learn-compact, midnight-skill)"
```

---

## Task 10: README companion + full validation pass

**Files:**
- Create: `manifests/midnight/README.md`

- [ ] **Step 1: Author `manifests/midnight/README.md`** — purpose, conventions (brief), the index table, and the per-repo recipe. Table columns: `slug | repo | branch | kind | owner | attribution | verified | trust`. Rows: all 44 repos from the spec §7 (Foundation rows use `verified=true`; all others `verified=false`). Recipe block:

````markdown
# Default Midnight ingestion manifests

Directory-level `hierarchy.yaml` manifests for the default Midnight corpus
(`midnight-manual`). One file per source repo; no individual-file leaves. Owner
and trust live in each manifest's `root.provenance` (see the design spec
`docs/superpowers/specs/2026-06-01-midnight-ingestion-manifests-design.md`).

Git URL / branch / slug are NOT in the manifests — register each source first,
then ingest against a fresh checkout:

```bash
git clone --depth=1 -b main https://github.com/midnightntwrk/midnight-docs /tmp/clones/midnight-docs
mnm sources create --slug midnight-docs --kind docs_site \
    --origin-url https://github.com/midnightntwrk/midnight-docs
mnm ingest run manifests/midnight/midnight-docs.yaml \
    --source-slug midnight-docs --source-root /tmp/clones/midnight-docs
```
`--kind` ∈ `docs_site | code_repo | standalone | mixed`.

## Index

| slug | repo | branch | kind | owner | attribution | verified | trust |
|------|------|--------|------|-------|-------------|----------|-------|
| midnight-ledger | midnightntwrk/midnight-ledger | main | mixed | Foundation | foundation | true | high |
| … one row per repo … |
````

Fill every row from spec §7 (slug, `https://github.com/<owner>/<repo>`, `main`, the suggested kind, owner type, the provenance values). Include all 44; do **not** include `olanetsoft-midnight-mcp`.

- [ ] **Step 2: Run the full validation harness**

Run: `bash scripts/validate-midnight-manifests.sh`
Expected: `checked 44 manifest(s); fail=0` and exit code 0.

- [ ] **Step 3: Spot-check the directory-level invariant and trust distribution**

```bash
grep -rlE '^[[:space:]]*file:[[:space:]]' manifests/midnight/*.yaml && echo "UNEXPECTED file: leaf" || echo "OK: no file: leaves"
grep -rl 'trust:low'  manifests/midnight/*.yaml   # expect exactly: adavault-midnight-skill.yaml
ls manifests/midnight/*.yaml | wc -l               # expect 44
```
Expected: "OK: no file: leaves"; `trust:low` only in `adavault-midnight-skill.yaml`; count 44.

- [ ] **Step 4: Commit**

```bash
git add manifests/midnight/README.md
git commit -m "docs(manifests): README index + recipe for the Midnight ingestion set"
```

---

## Self-Review (completed during planning)

**1. Spec coverage:**
- §3 layout/slug/trust/granularity decisions → Tasks 1–9 author files per those rules; README (Task 10) is the human index. ✓
- §5 provenance mapping → the six provenance blocks (`F/PH/PM/HK/CM/CL`) cover all owner/trust combinations; assigned per-repo in Tasks 2–9. ✓
- §6 baseline exclude → verbatim in the simple template and partial roots. ✓
- §7 per-repo table → 22 Foundation-simple (T2–T4), 10 Foundation-partial (T5–T7), 8 partner + 1 hackathon + 1 community-simple (T8), 2 other-partial (T9). 22+10+8+1+1+2 = 44. ✓
- §9 validation → harness (T1) + per-partial `ingest plan` (T5–T7, T9) + full pass (T10). ✓
- §10 acceptance criteria → enforced by the harness (count=44, no `file:` leaves, provenance present, mcp absent) and the T10 spot-checks. ✓

**2. Placeholder scan:** Simple manifests use a fully-specified template + explicit name/provenance per file (deterministic, not a placeholder). The only `<NAME>`/`<PROVENANCE>` tokens are in the reusable template with an explicit substitution table — every file's content is fully determined. Partial manifests are shown in full. README rows are specified by reference to spec §7 (all values enumerated there). No "TBD"/"handle edge cases"/"similar to Task N". ✓

**3. Type/field consistency:** provenance keys (`attribution`, `verified`, `verified_by`, `tags`, `verification_notes`, `content_type`) match `mn_core::provenance::Provenance`; `attribution` values are snake_case enum variants (`foundation`/`partner`/`third_party`/`community`); manifest keys (`manifest_version`, `root`, `name`, `path`, `include`, `exclude`, `provenance`, `children`) match `ManifestNode`. CLI flags verified against source: `manifest check <f>`, `ingest plan <f> --source-slug --base`, `ingest run <f> --source-slug --source-root`, `sources create --slug --kind --origin-url`. ✓

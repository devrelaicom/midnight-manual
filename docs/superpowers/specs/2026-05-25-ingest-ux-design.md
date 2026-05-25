# Ingest UX rework — design

**Date:** 2026-05-25
**Status:** approved
**Touches:** `mn-cli` (new `manifest` top-level + restructured `ingest`),
`mn-content` (`Manifest` resolver, `Walker` rework, sitemap matcher), `mn-core`
(server-URL default), `mn-telemetry` (`IngestComplete` schema bump),
`docs/README-deploy.md`, `corpus/sample/`.

## Problem

The cloud server is deployed. The next step in the v1 plan is to load a real
corpus, and the current ingest UX makes that prohibitively painful:

- The `mnm ingest <manifest>` command requires the operator to hand-write a
  `hierarchy.yaml` for every source. For a real Midnight-docs repo this is
  thousands of `file:` entries; the only practical authoring path today is a
  custom Python script, which is not an acceptable user experience.
- Half of the manifest schema is decorative. `ManifestNode` declares
  `published_url`, `provenance`, and `path:` directory pinning, and a
  `--strict-manifest` mode is mentioned in source comments. None of the four
  are implemented end-to-end: the `Walker` drops them, the CLI's
  `DocumentUpload` hardcodes `published_url`, `source_url`, `language`,
  `source_modified_at`, and `token_count` to `None`/`0`, and the
  `--strict-manifest` flag does not exist.
- A run produces no progress output. Operators stare at a silent terminal
  while walk + chunk + a single giant `PUT` execute.
- The single-batch upload (`crates/mn-cli/src/commands/ingest.rs:285`) is a
  liability for any real corpus; one network blip or a 413 from a real-sized
  payload aborts the entire run.
- The server URL has to be repeated on every command (`--server <url>`).
- `mnm ingest <slug>` against a non-existent source returns an opaque 404
  from the start-ingest-run endpoint; operators have to know they must run
  `mnm sources create` first.
- Several error paths surface raw HTTP status codes with no remediation hint
  (409 model mismatch, 413 body too large, manifest-missing-files only
  prints the *first* missing file).

Relevant requirements: FR-017 (manifest is source of truth), FR-019
(revision recorded for reproducibility), FR-022 (failed ingests do not block
subsequent attempts), FR-047 (Compact code chunking — touches `language`
field), FR-050 (manifest schema), FR-066 (admin-command visibility, D23),
FR-113 (telemetry batching), D17 (server-URL precedence).

## Scope

**In:**

- A new top-level `mnm manifest {init, generate, check}` namespace (always
  visible) for purely-local manifest authoring and validation.
- A restructured `mnm ingest {plan, run}` namespace (admin-hidden) for
  server-touching work. The current `mnm ingest <manifest>` form is removed.
- A sitemap-aware manifest generator that takes globs and (optionally) a
  sitemap URL or file, emits a populated `hierarchy.yaml`, and produces a
  coverage report.
- Full implementation of the long-stubbed manifest features: `path:`
  directory pinning, `published_url` inheritance with prefix-join,
  `provenance` inheritance with field-merge, and a strict-only walker
  (no silent directory-tree fallback at ingest time).
- A `mn_content::manifest::resolve` flattener that produces `ResolvedLeaf`
  records consumed by the walker and threaded through `PlanBuilder` to
  `PlannedDocument`.
- Fixing every hardcoded `None`/`0` in `DocumentUpload` (`published_url`,
  `source_url`, `source_modified_at`, `language`, `token_count`).
- TTY progress UX (`indicatif` multi-progress); JSONL phase events when
  piped or `--json`.
- Chunked upload (default 50 documents per `PUT`, `--batch-size` override);
  abort-on-failure (resumability deferred — see Out).
- Auto-create source on first `ingest run` (prompted unless `--yes` or
  non-TTY).
- Server URL default changed to
  `https://midnight-manual.midnightntwrk.expert`.
- `--revision` default derived from `git rev-parse --short HEAD` of the
  manifest's parent directory, falling back to `"unknown"`.
- Documentation updates: `docs/README-deploy.md` §10 rewritten;
  `docs/cookbook/ingesting-content.md` added; `corpus/sample/hierarchy.yaml`
  updated to use `path:` as a worked example.
- Improved error messages for the seven most-likely operator-facing
  failures (see §2.6).
- `IngestComplete` telemetry schema bumped with `batch_count` and
  `failed_batch_index`.

**Out (deferred):**

- Resumable upload (re-uploading is acceptable while corpora are small).
- Server-side embed-pipeline batching changes.
- Full tree-sitter code-chunker integration (Markdown chunker fallback
  stays; `path:` discovery sets `language` correctly so this is a
  drop-in upgrade later).
- Auto-fetching a sitemap from `/robots.txt` given a docs-site URL
  (operator passes `--sitemap` explicitly).
- `mnm manifest diff <a> <b>`.
- Synthesizing a sitemap by crawling.
- Multi-source ingest in one command.

**Non-goals:**

- Replacing the manifest with directory-walk inference at ingest time.
  Manifests remain the source of truth at ingest; the generator just makes
  authoring cheap.
- Changing the server's ingest-run HTTP contract beyond the already-idempotent
  `(ingest_run_id, document.path)` semantics and an informational
  `batch_index` / `batch_count` body field.

## Command tree (final)

```
mnm manifest init                                       (always visible)
mnm manifest generate [GLOBS...]                        (always visible)
        [--include G]... [--exclude G]...
        [--base DIR]
        [--sitemap URL|FILE]...
        [--url-base URL]
        [--name NAME]
        [-o FILE] [--force]
        [--strict] [--report FILE]
        [--no-hoist] [--no-pin-dirs] [--pin-threshold N]
        [--dry-run]
mnm manifest check <manifest>                           (always visible)
        [--base DIR]
        [--sitemap URL|FILE]...
        [--strict]
mnm ingest plan <manifest>                              (admin-hidden)
        --source-slug X
        [--revision SHA]
        [--embedding-model NAME@REV]
        [--base DIR]
        [--json]
mnm ingest run <manifest>                               (admin-hidden)
        --source-slug X
        [--revision SHA]
        [--note ...]
        [--embedding-model NAME@REV]
        [--base DIR]
        [--source-base-url URL]
        [--batch-size N]
        [--yes]
        [--json]
```

The current `mnm ingest <manifest>` form is removed. Nothing public depends
on it; the only in-repo reference is `docs/README-deploy.md` §10, updated as
part of this work.

`manifest` lives at the top level (not under `ingest`) because:

1. Its operations are 100% local — no server, no admin token, no
   `auth.toml`. Hiding them under the admin gate would be wrong.
2. Manifests are reusable artifacts whose authoring lifecycle is independent
   of any particular ingest (generate locally, hand off, ingest later).
3. It matches the project's noun-first convention (`sources`, `users`,
   `keys`, `models`).

## §1 — `mnm manifest {init, generate, check}`

### 1.1 `manifest init`

Writes a self-documenting starter manifest. Default output `./hierarchy.yaml`,
`-o`/`--output` overrides, `--force` to allow overwrite. Body:

```yaml
# Generated by `mnm manifest init` on <date>.
# Schema: crates/mn-content/src/manifest.rs (manifest_version = 1).
#
# Each leaf node references one source file via `file:` (relative to the
# manifest's parent dir, or `--base` at generate time). Groups use `name:`
# and `children:` to nest. `published_url:` and `provenance:` on any node
# are inherited by descendants — declare them at the highest applicable
# node to avoid repetition. A node with `path:` auto-discovers every
# supported file under that directory.

manifest_version: 1
root:
  name: My Source
  # published_url: https://example.com/docs/
  children:
    - file: path/to/your/first.md
      # name: Pretty Title
      # published_url: https://example.com/docs/first/
```

### 1.2 `manifest generate`

The workhorse. Pure-local: walks the filesystem, parses frontmatter, optionally
parses sitemaps, builds the tree, hoists shared metadata to the highest
applicable node, writes YAML, prints a coverage summary.

**Inputs:**

- Positional `GLOBS...` and/or `--include G` flags — unioned to form the
  include set. `--exclude G` flags subtract.
- `--base DIR` (default cwd) anchors all glob resolution.
- `--sitemap URL|FILE` (repeatable) supplies URLs to match files against.
  Arguments starting with `http://` or `https://` are fetched (10 s
  timeout, follow redirects, no auth); anything else is treated as a
  filesystem path. Parses `<urlset>` and one level of `<sitemapindex>`
  recursion.
- `--url-base URL` is the fallback when no sitemap is provided or no
  sitemap match is found: each file's `published_url` becomes
  `<url-base>/<file-path-suffix-from-base>/`.

**Algorithm (deterministic, single-pass):**

1. **Collect.** Union of `GLOBS` and `--include`; subtract `--exclude`;
   resolve against `--base`; dedup; lex-sort; reject paths escaping `--base`
   (reuse `Manifest::validate`'s safety guard).
2. **Frontmatter.** For each file, call `mn_content::frontmatter::split`;
   capture `slug`, `title`, provenance fields.
3. **Sitemap.** Fetch and parse each `--sitemap`; build a
   `Vec<sitemap::Url>` deduped by full URL.
4. **Match each file → URL** in this order:
   - **C (slug):** if frontmatter has `slug: foo`, search the sitemap
     for a URL whose final non-empty path segment is `foo`.
   - **A (leaf):** else compare `file.basename_without_ext` to each URL's
     final non-empty path segment.
   - **Tie-break (parent-dir relaxation):** if multiple URLs match by leaf,
     walk up both sides — pick the URL whose path-suffix shares the longest
     tail with the file's path-suffix after one optional leading directory
     (e.g. `docs/`) is stripped from the file side. Ties after that → mark
     unmatched (deterministic, not arbitrary).
5. **Build the tree.** Folder structure under `--base` becomes the
   hierarchy. Group `name:` derives from the directory name, titleized
   (`getting-started` → `Getting Started`). Leaf `name:` derives from
   frontmatter `title:` if present, else the markdown H1, else the
   basename.
6. **Hoist `published_url` (`--hoist`, default on).** Where every leaf in a
   subtree shares a common URL prefix, declare the prefix once on the
   parent node and drop it from the leaves.
7. **Pin directories (`--pin-dirs`, default on; `--pin-threshold N`,
   default 5).** When a directory has ≥ N files that all matched via the
   same rule and have no leaf-level overrides, emit a `path:` node with
   the inherited metadata instead of N explicit `file:` entries.
8. **Emit.** Write YAML with the same header comment as `init`.

**Outputs:**

- The generated manifest at `-o` (default `./hierarchy.yaml`).
- A stdout summary: `generated hierarchy.yaml: 142 files, 138 URLs matched
  (via slug: 73, leaf: 65), 4 unmatched.`
- `--report FILE` (optional): one file path per line with reason
  (`no-slug-match`, `ambiguous-leaf`, `no-sitemap-coverage`).
- `--strict` upgrades any unmatched count > 0 to a non-zero exit.
- `--dry-run` prints the YAML to stdout, writes nothing.

### 1.3 `manifest check`

Pure validation, no server contact. Runs every check (not first-fail) and
reports them all together:

- **Schema:** YAML parses, `manifest_version == 1`.
- **Paths:** every `file:` / `path:` is relative, safe (no `..`, no
  absolute, no scheme prefix), unique; every referenced file exists under
  `--base`.
- **Hierarchy sanity:** every leaf has `file:` (no dangling groups); no
  node has both `file:` and `children:`; group `name:` non-empty.
- **Sitemap coverage** (if `--sitemap` provided): percentage of leaves
  whose `published_url` is present in any sitemap; list mismatches.

Exits 0 on success; 1 on hard error; 1 on warnings when `--strict`.

## §2 — `mnm ingest {plan, run}`

### 2.1 Common args

`--source-slug` is required. `--revision` defaults to `git rev-parse --short
HEAD` against `--base` (or the manifest's parent dir); silently falls back
to `"unknown"` only if the directory is not a git repo. `--embedding-model`
defaults to `bge-base-en-v1.5@1`. `--base` defaults to the manifest's
parent dir.

### 2.2 `ingest plan`

Computes the full plan without starting a server-side ingest. Calls
`GET /v1/sources/:slug/active-version/documents` (admin read) to materialize
`PriorState` so `carried` / `deleted` counts are real. Output:

```
plan for source `midnight-docs` (rev abc1234):
  walked       142 files          (3.2 MB)
  chunked      518 chunks         (avg 6.1 KB, max 32 KB)
  vs active rev 7:
    new          12 documents
    carried     130 documents
    deleted       0 documents
  estimated upload:  ~ 4.1 MB in 11 batches of 50 docs
```

`--json` emits the same data structured. If the source does not exist,
report `(would auto-create source 'X' as kind=docs_site, retention=5)`
rather than failing — the operator gets a complete picture pre-run.

### 2.3 `ingest run` — progress UX

TTY (multi-progress via `indicatif`, rows updated in place):

```
✓ resolved server          https://midnight-manual.midnightntwrk.expert
✓ validated manifest        142 files, 0 errors
✓ walked source             142 files (3.2 MB) in 0.4s
✓ chunked                   518 chunks in 1.1s
✓ source created            midnight-docs (kind=docs_site, retention=5)
✓ started ingest run        id=4f8b…  source_version=8 (building)
⠋ uploading documents       batch 4/11  ▇▇▇▇▇▆▁▁▁▁▁  187/518 chunks  72 KB/s  ETA 8s
  finalizing                pending
```

Non-TTY (stdout piped) or `--json`: one JSONL event per phase
transition:

```jsonl
{"phase":"manifest_validated","files":142}
{"phase":"walked","files":142,"bytes":3358720,"duration_ms":401}
{"phase":"chunked","chunks":518,"duration_ms":1118}
{"phase":"source_created","slug":"midnight-docs","kind":"docs_site"}
{"phase":"run_started","ingest_run_id":"4f8b...","source_version":8}
{"phase":"batch_uploaded","batch":1,"of":11,"docs":50,"chunks":189}
{"phase":"finalized","revision":8,"demoted":7,"added":12,"carried":130}
```

Implementation: thin `mn_cli::progress::Reporter` trait with `Tty` and
`Json` impls, selected at command entry from `cli.json || !stdout.is_terminal()`.
The rest of the run code is unaware of which mode is active.

### 2.4 Chunked upload (default 50 docs/batch)

Replace the single `PUT .../documents` with N sequential `PUT`s of the
same shape. The server endpoint is already idempotent on `(ingest_run_id,
document.path)` so this is a CLI-only change to the wire pattern.

The body gains two informational fields:

```json
{ "batch_index": 4, "batch_count": 11, "documents": [...] }
```

Server logs an info message if it sees batch K arrive without 1..K-1, but
otherwise honors any ordering.

Failure: on any batch error, `POST .../abort` and surface
`upload failed at batch K/N (network|413|...); aborted run <id> — re-run
to retry`. Operator restarts from scratch.

### 2.5 Auto-create source (5B)

Before starting the run, `GET /v1/sources/:slug`. If 404:

- **TTY:** prompt
  `Source 'midnight-docs' doesn't exist on this server. Create it as
  kind=docs_site (retention=5)? [Y/n]`. Yes → `POST /v1/admin/sources`
  with defaults, continue. No → exit 1 (`cancelled; run mnm sources
  create manually if you want different defaults`).
- **`--yes`:** skip prompt, create with defaults.
- **Non-TTY without `--yes`:** exit 1 (`source 'X' does not exist;
  re-run with --yes or create it explicitly with mnm sources create`).

Defaults on auto-create: `kind=docs_site`, `display_name = slug`,
`retention_count = 5`.

### 2.6 Error message rewrites

| Failure | Today | New |
|---|---|---|
| `auth.toml` admin section missing | already good | unchanged |
| Token expired | already good | unchanged |
| Source 404 | opaque `404 from POST .../ingest-runs` | `source 'X' does not exist; pass --yes to auto-create or run mnm sources create` |
| 409 embedding-model mismatch | opaque `409 from POST ...` | `server's active embedding model is bge-base-en-v1.5@2 but --embedding-model is bge-base-en-v1.5@1; pass --embedding-model bge-base-en-v1.5@2 or run mnm models pull && retry` |
| Network timeout mid-batch | `error sending request` | `upload failed at batch K/N (network); aborted run <id> — re-run mnm ingest run to retry` |
| Manifest references missing file(s) | reports only the first | reports the full list |
| 413 body too large | opaque `413 from PUT ...` | `batch K exceeded server payload limit; aborted. Re-run with --batch-size 25 (or lower) — current default is 50 docs/batch` |

## §3 — Manifest pipeline fixes

### 3.1 `path:` directory pinning — implementation

A `ManifestNode` with `path:` set means "discover every supported file
under this directory and treat each as a leaf child of this node."

- **Discovery rule:** walk `<--base>/<path>` recursively; include files
  whose extension matches a known `DocumentKind` (`.md`, `.mdx` →
  Markdown; `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`, `.compact` → Code;
  `.txt` → Plaintext). Skip dotfiles and a small default ignore list
  (`node_modules`, `.git`, `target`, `dist`).
- **Per-node filtering:** optional `include: ["**/*.md"]` and
  `exclude: ["**/draft/**"]` arrays. Default include = all known
  kinds; default exclude = empty.
- **Interaction with explicit `children:`:** both allowed on the same
  node. Explicit `file:` children win; a discovered file whose path
  matches an explicit `file:` is dropped from the auto-set.
- **Inheritance:** discovered files inherit the node's `published_url`
  (as a prefix; see 3.2) and `provenance` (merged with frontmatter;
  see 3.2).

### 3.2 `published_url` and `provenance` inheritance

Resolved top-down by a new `mn_content::manifest::resolve` module that
flattens a parsed `Manifest` into `Vec<ResolvedLeaf>`:

```rust
pub struct ResolvedLeaf {
    pub rel_path: PathBuf,
    pub kind: DocumentKind,
    pub name: Option<String>,
    pub published_url: Option<String>,
    pub source_url: Option<String>,
    pub provenance_override: Provenance,
}
```

- **`published_url`:** if the nearest ancestor declares
  `https://docs.example.com/cookbook/`, a leaf `file: auth.md` with no
  leaf override gets `https://docs.example.com/cookbook/auth/`. The join
  is path-style: trailing slash on the ancestor, file basename (no
  extension) appended, trailing slash kept. A leaf can declare its own
  `published_url:` to override outright (no merging). A leaf can set
  `published_url: null` to remove the inherited value.
- **`provenance`:** structured merge, leaf-wins. Precedence (highest →
  lowest): frontmatter > nearest ancestor `provenance:` > defaults.
  Fields merge field-by-field, not whole-object replace.

The walker consumes `Vec<ResolvedLeaf>` instead of raw `ManifestNode`s.
`PlanBuilder::add_walked_document` gains a fourth argument
`leaf: &ResolvedLeaf` and threads `published_url`, `source_url`, and
`provenance_override` into `PlannedDocument`.

### 3.3 Strict-only walker

The walker's docstring promises "files NOT referenced fall back to
directory-tree inference unless `--strict-manifest` is set." This
spec resolves the ambiguity to strict-only:

- The walker emits only files the manifest references (via `file:` or
  discovered under `path:`). Files outside the manifest's reachable set
  are not ingested, even if they exist under `--base`.
- Rationale: directory-tree fallback would duplicate `manifest generate`
  worse — silently. "I added a new file and it got ingested without
  anyone noticing" is exactly the surprise the manifest exists to prevent.
- `walker.rs:5` comment is corrected.
- `manifest check --strict` does a *warning* pass: reports files under
  `--base` that aren't reachable from the manifest, escalating to a
  non-zero exit only under `--strict`.

### 3.4 CLI uploader wiring bugs

`DocumentUpload` in `crates/mn-cli/src/commands/ingest.rs:449` currently
hardcodes five fields to `None`/`0`. Each gets a real value:

| Field | Source |
|---|---|
| `published_url` | `ResolvedLeaf.published_url` (from §3.2 inheritance) |
| `source_url` | `ResolvedLeaf.source_url` if declared, else derived from a new `--source-base-url URL` flag joined with `rel_path` (e.g. `https://github.com/midnight-network/midnight-docs/blob/<rev>/docs/auth.md`) |
| `source_modified_at` | `fs::metadata(abs).modified()` captured at walk time |
| `language` | derived from extension via a new `mn_content::language` lookup (`.rs` → `rust`, `.ts` → `typescript`, `.md` → `markdown`, `.compact` → `compact`, etc.) |
| `token_count` (doc + chunk) | computed at chunk time using `tokenizers` (already a transitive dep of `fastembed`); replaces the `0` placeholder; powers `mnm ingest plan`'s real upload estimate |

Regression test: ingest a 2-file manifest with `published_url` declared
at the root; assert the resulting `chunk` rows have non-null
`published_url` matching the inheritance-joined value.

### 3.5 Generator-side support

`manifest generate` produces manifests that *use* `path:` and inheritance,
not just flat `file:` lists:

- `--hoist` (default on, `--no-hoist` to disable): when every leaf in a
  one-level subtree shares a common `published_url` prefix, emit the
  prefix once on the parent node and strip it from the leaves.
- `--pin-dirs` (default on, `--no-pin-dirs` to disable; `--pin-threshold
  N`, default 5): when a directory contains ≥ N files all matched by
  the same sitemap rule with no leaf-level overrides, emit a `path:`
  node instead of N `file:` entries.

A 5,000-file ingest should produce a manifest in the low hundreds of
lines.

## §4 — Server URL default + housekeeping

- Compiled-in server URL default in `crates/mn-cli/src/shared.rs:17` and
  the `mn_core::config` default change from `https://manual.midnight.network`
  to `https://midnight-manual.midnightntwrk.expert`. Precedence
  unchanged: `--server` > `MIDNIGHT_MANUAL_SERVER` > `[server].url` >
  default.
- `docs/README-deploy.md` §10 rewritten around the new command tree;
  no `--server` flags in any example.
- `docs/cookbook/ingesting-content.md` added — two walkthroughs: (a) a
  docs repo you own (commit the manifest); (b) a third-party repo
  (generate the manifest locally next to `auth.toml`).
- `corpus/sample/hierarchy.yaml` updated to use `path:` as a worked
  example and to smoke-test the new walker.
- `IngestComplete` telemetry event gains `batch_count` (u32) and
  `failed_batch_index` (`Option<u32>`, None on success). Schema bump
  under the existing FR-113 pattern.

## §5 — Crate layout

Net file layout after the change:

```
crates/mn-content/src/
  manifest.rs              # existing; gains optional include/exclude on ManifestNode
  manifest/
    resolve.rs             # NEW — flatten Manifest → Vec<ResolvedLeaf>
    sitemap.rs             # NEW — fetch + parse sitemaps (XML)
    matcher.rs             # NEW — slug-first / leaf / parent-dir-relaxation
    generate.rs            # NEW — globs + frontmatter + sitemap → Manifest
  ingest/
    walker.rs              # consumes Vec<ResolvedLeaf>, no directory fallback
    plan.rs                # PlannedDocument gains published_url/source_url/source_modified_at/language/token_count fields
  language.rs              # NEW — extension → language lookup

crates/mn-cli/src/
  commands/
    manifest/
      mod.rs               # NEW — `mnm manifest` dispatcher
      init.rs              # NEW
      generate.rs          # NEW — thin veneer over mn_content::manifest::generate
      check.rs             # NEW
    ingest/
      mod.rs               # `mnm ingest` dispatcher (replaces today's ingest.rs)
      plan.rs              # NEW
      run.rs               # restructured from today's ingest.rs
  progress.rs              # NEW — Reporter trait + Tty/Json impls
  shared.rs                # default server URL bumped
  cli.rs                   # add `Manifest(...)` variant; restructure `Ingest`

crates/mn-telemetry/src/events.rs   # IngestComplete schema bump
```

One new third-party dep needed: `sitemap` (or `quick-xml` for a hand-roll —
call at implementation time; both are small). `indicatif` is added to
`mn-cli` for progress UI; `globset` is added to `mn-content` for glob
matching.

## §6 — Out of scope (recap)

- Resumable upload.
- Server-side embed batching changes.
- Real tree-sitter code chunking (Markdown fallback stays; `language`
  field is now populated so this becomes a drop-in upgrade).
- Auto-fetching sitemaps from `/robots.txt`.
- `mnm manifest diff`.
- Synthesizing sitemaps by crawling.
- Multi-source ingest in one command.

## §7 — Open follow-up

`mnm sources create` is currently hidden under the admin gate (D23). With
auto-create-source landing in `ingest run`, the visible affordance for the
non-default case (custom `retention_count`, custom `display_name`) is
removed. Recommendation: leave `sources create` hidden for now — the
auto-create path covers 95% of cases, and the override path is documented
in the new `docs/cookbook/ingesting-content.md` (with
`MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1` as the escape hatch). Revisit if
operators report friction.

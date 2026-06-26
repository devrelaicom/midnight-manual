---
title: Running an ingest
sidebar_label: Running an ingest
description: How to use mnm ingest plan and mnm ingest run to get content into your corpus, with worked examples for docs sites and code repos.
---

# Running an ingest

An ingest needs two things: a **manifest** (what to ingest and how to attribute it) and a **source root** (where the files live). With those in place, `mnm ingest run` handles the rest. It walks the tree, chunks each file, embeds the chunks, uploads them in batches, and finalizes the new version atomically.

Admin commands are hidden by default. Reveal them with:

```bash
export MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1
```

## Embedding and BYOK

Ingestion embeds every chunk through VoyageAI. For bulk runs, set `VOYAGE_API_KEY` to embed directly against your own account (BYOK); otherwise embedding is proxied by the server and counts against its token budget. Large batches can take tens of seconds. Widen the per-request timeout with `--voyage-timeout-secs <N>` (env `VOYAGE_TIMEOUT_SECS`, default 120).

## mnm ingest plan

`ingest plan` walks the source tree and builds the ingest plan locally, without contacting the server for writes. It shows how many files would be chunked, how many are new versus carried from the previous version, and how many would be deleted, all without uploading anything.

```bash
mnm ingest plan hierarchy.yaml --source-slug my-source
```

Key flags:

| Flag | Description |
|---|---|
| `--source-slug <SLUG>` | Required. Slug of the target source. |
| `--revision <REV>` | Free-form revision label (defaults to `git rev-parse --short HEAD`). |
| `--base <DIR>` | Override the source root (default: the manifest's parent directory). |
| `--json` | Emit a single-line JSON object instead of the human summary. |
| `--report-file <PATH>` | Write the structured `IngestReport` JSON here in addition to the stdout summary. |

The plan output reads like:

```
plan for source `my-source` (rev abc1234):
  walked       42 files
  chunked      183 chunks
    new          8 documents
    carried      34 documents
    deleted      0 documents
```

Plan output is conservative: it over-reports new documents for code sources (because it has no code-model flag) and degrades to an all-new classification if it cannot reach the server. That makes it safe to act on; it never under-reports.

## mnm ingest run

`ingest run` executes the full pipeline: walk, chunk, embed, upload in batches, and finalize. If the source slug does not yet exist on the server, the CLI prompts to create it (or auto-creates with `--yes`).

```bash
mnm ingest run hierarchy.yaml \
  --source-slug my-source \
  --source-root ./my-repo
```

Key flags:

| Flag | Description |
|---|---|
| `--source-slug <SLUG>` | Required. Slug of the target source. |
| `--source-root <DIR>` | Override the source root (default: the manifest's parent directory). |
| `--revision <REV>` | Free-form revision label (defaults to `git rev-parse --short HEAD`). |
| `--dry-run` | Walk and build the plan, but do not upload or finalize. |
| `--yes` | Auto-confirm the source-create prompt (non-interactive mode). |
| `--batch-size <N>` | Documents per upload batch (default: 25). |
| `--chunk-tokens <N>` | Token budget per chunk, all document kinds (default: 1024). |
| `--voyage-timeout-secs <N>` | Per-request timeout for BYOK embedding calls (default: 120). |
| `--no-code-embeddings` | Skip `voyage-code-3` vectors for code files in this run. |
| `--json` | Emit the `IngestReport` JSON as the final stdout line. |
| `--report-file <PATH>` | Write the structured `IngestReport` JSON here in addition to the stdout summary. |
| `--note <TEXT>` | Optional note recorded on the `source_version` row. |

### Example A — ingest a docs site (with a manifest)

```bash
git clone https://github.com/midnightntwrk/midnight-docs.git

# hierarchy.yaml:
# manifest_version: 1
# root:
#   name: "Midnight Docs"
#   provenance:
#     attribution: foundation
#     verified_by: foundation
#   children:
#     - name: docs
#       path: docs/

mnm ingest run hierarchy.yaml \
  --source-slug midnight-docs \
  --source-root ./midnight-docs
```

The CLI chunks every file, embeds each chunk through VoyageAI, uploads the chunks in batches, and finalizes the new version, promoting it live atomically once the run completes.

### Example B — ingest a code repo without hand-writing a manifest

For source repos you do not need to author a manifest by hand. Let `mnm manifest generate` walk the tree (honouring `.gitignore`) and build the manifest for you, then ingest it:

```bash
# OpenZeppelin's Compact contracts
git clone https://github.com/OpenZeppelin/compact-contracts.git
mnm manifest generate --base ./compact-contracts \
    --include '**/*.compact' --include '**/*.ts' --include '**/*.md' \
    --output compact-contracts.yaml
mnm ingest run compact-contracts.yaml \
    --source-slug openzeppelin-compact \
    --source-root ./compact-contracts

# A full example dApp — the Midnight "kitties" sample
git clone https://github.com/midnightntwrk/example-kitties.git
mnm manifest generate --base ./example-kitties \
    --include '**/*.compact' --include '**/*.ts' --include '**/*.tsx' --include '**/*.md' \
    --output kitties.yaml
mnm ingest run kitties.yaml \
    --source-slug example-kitties \
    --source-root ./example-kitties
```

The `.compact`, `.ts`, and `.tsx` files are chunked with full symbol awareness, so a search for a specific circuit or contract lands on exactly that definition, attributed back to the OpenZeppelin or Midnight source it came from.

## What the output tells you

A successful run prints something like:

```
finalized revision 3 (demoted revision 2); +8 new, 34 carried
```

`new` is documents added for the first time or whose content changed. `carried` is documents whose content hash matched the prior version; their chunks were re-linked, not re-embedded.

If any documents conflicted (the server refused to carry or insert them), they appear in the summary and are logged at `WARN` level. The run still finalizes if conflicts are all injection rejections; it aborts if any conflict cannot be resolved.

## After the run

Use `mnm versions list <slug>` to confirm the new revision is active, and `mnm versions rollback <slug>` to revert if needed. See [Versions & rate limits](./versions-rate-limits.md) for the full version-management reference.

## Related pages

- [Manifests](./manifests.md) — authoring `hierarchy.yaml` before running an ingest.
- [Ingestion pipeline](./ingestion-pipeline.md) — how the pipeline works internally.
- [Versions & rate limits](./versions-rate-limits.md) — inspecting and managing versions after a run.

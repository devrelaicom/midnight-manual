---
title: Manifests
sidebar_label: Manifests
description: What a hierarchy.yaml manifest is, how to author one, and how to use mnm manifest init / generate / check to build and validate manifests.
---

# Manifests

A manifest (`hierarchy.yaml`) is a YAML file that tells the ingestion pipeline what to ingest, how to organise it into a hierarchy, and what provenance to record for each node. Every ingest run needs one.

You can write manifests by hand (for complete control), generate them from globs and an optional sitemap (for third-party repos), or start from an empty template. All three workflows produce the same file format.

## Manifest schema

Manifests are versioned. Every file starts with:

```yaml
manifest_version: 1
```

The `root` node is the top of the content tree. Every node can carry:

| Field | Required | Description |
|---|---|---|
| `name` | yes | Human-readable label for this node. |
| `path` | no | Directory path, relative to the source root. Auto-discovers all supported files under this directory. |
| `file` | no | Single file path. Use instead of `path` for individual files. |
| `published_url` | no | Base URL for this node; inherited by descendants. |
| `include` | no | Glob list; filters files when `path:` is used. |
| `exclude` | no | Glob list; additive over `.gitignore` and defaults. |
| `children` | no | Nested child nodes (same schema, recursive). |
| `provenance` | no | Trust/attribution metadata; inherited by descendants. |
| `code_embeddings` | no | Set to `false` to skip `voyage-code-3` vectors for code files in this subtree. |

`provenance` carries:

| Field | Values |
|---|---|
| `attribution` | `foundation`, `partner`, `third_party`, `community` |
| `verified` | `true` / `false` |
| `verified_by` | free-form string (e.g. `midnight-foundation`) |
| `tags` | list (e.g. `[trust:high]`) |

Attribution and `verified` drive the confidence-scoring system: `foundation`-attributed, `verified: true` nodes receive the highest trust weight.

## A worked example

Here is the `manifests/midnight/midnight-docs.yaml` manifest used for the official Midnight documentation:

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

Each `path:` entry auto-discovers every supported file under that directory (honouring `.gitignore`). Provenance at the `root` level is inherited by all five children — you only need to declare it once.

For a code repo with mixed content you can scope includes per node:

```yaml
manifest_version: 1
root:
  name: openzeppelin-compact-contracts
  path: .
  exclude:
    - "**/node_modules/**"
    - "**/dist/**"
    - "**/target/**"
    - "**/managed/**"
  provenance:
    attribution: partner
    verified: false
    tags: [trust:high]
```

## The three manifest commands

Admin commands are hidden by default. Reveal them with `export MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1`.

### mnm manifest init

Writes an empty starter manifest with comments explaining every field:

```bash
mnm manifest init                         # writes ./hierarchy.yaml
mnm manifest init -o my-source.yaml       # custom output path
mnm manifest init -o hierarchy.yaml --force  # overwrite existing
```

The generated file has `manifest_version: 1`, a `root` node with a placeholder name, and commented-out examples for `published_url` and `children`. It is a valid manifest you can ingest immediately (it walks zero files) — edit it before running an ingest.

### mnm manifest generate

Walks a source tree against glob patterns and, optionally, a sitemap, then writes a populated `hierarchy.yaml`:

```bash
# Basic: all Markdown under ./docs
mnm manifest generate 'docs/**/*.md' --base ./my-repo -o my-repo.yaml

# With a sitemap for URL matching
mnm manifest generate 'docs/**/*.md' 'docs/**/*.mdx' \
    --base ./my-repo \
    --sitemap https://my-repo.example.com/sitemap.xml \
    -o my-repo.yaml

# Multiple include globs (positional or --include)
mnm manifest generate --base ./compact-contracts \
    --include '**/*.compact' --include '**/*.ts' --include '**/*.md' \
    -o compact-contracts.yaml

# Dry-run: print YAML to stdout, write nothing
mnm manifest generate 'docs/**/*.md' --base ./my-repo --dry-run
```

`generate` honours `.gitignore` during the walk and skips common generated directories (`node_modules`, `target`, `dist`, and similar) by default. When a sitemap is supplied it matches discovered files to sitemap URLs so each leaf node gets a `published_url`; unmatched files are reported but not excluded (pass `--strict` to fail on any unmatched file).

Key flags:

| Flag | Description |
|---|---|
| `--base <DIR>` | Root directory for glob resolution (default: `.`). |
| `--include <GLOB>` | Additional include pattern (repeatable). |
| `--exclude <GLOB>` | Exclude pattern (repeatable). |
| `--sitemap` | Sitemap URL or file path for URL matching (repeatable). Probes `robots.txt` first. |
| `--url-base` | Fallback URL prefix for unmatched files. |
| `--name` | Root node name (default: the `--base` directory name). |
| `-o` | Output path (default: `./hierarchy.yaml`). |
| `--force` | Overwrite an existing output file. |
| `--strict` | Fail if any file is unmatched against the sitemap. |
| `--dry-run` | Print YAML to stdout; write nothing. |

### mnm manifest check

Validates a manifest locally — schema, paths, file existence — without contacting the server:

```bash
mnm manifest check hierarchy.yaml
mnm manifest check my-source.yaml --sitemap https://example.com/sitemap.xml
```

`check` is the right step before committing a manifest or running an ingest. It catches malformed YAML, missing `manifest_version`, invalid `path`/`file` references, and — when `--sitemap` is provided — mismatched URLs.

## Two authoring workflows

**Workflow A: own the repo.** Commit `hierarchy.yaml` alongside the content. Team members re-ingest from the committed manifest:

```bash
cd /path/to/your-docs-repo
mnm manifest init -o hierarchy.yaml        # start from the template
# … edit hierarchy.yaml …
mnm manifest check hierarchy.yaml          # validate before committing
git add hierarchy.yaml && git commit -m "chore: add ingest manifest"
```

**Workflow B: third-party repo.** When you cannot commit to the source, keep the manifest in your own working tree:

```bash
mnm manifest generate \
    'docs/**/*.{md,mdx}' \
    --base ~/code/their-docs-repo \
    --sitemap https://their-docs.example.com/sitemap.xml \
    -o ~/midnight-manual-manifests/their-source.yaml
```

## Related pages

- [Running an ingest](./running-an-ingest.md) — how to use `mnm ingest run` once the manifest is ready.
- [Ingestion pipeline](./ingestion-pipeline.md) — how the pipeline uses manifests during a run.
- [Smart chunker](/docs/concepts/smart-chunker) — how individual files are split into chunks.

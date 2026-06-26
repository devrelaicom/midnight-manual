---
title: Ingestion pipeline
sidebar_label: Ingestion pipeline
description: How content moves from a manifest through chunking, embedding, and atomic promotion into the live corpus.
---

# Ingestion pipeline

Getting content into the corpus is a single, resumable, atomically-promoted flow. The orchestrator is pure — it never touches the database directly — which makes ingestion predictable and testable.

```
manifest ─▶ .gitignore-aware walk ─▶ per-file chunker ─▶ VoyageAI embed ─▶ versioned corpus ─▶ promote
   (or auto-generated)          (markdown / code / fallback)            (carry-forward unchanged docs)
```

## Stages

**1. Manifest → walk.** The walker reads the `hierarchy.yaml` manifest (or an auto-generated one), respects `.gitignore` rules, and applies the manifest's `include`/`exclude` globs to produce a list of files to process.

**2. Walk → chunker.** Each file routes to the appropriate chunker based on its extension (and shebang for scripts):

- Markdown files go to the **heading-aware chunker**: content is split along the heading hierarchy, so each chunk stays within a named section and carries its full `heading_path`.
- Source code files (`.compact`, `.ts`, `.rs`, and [many others](/docs/concepts/smart-chunker)) go to the **symbol-aware chunker**: content is split on function, class, and `impl` boundaries using `tree-sitter` (or the `compactp` parser for Compact files).
- Anything else falls back to a **line-window chunker**: token-budgeted, non-overlapping, always succeeds.

All chunkers target the same token budget (default: 1024 tokens, configurable with `--chunk-tokens`). A catastrophically malformed file falls back to line-window chunking rather than aborting the run.

**3. Chunker → VoyageAI embed.** Every chunk is embedded before upload. The CLI handles this directly — the server never loads an embedding model. Two vectors are produced per chunk of a code file:

- A **general contextualized vector** (`voyage-context-3`) for every chunk. Contextualization groups sibling chunks and sends them together so the embedding model has surrounding context. This is what the search endpoint queries.
- A **code vector** (`voyage-code-3`) additionally for chunks of code-kind files. Code-specific embeddings are optimized for exact symbol retrieval. You can disable this per-source with `code_embeddings: false` in the manifest or `--no-code-embeddings` at ingest time.

Embedding is the slow step. For bulk runs, set `VOYAGE_API_KEY` to embed directly against your own VoyageAI account (BYOK); without it, embedding is proxied through the server and counts against its token budget.

**4. Embed → versioned corpus.** Embedded chunks are uploaded to the server in batches (`--batch-size`, default 25). The server allocates a new `source_version` in `building` state — invisible to search while being built.

**5. Finalize → promote.** A single `finalize` call flips the version from `building` to `active` and demotes the previous version to `inactive` in one transaction. Readers never see a half-built corpus.

## What makes it reliable

**Versioned, atomic promotion.** Every ingest builds a new `source_version` in a `building` state, invisible to search. A single finalize step flips it `active` and demotes the previous one in one transaction — readers never see a half-built corpus, and rollback is one command: `mnm versions rollback <slug>`.

**Carry-forward.** If a document's content hash is unchanged from the active version, its chunks (and their embeddings) are re-linked instead of re-embedded. Re-ingesting a docs site where two pages changed costs you two pages of work, not the whole site. Carry-forward is gated on model identity: if the embedding model changed since the last run, the pipeline re-embeds everything to keep vectors consistent.

**Per-file dispatch.** A `README.md` next to a `lib.rs` next to a `Cargo.toml` each routes to the right chunker automatically, by extension. No manual routing configuration needed.

**Resilient.** A catastrophically malformed source file falls back to line-window chunking rather than aborting the run, and any chunk that fails to embed lands in an `embed_failed` state and is simply skipped by readers (so search has clean gaps, never broken results).

**Abort on failure.** If anything goes wrong between the start of an upload and finalization, the CLI calls the `abort` endpoint so the in-progress `source_version` is marked dead and does not block the next attempt.

**Observable.** Each run emits an `ingest_complete` telemetry event with documents added, updated, and skipped, plus duration — counts only, never content.

## Related pages

- [Manifests](./manifests.md) — how to author the manifest the pipeline starts from.
- [Running an ingest](./running-an-ingest.md) — the commands that drive the pipeline.
- [Smart chunker](/docs/concepts/smart-chunker) — detail on how each file type is split.
- [Versions & rate limits](./versions-rate-limits.md) — how to inspect, roll back, and retire versions after a run.

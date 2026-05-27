# Real BPE token counts in `mn-content`

Date: 2026-05-27
Crate: `mn-content`
Author: tooling follow-up (Item 2 from original handover)

## Problem

`crates/mn-content/src/tokens.rs::count` currently returns the
whitespace-word count of a chunk:

```rust
pub fn count(text: &str) -> u32 {
    u32::try_from(text.split_whitespace().count()).unwrap_or(u32::MAX)
}
```

That value is stored in `document.token_count` and `chunk.token_count`
(see `crates/mn-store/src/entities/document.rs` and
`crates/mn-server/src/routes/admin_ingest.rs`), and is surfaced to
operators via `mnm ingest plan` as the upload-size estimate. The number
is meant to approximate "how many tokens will the embedder see," and a
whitespace split is off by ~30-100% for typical English documentation
and far worse for code (single identifier `Vec::<HashMap<String, _>>`
counts as 1 word but ~12 BPE pieces).

The ingest-UX design note
(`docs/superpowers/specs/2026-05-25-ingest-ux-design.md`, §3.4 row for
`token_count`) called this out explicitly: real counts should come from
the `tokenizers` crate that is already a transitive dependency of
`fastembed`.

## Chosen approach

Vendor the `bge-base-en-v1.5` tokenizer (HuggingFace
`Xenova/bge-base-en-v1.5/tokenizer.json`, 711 KB) into the crate at
`crates/mn-content/assets/bge-base-en-v1.5/tokenizer.json`, embed it
via `include_bytes!`, and parse it once with
`tokenizers::Tokenizer::from_bytes` behind a `std::sync::OnceLock`.

`tokens::count` then calls `tokenizer.encode(text, true)` and returns
`encoding.get_ids().len()` — the exact input length the embedder would
see, including the `[CLS]` and `[SEP]` BERT special tokens that the
model's post-processor inserts.

### Why vendor instead of reusing the fastembed singleton

`mn-embedding::Embedder` exposes the `Tokenizer` (it's a public field
on `TextEmbedding`), so in principle we could route token counting
through there. We chose not to because:

1. **Ingest planning does not need the model.** `mnm ingest plan`
   walks a manifest, chunks files, and reports estimates without
   touching the embedder. Threading a 450 MB ONNX load through that
   path just to get token counts is a major regression for the most
   common UX entry point.
2. **`mn-content` is `mn-embedding`-free today.** Adding that crate as
   a dependency would create a circular-looking layering — `mn-content`
   is *upstream* of embedding in the ingest pipeline.
3. **`tokenizers` is already in the build graph** as a transitive dep
   of `fastembed`. Adding a direct dependency adds zero new
   third-party crates to the lockfile.

### Why vendor instead of downloading at runtime

The `tokenizers` crate's `from_pretrained` requires the `http` feature
(pulls `hf-hub` + `ureq`), which would force ingest planning to make a
network call. Vendoring keeps the operation offline and deterministic;
the file is 711 KB, smaller than several existing test fixtures.

### Lifecycle

```rust
static TOKENIZER: OnceLock<Tokenizer> = OnceLock::new();

fn tokenizer() -> &'static Tokenizer {
    TOKENIZER.get_or_init(|| {
        Tokenizer::from_bytes(BGE_BASE_TOKENIZER_BYTES)
            .expect("vendored bge tokenizer must parse")
    })
}
```

A single process-wide instance. `Tokenizer::encode` is `&self`, so no
locking on the hot path. The `OnceLock` cost is one `Relaxed` load per
call.

The `expect` is justified: the bytes are compiled into the binary, so
a parse failure means the build is broken, not a runtime condition the
caller could handle.

## Performance impact

A `tokenizer.encode` call for a typical 4 KB Markdown chunk is on the
order of a few hundred microseconds — measurably slower than
`split_whitespace().count()` but still negligible compared to chunking
(~milliseconds with tree-sitter) and embedding (~50-200 ms per chunk).

Concretely:

- v0 (whitespace): ~50 ns per chunk.
- v1 (BPE): ~100-500 µs per chunk (single-threaded encode, no SIMD
  fast path).

For a 10,000-chunk ingest (current docs corpus is ~1,200), that adds
1-5 seconds to `ingest plan`, which is dwarfed by file I/O and
chunking time. Acceptable. If it ever becomes hot we can batch via
`tokenizer.encode_batch` — the API is already there.

## Provenance

The vendored file is bit-identical to the upstream
`Xenova/bge-base-en-v1.5/tokenizer.json` as of 2026-05-27:

```
sha256: d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66
size  : 711_396 bytes
source: https://huggingface.co/Xenova/bge-base-en-v1.5/resolve/main/tokenizer.json
```

If we ever upgrade the embedder (e.g. to `bge-large-en-v1.5`), the
matching tokenizer.json needs to be re-vendored alongside the model
swap. A test in `mn-content::tokens::tests::known_counts_match_model`
pins a handful of fixture strings to their expected token counts; that
test fails loudly if the vendored vocab drifts away from what the live
embedder produces.

## Tests

- Unit fixtures with hand-curated strings whose BPE counts were
  cross-checked against the same `tokenizer.json` (the test is
  effectively a tautology against itself, but it pins the
  contract).
- Edge cases: empty string (returns 2 — just `[CLS]` + `[SEP]`),
  whitespace-only string, Unicode, mixed code/prose.
- Property: count is monotonic — appending text never decreases the
  count (modulo truncation, which is `null` in our vendored config).

No criterion bench is added; the existing `benches/` directory has no
scaffolding, and bootstrapping criterion just for this micro-routine
isn't justified.

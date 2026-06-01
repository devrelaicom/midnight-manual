# CLI-side embedding by default + server OOM guardrails

- **Date:** 2026-06-01
- **Status:** Approved (design)
- **Branch:** `fix/server-oom-ingest` (off `main` — deliberately independent of the manifest-ingestion branch)
- **Related incident:** Production `midnight-manual` OOM/crash-loop during a bulk ingest of `midnight-ledger` (2026-06-01).

## Context

Ingesting a single large manifest (`midnight-ledger`, ~518 files / ~10k chunks) drove the
production server into a kernel OOM (`exit_code=137, oom_killed=true`) and a reboot loop —
a self-inflicted DoS. Production was stabilised mid-incident by scaling the Fly machine
`1 GB → 2 GB` (live only; `fly.toml` still says 1 GB at the time of writing).

### Verified root cause (read from code during the incident)

The original handover hypothesis — that the upload handler embeds the whole batch inline and
buffers an unbounded body — is **wrong**. The code shows:

- **The CLI does not embed.** `mnm ingest run` chunks locally and uploads raw chunk *text*
  (`crates/mn-cli/src/commands/ingest/run.rs`). Confirmed by the worker's own comment:
  *"the CLI doesn't run the embedder"* (`crates/mn-server/src/jobs/embedder.rs:5-6`).
- **The upload handler does not embed.** `upload_documents` writes chunk rows with
  `embedding: None` and `status: EmbedFailed` (`crates/mn-server/src/routes/admin_ingest.rs`).
- **A background worker embeds, server-side, 16 chunks / 30 s**, using the local
  `bge-base-en-v1.5` ONNX model (`crates/mn-server/src/jobs/embedder.rs`).
- **The model loads eagerly at boot:** `main.rs` `await`s `LocalEmbedder::load(...)` *before*
  the listener binds (`crates/mn-server/src/main.rs:94,137`). A ~450 MB resident model
  (`crates/mn-embedding/src/embedder.rs`) on a 1 GB machine leaves near-zero headroom; the
  OOM RSS was ~877 MB. A large ingest fills the `embed_failed` backlog, which keeps the
  worker continuously active at the memory ceiling → repeated OOM → crash loop.
- **The reranker is not on the server:** *"used MCP-side only; the cloud server never sees a
  reranker invocation"* (`crates/mn-embedding/src/reranker.rs:4-5`).
- **A 16 MiB body limit already exists** (`MAX_BODY_BYTES`, applied via `DefaultBodyLimit`
  *and* `RequestBodyLimitLayer`, `crates/mn-server/src/app.rs:20,196-197`). The request body
  was never the OOM cause.

**Conclusion:** the OOM is the embedder model + ONNX working set against an undersized
machine, made permanent by the eager boot-load and a large server-side embed backlog.

## Goal

Make ingesting a large repo **unable to OOM the server** by moving embedding off the
memory-constrained server onto the CLI (which already loads `bge-base-en-v1.5` for querying
and runs on a developer's machine). The server's ONNX model becomes a *fallback*, never
resident under normal operation. Add guardrails so the failure modes are safe and cannot
silently regress.

## Non-goals

- **Sentry / observability** — deferred to a separate follow-up branch (sequencing decision:
  "OOM durable fix first"). A kernel OOM (SIGKILL) cannot be captured in-process anyway.
- **Cleaning up the orphaned `building` source_version** left by the aborted incident ingest —
  noted as operational follow-up, not part of this change.
- **Byte-budget batching** in the CLI (batching by serialized size rather than doc count) —
  YAGNI for v1; the 413 path + a smaller default batch size cover it.

## Resolved decisions

1. **CLI embed failure ⇒ hard-fail with guidance** (not silent fallback to server embedding).
2. **Server worker stays enabled but lazy-loads** the model (only on the first non-empty batch).
3. **CLI default `--batch-size` 50 → 25** (rather than raising the 16 MiB server cap), because
   embeddings inflate each chunk's JSON by ~8 KB.
4. **Include the 422-visibility fix** (handover objective 4) — it lives in the same CLI file.
5. **Echo the embedding model per upload batch** so the server can reject a client↔server
   model mismatch at the point the vectors arrive.

## Changes by component

| # | Component | Change |
|---|-----------|--------|
| A | **Wire schema** — `ChunkUpload` + `UploadDocumentsRequest` (server `admin_ingest.rs` and CLI `run.rs`) | Add `ChunkUpload.embedding: Option<Vec<f32>>` and `UploadDocumentsRequest.embedding_model: Option<String>`, both `#[serde(default)]`. Present ⇒ stored `Ready`; absent ⇒ today's `EmbedFailed`. **No DB migration** — `chunk::NewChunk` already accepts `embedding` + `status`. |
| B | **Server upload validation** — `insert_new_document` / `upload_documents` | When any chunk in a batch carries `embedding`: (1) `embedding_model` must be present and resolve to the **same model id as the run** (`source_version.embedding_model_id`), else reject with **`ErrorCode::EmbeddingModelMismatch`** (the same code the run-start check uses, which the CLI already recognises) naming expected vs. received; (2) each vector `len == 768`, else reject with **`ErrorCode::InvalidRequest`**. On success insert with `status: Ready`. Text-only uploads skip both checks (existing `EmbedFailed` path). |
| C | **Server worker** — `jobs/embedder.rs` + `main.rs` | **Lazy model load.** Stop `await`-ing `LocalEmbedder::load(...)` at boot. `LocalEmbedder` holds `cache_dir` + a `OnceCell<Embedder>` and calls `mn_embedding::embedder::global()` only on its first `embed()` call. `embed_once` already returns early on an empty batch, so an idle server never loads the model. Worker remains enabled by default; a load failure becomes a worker warning instead of a boot crash. |
| D | **CLI ingest** — `run.rs` | New flag `--enable-server-embedding` (default **false** ⇒ CLI embeds). When embedding: load `embedder::global(cache_dir)` once, embed each batch's chunk contents, attach vectors, set the batch `embedding_model`. **Hard-fail** with remediation if the model can't load. When the flag is set: leave `embedding: None` and omit `embedding_model` (today's path). Default `--batch-size` lowered to 25. |
| E | **fly.toml** | `memory = "2gb"` — makes the live scale durable so a deploy can't revert to 1 GB. |
| F | **Body limit** | Keep the 16 MiB cap as the safety bound; rely on the lower CLI batch size to fit embedding-inflated batches; add a regression test asserting an over-limit body ⇒ 413. |
| G | **CLI 422 visibility** — `translate_upload_error` | Currently *replaces* the error and only greps for `"413"`, discarding the server body. Change to chain via `.context(...)` so the real `{status} from {url}: {body}` (which names the failing field) surfaces. |

## Data flow — default (CLI embeds)

```
walk → chunk → [embed each batch locally] → PUT chunks WITH vectors + embedding_model
   → server validates model id + dim → insert Ready → finalize → searchable immediately
```

Server worker stays idle; the model is never loaded. Server RSS ≈ base server, well under 2 GB.

## Data flow — `--enable-server-embedding`

```
walk → chunk → PUT text-only → server inserts EmbedFailed
   → worker detects backlog → lazy-loads model → embeds 16/30s → Ready
```

Today's behaviour, now opt-in.

## Memory outcome

- Default path: no ~450 MB model resident on the server during ingest → OOM eliminated.
- The eager boot-load (the crash-loop vector) is gone.
- `--enable-server-embedding` runs still get the model, but now on a 2 GB machine with the
  body and batch bounds in place.

## Compatibility (deploy order is flexible)

| Client | Server | Behaviour |
|--------|--------|-----------|
| New CLI (embeds) | **Old** server | Server ignores unknown `embedding`/`embedding_model` fields ⇒ `EmbedFailed` ⇒ old eager worker embeds. Works (degraded). |
| **Old** CLI (text-only) | New server | No fields ⇒ `None` ⇒ lazy worker embeds. Works. |
| New CLI (embeds) | New server | Happy path: stored `Ready`, validated, searchable immediately. |

## Error handling

- CLI `--embedding-model` must be `bge-base-en-v1.5@1` when embedding locally (the CLI can
  only produce bge-base vectors); a mismatch ⇒ error suggesting `--enable-server-embedding`.
- Model not downloaded / ONNX init fails ⇒ hard error: *"run `mnm models pull`, or pass
  `--enable-server-embedding`"*.
- Embeddings present without a matching `embedding_model` ⇒ rejected with
  `ErrorCode::EmbeddingModelMismatch`, naming expected vs. received.
- Wrong-dim vector ⇒ rejected with `ErrorCode::InvalidRequest`.
- Over-limit body ⇒ 413 (existing behaviour, now regression-tested); the CLI's existing
  message suggests lowering `--batch-size`.

## Testing / regression guards

- **Server unit:** `Some(embedding)` ⇒ `Ready` + stored vector; `None` ⇒ `EmbedFailed`;
  wrong dim ⇒ rejected; `embedding_model` ≠ run model ⇒ rejected.
- **Server unit:** an injected `EmbedFn` loader is **not invoked** on an empty backlog
  (proves lazy / no boot-load).
- **CLI unit:** embedding-enabled ⇒ `ChunkUpload.embedding` is `Some(768)` and the batch
  carries `embedding_model`; `--enable-server-embedding` ⇒ both absent.
- **Body-limit test:** an over-limit body ⇒ 413.

## Rollout notes

- The live machine is already at 2 GB; `fly.toml` change (E) makes that durable. **Do not
  `fly deploy` from a branch where `fly.toml` still says 1 GB** until E lands.
- No DB migration. Deploy order is flexible per the compatibility matrix.

## Open questions

None.

# CLI-side embedding + server OOM guardrails — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop a large ingest from OOM-ing the server by embedding on the CLI by default (server embedding becomes opt-in via `--enable-server-embedding`), lazy-loading the server's model, validating client-supplied embeddings, and pinning the Fly machine to 2 GB.

**Architecture:** The CLI already links `mn-embedding` (used by `mnm models pull`). It will embed each upload batch locally and send 768-dim vectors plus the model id. The server stores supplied vectors as `Ready` (no migration — `chunk::NewChunk` already has `embedding` + `status`), validates the model id and dimension, and its background worker now lazy-loads the ONNX model only when it finds a real `embed_failed` backlog (so an idle server never holds the ~450 MB model — the OOM-on-boot vector is gone).

**Tech Stack:** Rust, axum, sqlx/Postgres+pgvector, fastembed (`bge-base-en-v1.5`, 768-dim), clap v4, anyhow, tokio. Tests: `cargo test` (unit) and `cargo test --features integration` (testcontainers Postgres).

**Spec:** `docs/superpowers/specs/2026-06-01-cli-embedding-oom-fix-design.md`

**Branch:** `fix/server-oom-ingest` (already created off `main`).

---

## File map

| Path | Responsibility | Change |
|------|----------------|--------|
| `fly.toml` | Fly machine config | Memory 1 GB → 2 GB |
| `crates/mn-server/src/routes/admin_ingest.rs` | Upload protocol + wire schema | Add `ChunkUpload.embedding`, `UploadDocumentsRequest.embedding_model`; validate model+dim; insert `Ready` when embedding present |
| `crates/mn-server/src/jobs/embedder.rs` | Background embedder worker | Replace eager `LocalEmbedder` with lazy `LazyEmbedder`; add unit test |
| `crates/mn-server/src/main.rs` | Server boot | Stop awaiting model load at boot; spawn worker with `LazyEmbedder` |
| `crates/mn-server/tests/admin_ingest_endpoints.rs` | Upload integration tests | Add embedded-upload happy/mismatch/dim tests |
| `crates/mn-server/tests/read_endpoints.rs` (or new `body_limit.rs`) | Body-limit guard | Add 413 regression test |
| `crates/mn-cli/src/commands/ingest/run.rs` | CLI ingest | `--enable-server-embedding` flag, default batch 25, local embedding, model echo, 422-visibility fix |

No DB migration. No new dependencies.

---

## Task 1: Pin the Fly machine to 2 GB

**Files:**
- Modify: `fly.toml`

- [ ] **Step 1: Edit the `[[vm]]` memory**

In `fly.toml`, change the `[[vm]]` block:

```toml
[[vm]]
size = "shared-cpu-1x"
memory = "2gb"
```

(It currently reads `memory = "1gb"`. The live machine is already scaled to 2 GB; this makes a `fly deploy` stop reverting it.)

- [ ] **Step 2: Verify the file parses**

Run: `grep -A2 '\[\[vm\]\]' fly.toml`
Expected: shows `memory = "2gb"`.

- [ ] **Step 3: Commit**

```bash
git add fly.toml
git commit -m "fix(server): pin Fly machine to 2GB to match live scale"
```

---

## Task 2: Server wire schema — accept `embedding` + `embedding_model`

**Files:**
- Modify: `crates/mn-server/src/routes/admin_ingest.rs` (structs `ChunkUpload` ~line 122, `UploadDocumentsRequest` ~line 150)
- Test: `crates/mn-server/src/routes/admin_ingest.rs` (new `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Append to the bottom of `crates/mn-server/src/routes/admin_ingest.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_request_deserializes_embedding_and_model() {
        let body = serde_json::json!({
            "embedding_model": "bge-base-en-v1.5@1",
            "documents": [{
                "path": "a.md",
                "kind": "markdown",
                "content_hash": "h",
                "provenance": {},
                "chunks": [{
                    "chunk_index": 0,
                    "total_chunks": 1,
                    "content": "hello",
                    "content_hash": "c",
                    "embedding": [0.1_f32, 0.2, 0.3]
                }]
            }]
        });
        let req: UploadDocumentsRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.embedding_model.as_deref(), Some("bge-base-en-v1.5@1"));
        assert_eq!(req.documents[0].chunks[0].embedding.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn upload_request_defaults_embedding_fields_to_none() {
        let body = serde_json::json!({
            "documents": [{
                "path": "a.md", "kind": "markdown", "content_hash": "h", "provenance": {},
                "chunks": [{ "chunk_index": 0, "total_chunks": 1, "content": "x", "content_hash": "c" }]
            }]
        });
        let req: UploadDocumentsRequest = serde_json::from_value(body).unwrap();
        assert!(req.embedding_model.is_none());
        assert!(req.documents[0].chunks[0].embedding.is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mn-server --lib routes::admin_ingest::tests -- --nocapture`
Expected: FAIL to compile — `ChunkUpload` has no field `embedding`, `UploadDocumentsRequest` has no field `embedding_model`.

- [ ] **Step 3: Add the fields**

In `ChunkUpload` (after the `token_count` field, ~line 146), add:

```rust
    /// Precomputed embedding vector, when the CLI embedded locally. `None`
    /// means the server-side embedder worker must fill it (`embed_failed`).
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
```

In `UploadDocumentsRequest` (after `batch_count`, ~line 159), add:

```rust
    /// Wire id (`name@revision`) of the model that produced any supplied
    /// `ChunkUpload.embedding` vectors. Required when embeddings are present;
    /// must match the run's model. `None` for text-only (server-embedded) runs.
    #[serde(default)]
    pub embedding_model: Option<String>,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mn-server --lib routes::admin_ingest::tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/routes/admin_ingest.rs
git commit -m "feat(server): accept client-supplied chunk embeddings on upload"
```

---

## Task 3: Server — validate model + dimension, store `Ready`

**Files:**
- Modify: `crates/mn-server/src/routes/admin_ingest.rs` (`upload_documents` ~line 282; `insert_new_document` ~line 607)
- Test: `crates/mn-server/tests/admin_ingest_endpoints.rs`

- [ ] **Step 1: Write the failing integration tests**

Append to `crates/mn-server/tests/admin_ingest_endpoints.rs`. These reuse the file's existing helpers (`cfg_with_auth`, `user_store_for`, `mint_admin_token`, `seed_source`, `json_call`, `hash_of`). Add a helper that builds an embedded-chunk payload, then the four cases.

```rust
fn embedded_document_payload(path: &str, content: &str, dim: usize) -> Value {
    let content_hash = format!("h:{path}:{}", hash_of(content));
    let chunk_hash = format!("c:{path}:{}", hash_of(content));
    let embedding: Vec<f32> = (0..dim).map(|i| (i as f32) * 0.001).collect();
    json!({
        "path": path, "kind": "markdown", "content_hash": content_hash,
        "char_count": content.len(), "token_count": 0, "provenance": {},
        "chunks": [{
            "chunk_index": 0, "total_chunks": 1, "content": content,
            "content_hash": chunk_hash, "heading_path": [], "symbol_path": [],
            "start_byte": 0, "end_byte": content.len(), "token_count": 0,
            "embedding": embedding,
        }],
    })
}

async fn start_run(app: axum::Router, slug: &str, token: &str) -> String {
    let (status, body) = json_call(
        app, "POST", &format!("/v1/admin/sources/{slug}/ingest-runs"), Some(token),
        Some(json!({"ingest_cli_version": "test", "embedding_model": "bge-base-en-v1.5@1"})),
    ).await;
    assert_eq!(status, StatusCode::OK, "start run: {body}");
    body["ingest_run_id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn embedded_upload_stores_ready_chunk() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;
    let run = start_run(app.clone(), &slug, &token).await;

    let (status, body) = json_call(
        app, "PUT", &format!("/v1/admin/sources/{slug}/ingest-runs/{run}/documents"),
        Some(&token),
        Some(json!({
            "embedding_model": "bge-base-en-v1.5@1",
            "documents": [embedded_document_payload("a.md", "hello world", 768)],
        })),
    ).await;
    assert_eq!(status, StatusCode::OK, "upload: {body}");

    let statuses: Vec<String> = sqlx::query_scalar(
        "SELECT status FROM chunk WHERE source_version_id = $1",
    )
    .bind(Uuid::parse_str(&run).unwrap())
    .fetch_all(&h.pool).await.unwrap();
    assert_eq!(statuses, vec!["ready".to_string()], "chunk should be ready, got {statuses:?}");
}

#[tokio::test]
async fn embedded_upload_without_model_is_409() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;
    let run = start_run(app.clone(), &slug, &token).await;

    let (status, _body) = json_call(
        app, "PUT", &format!("/v1/admin/sources/{slug}/ingest-runs/{run}/documents"),
        Some(&token),
        Some(json!({ "documents": [embedded_document_payload("a.md", "x", 768)] })),
    ).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn embedded_upload_wrong_model_is_409() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;
    let run = start_run(app.clone(), &slug, &token).await;

    let (status, _body) = json_call(
        app, "PUT", &format!("/v1/admin/sources/{slug}/ingest-runs/{run}/documents"),
        Some(&token),
        Some(json!({
            "embedding_model": "bge-base-en-v1.5@2",
            "documents": [embedded_document_payload("a.md", "x", 768)],
        })),
    ).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn embedded_upload_wrong_dim_is_400() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let (slug, _) = seed_source(&h.pool).await;
    let run = start_run(app.clone(), &slug, &token).await;

    let (status, _body) = json_call(
        app, "PUT", &format!("/v1/admin/sources/{slug}/ingest-runs/{run}/documents"),
        Some(&token),
        Some(json!({
            "embedding_model": "bge-base-en-v1.5@1",
            "documents": [embedded_document_payload("a.md", "x", 3)],
        })),
    ).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mn-server --features integration --test admin_ingest_endpoints embedded_ -- --nocapture`
Expected: FAIL — `embedded_upload_stores_ready_chunk` sees `embed_failed` (server ignores the embedding today); the mismatch/dim tests get `200 OK` instead of 409/400.
(Requires Postgres: either `DATABASE_URL` set, or Docker available for testcontainers.)

- [ ] **Step 3: Add batch validation in `upload_documents`**

In `upload_documents`, immediately after the `match sv.status { ... }` block that confirms `Building` (just before the `// Build a path → ... carry-forward` comment, ~line 356), insert:

```rust
    // If any chunk in this batch carries a precomputed embedding, the batch
    // MUST declare the model it used, and it MUST match the run's model and
    // the 768-dim contract. Text-only batches skip this and follow the
    // server-side embed path.
    let has_embeddings = req
        .documents
        .iter()
        .any(|d| d.chunks.iter().any(|c| c.embedding.is_some()));
    if has_embeddings {
        let Some(provided) = req.embedding_model.as_deref() else {
            return error::into_response(
                CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                    .message("upload supplies embeddings but no embedding_model")
                    .remediation("send the wire id (name@revision) the CLI embedded with")
                    .build(),
                rid,
            );
        };
        let run_model = match embedding_model::get_by_id(&state.pool, sv.embedding_model_id).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(request_id = rid, op = "upload_documents", error = %e, "run model lookup failed");
                return error::service_unavailable("run model lookup failed", rid);
            }
        };
        let expected = format!("{}@{}", run_model.name, run_model.revision);
        let provided_norm = match EmbeddingModelId::from_str(provided) {
            Ok(m) => format!("{}@{}", m.name, m.revision),
            Err(e) => {
                return error::into_response(
                    CoreError::builder(ErrorCode::InvalidRequest)
                        .message(format!("embedding_model parse failed: {e}"))
                        .remediation("supply name@revision (e.g. bge-base-en-v1.5@1)")
                        .build(),
                    rid,
                );
            }
        };
        if provided_norm != expected {
            return error::into_response(
                CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                    .message(format!(
                        "upload embeddings declare model `{provided_norm}` but run uses `{expected}`"
                    ))
                    .remediation("re-run ingest with --embedding-model matching the corpus, or --enable-server-embedding")
                    .build(),
                rid,
            );
        }
        for d in &req.documents {
            for c in &d.chunks {
                if let Some(v) = &c.embedding {
                    if v.len() != mn_embedding::BGE_BASE_DIM {
                        return error::into_response(
                            CoreError::builder(ErrorCode::InvalidRequest)
                                .message(format!(
                                    "chunk {}#{} embedding dim {} != {}",
                                    d.path,
                                    c.chunk_index,
                                    v.len(),
                                    mn_embedding::BGE_BASE_DIM
                                ))
                                .build(),
                            rid,
                        );
                    }
                }
            }
        }
    }
```

No import is needed: the code above uses the fully-qualified `mn_embedding::BGE_BASE_DIM`, which is re-exported at the `mn_embedding` crate root (`crates/mn-embedding/src/lib.rs:16`). `mn-embedding` is already a dependency of `mn-server` (`crates/mn-server/Cargo.toml:46` — the worker uses `mn_embedding::Embedder`).

- [ ] **Step 4: Store `Ready` when an embedding is present**

In `insert_new_document` (~line 653), replace the chunk-insert loop body's `embedding: None,` + `status: ChunkStatus::EmbedFailed,` with a presence check. Change the loop so each chunk computes:

```rust
    for chunk_upload in &doc.chunks {
        let chunk_node = node::insert(
            pool,
            sv_id,
            Some(doc_node),
            NodeKind::Chunk,
            &format!("chunk-{}", chunk_upload.chunk_index),
            chunk_upload.chunk_index,
        )
        .await?;
        let (embedding, status) = match &chunk_upload.embedding {
            Some(v) => (Some(v.clone()), ChunkStatus::Ready),
            None => (None, ChunkStatus::EmbedFailed),
        };
        chunk::insert(
            pool,
            chunk::NewChunk {
                source_version_id: sv_id,
                document_id: new_doc_id,
                node_id: chunk_node,
                chunk_index: chunk_upload.chunk_index,
                total_chunks: chunk_upload.total_chunks,
                content: &chunk_upload.content,
                content_hash: &chunk_upload.content_hash,
                embedding,
                embedding_model_id,
                heading_path: &chunk_upload.heading_path,
                symbol_path: &chunk_upload.symbol_path,
                start_byte: chunk_upload.start_byte,
                end_byte: chunk_upload.end_byte,
                token_count: chunk_upload.token_count,
                status,
            },
        )
        .await?;
    }
```

(`chunk::NewChunk.embedding` is `Option<Vec<f32>>` and `.status` is `ChunkStatus` — already the field types.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mn-server --features integration --test admin_ingest_endpoints embedded_ -- --nocapture`
Expected: PASS (4 tests). Also re-run the existing suite to confirm no regression:
Run: `cargo test -p mn-server --features integration --test admin_ingest_endpoints`
Expected: PASS (existing text-only lifecycle tests still green — they send no embedding, so chunks stay `embed_failed`).

- [ ] **Step 6: Commit**

```bash
git add crates/mn-server/src/routes/admin_ingest.rs crates/mn-server/tests/admin_ingest_endpoints.rs
git commit -m "feat(server): validate + store client embeddings as ready chunks"
```

---

## Task 4: Server — lazy-load the embedder model

**Files:**
- Modify: `crates/mn-server/src/jobs/embedder.rs` (`LocalEmbedder` ~line 155)
- Modify: `crates/mn-server/src/main.rs` (~line 87-107)
- Test: `crates/mn-server/src/jobs/embedder.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing unit test**

In `crates/mn-server/src/jobs/embedder.rs`, inside the existing `#[cfg(test)] mod tests` block (~line 187), add:

```rust
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn lazy_embedder_does_not_load_until_first_embed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let lazy = LazyEmbedder::new(Box::new(move || {
            let calls = calls2.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(ConstantEmbedder { dim: 4 }) as Arc<dyn EmbedFn>)
            })
        }));

        // Construction must not invoke the loader.
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // First embed loads once.
        let v = lazy.embed(vec!["a".to_owned()]).await.unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Second embed reuses the cached inner — no second load.
        let _ = lazy.embed(vec!["b".to_owned(), "c".to_owned()]).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mn-server --lib jobs::embedder::tests::lazy_embedder_does_not_load_until_first_embed`
Expected: FAIL to compile — `LazyEmbedder` does not exist.

- [ ] **Step 3: Implement `LazyEmbedder` and rewrite `LocalEmbedder` on top of it**

In `crates/mn-server/src/jobs/embedder.rs`, replace the `LocalEmbedder` struct + impls (~lines 155-185) with:

```rust
/// Boxed async loader that produces the real embed function on first use.
type EmbedLoader = Box<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<Arc<dyn EmbedFn>, String>> + Send>>
        + Send
        + Sync,
>;

/// Lazily-initialized [`EmbedFn`]. Holds a loader and a `OnceCell`; the loader
/// runs at most once, on the first `embed` call. Construction is cheap and
/// infallible, so the server boots without loading the ~450 MB ONNX model —
/// it only loads if the worker actually finds an `embed_failed` backlog.
pub struct LazyEmbedder {
    loader: EmbedLoader,
    inner: tokio::sync::OnceCell<Arc<dyn EmbedFn>>,
}

impl LazyEmbedder {
    /// Wrap an arbitrary loader (used by tests to inject a fake).
    #[must_use]
    pub fn new(loader: EmbedLoader) -> Self {
        Self { loader, inner: tokio::sync::OnceCell::new() }
    }

    /// Production constructor: loads the process-wide local ONNX embedder from
    /// `cache_dir` on first use.
    #[must_use]
    pub fn local(cache_dir: std::path::PathBuf) -> Self {
        Self::new(Box::new(move || {
            let cache_dir = cache_dir.clone();
            Box::pin(async move {
                let embedder = mn_embedding::embedder::global(cache_dir)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(Arc::new(EmbedderFn(embedder)) as Arc<dyn EmbedFn>)
            })
        }))
    }
}

impl EmbedFn for LazyEmbedder {
    fn embed(&self, texts: Vec<String>) -> EmbedFuture<'_> {
        Box::pin(async move {
            let inner = self.inner.get_or_try_init(|| (self.loader)()).await?;
            inner.embed(texts).await
        })
    }
}

/// Adapts a concrete [`mn_embedding::Embedder`] to the [`EmbedFn`] trait.
struct EmbedderFn(mn_embedding::Embedder);

impl EmbedFn for EmbedderFn {
    fn embed(&self, texts: Vec<String>) -> EmbedFuture<'_> {
        Box::pin(async move {
            self.0
                .embed_blocking(texts, None)
                .await
                .map_err(|e| e.to_string())
        })
    }
}
```

Keep the `use` lines for `Future`, `Pin`, `Arc` (already imported at the top of the file).

- [ ] **Step 4: Update `main.rs` to construct the lazy worker without awaiting a load**

In `crates/mn-server/src/main.rs`, replace the embedder block (~lines 87-107) `let local = jobs::embedder::LocalEmbedder::load(cache_dir).await ...; Some(jobs::embedder::spawn(... Arc::new(local) ...))` with:

```rust
    let _embedder_handle = if cfg.embedder_enabled {
        let cache_env = mn_embedding::cache::StdEnv;
        let cache_dir = mn_embedding::cache::resolve(&cache_env).context(
            "could not resolve model cache dir (MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME)",
        )?;
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("create model cache dir at {}", cache_dir.display()))?;
        // Lazy: the model is NOT loaded here. The worker loads it on its first
        // non-empty batch, so an idle server never holds the ~450 MB model.
        let lazy = jobs::embedder::LazyEmbedder::local(cache_dir);
        Some(jobs::embedder::spawn(
            pool.clone(),
            Arc::new(lazy),
            active.id,
            Duration::from_millis(cfg.embedder_interval_ms),
            cfg.embedder_batch_size,
        ))
    } else {
        tracing::info!("embedder worker disabled (MIDNIGHT_MANUAL_EMBEDDER_ENABLED=false)");
        None
    };
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mn-server --lib jobs::embedder`
Expected: PASS (existing `constant_embedder_returns_one_vector_per_text` + new `lazy_embedder_does_not_load_until_first_embed`).
Run: `cargo build -p mn-server`
Expected: builds (confirms `main.rs` compiles against the new API).

- [ ] **Step 6: Commit**

```bash
git add crates/mn-server/src/jobs/embedder.rs crates/mn-server/src/main.rs
git commit -m "fix(server): lazy-load embedder model off the boot path"
```

---

## Task 5: Server — regression test the 413 body bound

**Files:**
- Test: `crates/mn-server/tests/body_limit.rs` (new)

- [ ] **Step 1: Write the test**

Create `crates/mn-server/tests/body_limit.rs`:

```rust
//! Guard: oversize request bodies are refused with 413 before any handler
//! runs (the OOM-safety bound, `app::MAX_BODY_BYTES`).
#![cfg(feature = "integration")]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mn_server::{app, config::ServerConfig};
use tower::ServiceExt;

#[tokio::test]
async fn oversize_body_is_rejected_with_413() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    // One byte over the configured cap.
    let oversized = vec![b'x'; mn_server::app::MAX_BODY_BYTES + 1];
    let req = Request::builder()
        .method("PUT")
        .uri("/v1/admin/sources/whatever/ingest-runs/00000000-0000-0000-0000-000000000000/documents")
        .header("content-type", "application/json")
        .body(Body::from(oversized))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
```

- [ ] **Step 2: Confirm `MAX_BODY_BYTES` is public**

Run: `grep -n 'pub const MAX_BODY_BYTES' crates/mn-server/src/app.rs`
Expected: `pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;` (it is already `pub`).

- [ ] **Step 3: Run the test**

Run: `cargo test -p mn-server --features integration --test body_limit`
Expected: PASS — the `RequestBodyLimitLayer` returns 413 before routing/auth.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-server/tests/body_limit.rs
git commit -m "test(server): regression-guard the 413 request-body bound"
```

---

## Task 6: CLI — flag, schema fields, default batch 25

**Files:**
- Modify: `crates/mn-cli/src/commands/ingest/run.rs` (`Args` ~line 47; `ChunkUpload` ~line 726; `UploadDocumentsRequest` ~line 701)
- Test: same file (`#[cfg(test)] mod tests` ~line 838)

- [ ] **Step 1: Write the failing tests**

In the `tests` module of `crates/mn-cli/src/commands/ingest/run.rs`, add:

```rust
    #[test]
    fn default_batch_size_is_25_and_server_embedding_defaults_off() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            inner: Args,
        }
        let w = Wrap::try_parse_from(["ingest-run", "--source-slug", "s", "m.yaml"]).unwrap();
        assert_eq!(w.inner.batch_size, 25);
        assert!(!w.inner.enable_server_embedding);
    }

    #[test]
    fn enable_server_embedding_flag_parses() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            inner: Args,
        }
        let w = Wrap::try_parse_from(
            ["ingest-run", "--source-slug", "s", "--enable-server-embedding", "m.yaml"],
        )
        .unwrap();
        assert!(w.inner.enable_server_embedding);
    }

    #[test]
    fn chunk_upload_skips_embedding_when_none() {
        let c = ChunkUpload {
            chunk_index: 0, total_chunks: 1, content: "x".into(), content_hash: "c".into(),
            heading_path: vec![], symbol_path: vec![], start_byte: 0, end_byte: 1, token_count: 0,
            embedding: None,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("embedding"), "None embedding must be omitted: {s}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mn-cli --lib commands::ingest::run::tests`
Expected: FAIL to compile — `batch_size` default is 50, `enable_server_embedding` field missing, `ChunkUpload` has no `embedding` field.

- [ ] **Step 3: Add the flag and lower the batch default**

In `Args`, change the `batch_size` arg (~line 93) default and doc:

```rust
    /// Number of documents per upload batch (default: 25). Reduce if you hit
    /// 413 responses from the server (local embedding inflates each batch).
    #[arg(long, default_value_t = 25)]
    pub batch_size: usize,
```

Add a new field to `Args` (after `batch_size`):

```rust
    /// Embed on the server instead of locally. Off by default: the CLI embeds
    /// chunks with its local model and uploads the vectors, so the server
    /// never has to load the model. Use this when the local model is
    /// unavailable or you want the server to embed.
    #[arg(long)]
    pub enable_server_embedding: bool,
```

- [ ] **Step 4: Add the wire fields**

In the CLI's `ChunkUpload` (Serialize struct, ~line 726), add at the end:

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    embedding: Option<Vec<f32>>,
```

In the CLI's `UploadDocumentsRequest` (~line 701), add:

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    embedding_model: Option<String>,
```

Update the `ChunkUpload { ... }` literal inside the `docs` mapping (~line 431) to set `embedding: None,` (it will be filled later, per batch):

```rust
                .map(|c| ChunkUpload {
                    chunk_index: i32::try_from(c.chunk_index).unwrap_or(i32::MAX),
                    total_chunks: i32::try_from(c.total_chunks).unwrap_or(i32::MAX),
                    content: c.content.clone(),
                    content_hash: c.content_hash.clone(),
                    heading_path: c.heading_path.clone(),
                    symbol_path: c.symbol_path.clone(),
                    start_byte: i32::try_from(c.start_byte).unwrap_or(i32::MAX),
                    end_byte: i32::try_from(c.end_byte).unwrap_or(i32::MAX),
                    token_count: i32::try_from(c.token_count).unwrap_or(i32::MAX),
                    embedding: None,
                })
```

The two `UploadDocumentsRequest { ... }` literals (the per-batch one ~line 464) currently set only `documents`, `batch_index`, `batch_count`. Add `embedding_model: None,` to the existing literal for now (Task 7 fills it in):

```rust
        let body = UploadDocumentsRequest {
            documents: chunk.to_vec(),
            batch_index: i,
            batch_count,
            embedding_model: None,
        };
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mn-cli --lib commands::ingest::run::tests`
Expected: PASS (new + existing `parses_code_chunk_and_filter_flags` etc.).

- [ ] **Step 6: Commit**

```bash
git add crates/mn-cli/src/commands/ingest/run.rs
git commit -m "feat(cli): add --enable-server-embedding flag + embedding wire fields"
```

---

## Task 7: CLI — embed locally per batch and echo the model

**Files:**
- Modify: `crates/mn-cli/src/commands/ingest/run.rs` (helpers + upload loop + load point)
- Test: same file (`tests` module)

- [ ] **Step 1: Write the failing unit tests for the pure helpers**

In the `tests` module, add:

```rust
    #[test]
    fn attach_embeddings_distributes_in_order() {
        let mut docs = vec![
            DocumentUpload {
                path: "a".into(), kind: DocumentKind::Markdown, content_hash: "h".into(),
                source_url: None, published_url: None, language: None, source_modified_at: None,
                frontmatter: None, provenance: Provenance::default(), char_count: 0, token_count: 0,
                package: None,
                chunks: vec![
                    mk_chunk(0), mk_chunk(1),
                ],
            },
            DocumentUpload {
                path: "b".into(), kind: DocumentKind::Markdown, content_hash: "h".into(),
                source_url: None, published_url: None, language: None, source_modified_at: None,
                frontmatter: None, provenance: Provenance::default(), char_count: 0, token_count: 0,
                package: None,
                chunks: vec![ mk_chunk(0) ],
            },
        ];
        let vectors = vec![vec![1.0_f32], vec![2.0], vec![3.0]];
        attach_embeddings(&mut docs, vectors).unwrap();
        assert_eq!(docs[0].chunks[0].embedding, Some(vec![1.0]));
        assert_eq!(docs[0].chunks[1].embedding, Some(vec![2.0]));
        assert_eq!(docs[1].chunks[0].embedding, Some(vec![3.0]));
    }

    #[test]
    fn attach_embeddings_rejects_count_mismatch() {
        let mut docs = vec![DocumentUpload {
            path: "a".into(), kind: DocumentKind::Markdown, content_hash: "h".into(),
            source_url: None, published_url: None, language: None, source_modified_at: None,
            frontmatter: None, provenance: Provenance::default(), char_count: 0, token_count: 0,
            package: None, chunks: vec![mk_chunk(0)],
        }];
        assert!(attach_embeddings(&mut docs, vec![]).is_err());
    }

    #[test]
    fn local_model_must_be_bge_base() {
        assert!(validate_local_embedding_model("bge-base-en-v1.5@1").is_ok());
        let err = validate_local_embedding_model("some-other-model@1").unwrap_err();
        assert!(err.to_string().contains("--enable-server-embedding"), "{err}");
    }

    fn mk_chunk(idx: i32) -> ChunkUpload {
        ChunkUpload {
            chunk_index: idx, total_chunks: 2, content: format!("c{idx}"), content_hash: "c".into(),
            heading_path: vec![], symbol_path: vec![], start_byte: 0, end_byte: 0, token_count: 0,
            embedding: None,
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mn-cli --lib commands::ingest::run::tests::attach_embeddings_distributes_in_order`
Expected: FAIL to compile — `attach_embeddings` / `validate_local_embedding_model` undefined.

- [ ] **Step 3: Add the helper functions**

Add near the other free functions in `run.rs` (e.g. after `url_encode`):

```rust
/// Distribute one embedding vector per chunk, in document-then-chunk order.
///
/// # Errors
///
/// Errors if `vectors.len()` does not equal the total chunk count.
fn attach_embeddings(docs: &mut [DocumentUpload], vectors: Vec<Vec<f32>>) -> Result<()> {
    let total: usize = docs.iter().map(|d| d.chunks.len()).sum();
    if vectors.len() != total {
        return Err(anyhow!("embedder returned {} vectors for {total} chunks", vectors.len()));
    }
    let mut it = vectors.into_iter();
    for d in docs.iter_mut() {
        for c in d.chunks.iter_mut() {
            c.embedding = it.next();
        }
    }
    Ok(())
}

/// Local embedding can only produce `bge-base-en-v1.5` vectors. Reject any
/// other `--embedding-model` with a pointer to `--enable-server-embedding`.
///
/// # Errors
///
/// Errors if the wire id fails to parse or names a different model.
fn validate_local_embedding_model(model_wire: &str) -> Result<()> {
    use std::str::FromStr as _;
    let id = mn_core::model_id::EmbeddingModelId::from_str(model_wire)
        .map_err(|e| anyhow!("invalid --embedding-model `{model_wire}`: {e}"))?;
    if id.name != mn_embedding::embedder::MODEL_NAME {
        return Err(anyhow!(
            "local embedding only supports `{}@…`; got `{model_wire}`. \
             Pass --enable-server-embedding to ingest with a server-side model.",
            mn_embedding::embedder::MODEL_NAME
        ));
    }
    Ok(())
}

/// Load the process-wide local embedder, mapping failures to actionable advice.
///
/// # Errors
///
/// Errors if the model cache dir cannot be resolved or the model fails to load.
async fn load_local_embedder() -> Result<mn_embedding::Embedder> {
    let env = mn_embedding::cache::StdEnv;
    let cache_dir = mn_embedding::cache::resolve(&env)
        .context("could not resolve model cache dir (set MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME)")?;
    mn_embedding::embedder::global(cache_dir).await.map_err(|e| {
        anyhow!("could not load local embedder ({e}). Run `mnm models pull`, or pass --enable-server-embedding.")
    })
}

/// Embed every chunk of `docs` in place using the local embedder.
///
/// # Errors
///
/// Errors if the embedder call fails or returns the wrong vector count.
async fn embed_batch(emb: &mn_embedding::Embedder, docs: &mut [DocumentUpload]) -> Result<()> {
    let texts: Vec<String> = docs
        .iter()
        .flat_map(|d| d.chunks.iter().map(|c| c.content.clone()))
        .collect();
    if texts.is_empty() {
        return Ok(());
    }
    let vectors = emb.embed_blocking(texts, None).await.context("local embedding")?;
    attach_embeddings(docs, vectors)
}
```

- [ ] **Step 4: Load the embedder before starting the run, and embed per batch**

In `run_inner`, after the source check/create phase and **before** the `// ── Phase: start ingest run` block (~line 382), add:

```rust
    // Load the local embedder up front (unless the server will embed). Doing
    // this before we create a server-side run means a missing model fails
    // fast without leaving an orphaned `building` source_version.
    let embedder = if args.enable_server_embedding {
        None
    } else {
        validate_local_embedding_model(&args.embedding_model)?;
        reporter.phase("load_embedder", serde_json::json!({"model": args.embedding_model}));
        let emb = load_local_embedder().await?;
        reporter.phase_done("load_embedder", serde_json::json!({"model": args.embedding_model}));
        Some(emb)
    };
```

Then replace the upload loop (~lines 462-483) with:

```rust
    for (i, batch) in docs.chunks(batch_size).enumerate() {
        reporter.batch(i + 1, batch_count, "uploading documents");
        let mut batch_docs = batch.to_vec();
        if let Some(emb) = &embedder {
            if let Err(e) = embed_batch(emb, &mut batch_docs).await {
                abort_run(&client, server_url, &args.source_slug, start.ingest_run_id, &token).await;
                return Err(e.context(format!("embed batch {}/{batch_count}", i + 1)));
            }
        }
        let body = UploadDocumentsRequest {
            documents: batch_docs,
            batch_index: i,
            batch_count,
            embedding_model: embedder.as_ref().map(|_| args.embedding_model.clone()),
        };
        let result: Result<UploadDocumentsResponse> =
            put_json(&client, &upload_url, &token, &body).await;
        match result {
            Ok(r) => {
                accepted += r.accepted;
                carried += r.carried;
            }
            Err(e) => {
                abort_run(&client, server_url, &args.source_slug, start.ingest_run_id, &token)
                    .await;
                return Err(translate_upload_error(e, i + 1, batch_count, start.ingest_run_id)
                    .context("upload documents"));
            }
        }
    }
```

(Note: `translate_upload_error(e, ...)` now takes `e` by value — see Task 8. The `embedding_model: None` you added to this literal in Task 6 is replaced by the `embedder.as_ref().map(...)` line above. The dry-run early-return path is untouched, so `--dry-run` never loads the model.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mn-cli --lib commands::ingest::run::tests`
Expected: PASS. Note: the real ONNX `embed_blocking` path is exercised by manual/integration verification (Task 9), not by these unit tests — the unit tests cover the pure helpers (`attach_embeddings`, `validate_local_embedding_model`).

- [ ] **Step 6: Build to confirm the loop compiles**

Run: `cargo build -p mn-cli`
Expected: builds. (If `translate_upload_error` still takes `&anyhow::Error`, this fails — proceed to Task 8 which changes its signature, then re-build. To keep each task green, do Task 8 immediately.)

- [ ] **Step 7: Commit**

```bash
git add crates/mn-cli/src/commands/ingest/run.rs
git commit -m "feat(cli): embed chunks locally per batch and echo the model id"
```

---

## Task 8: CLI — surface the server's error body (422 visibility)

**Files:**
- Modify: `crates/mn-cli/src/commands/ingest/run.rs` (`translate_upload_error` ~line 566)
- Test: same file (`tests` module)

- [ ] **Step 1: Write the failing test**

In the `tests` module add:

```rust
    #[test]
    fn upload_error_preserves_server_body() {
        let original = anyhow!(
            "422 Unprocessable Entity from http://x/documents: \
             {{\"error\":{{\"code\":\"invalid_request\",\"message\":\"unknown field embeding\"}}}}"
        );
        let translated = translate_upload_error(original, 8, 11, Uuid::nil());
        let shown = format!("{translated:#}");
        assert!(shown.contains("422"), "must keep server status: {shown}");
        assert!(shown.contains("invalid_request"), "must keep server body: {shown}");
        assert!(shown.contains("batch 8/11"), "must add batch context: {shown}");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mn-cli --lib commands::ingest::run::tests::upload_error_preserves_server_body`
Expected: FAIL to compile (signature is `&anyhow::Error`) or FAIL assertion (body discarded).

- [ ] **Step 3: Rewrite `translate_upload_error` to chain the original**

Replace the function (~line 566) with:

```rust
/// Translate a batch-upload HTTP error into a helpful message, **preserving**
/// the underlying error (which carries the server's `{status}: {body}`, naming
/// the failing field). The CLI prints with `{:#}`, so the whole chain shows.
fn translate_upload_error(e: anyhow::Error, batch: usize, of: usize, run_id: Uuid) -> anyhow::Error {
    let msg = e.to_string();
    if msg.contains("413") {
        return e.context(format!(
            "batch {batch} exceeded the server payload limit; aborted run {run_id}. \
             Re-run with --batch-size 15 (or lower) — current default is 25 docs/batch"
        ));
    }
    e.context(format!(
        "upload failed at batch {batch}/{of}; aborted run {run_id}. \
         The server's response is shown above; re-run `mnm ingest run` to retry"
    ))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mn-cli --lib commands::ingest::run::tests`
Expected: PASS (all run.rs unit tests, including the existing `redacts_long_alnum_blobs`).

- [ ] **Step 5: Build the whole CLI**

Run: `cargo build -p mn-cli`
Expected: builds (call site from Task 7 now matches the by-value signature).

- [ ] **Step 6: Commit**

```bash
git add crates/mn-cli/src/commands/ingest/run.rs
git commit -m "fix(cli): chain the server error body in ingest upload failures"
```

---

## Task 9: Full verification + spec status

**Files:**
- Modify: `docs/superpowers/specs/2026-06-01-cli-embedding-oom-fix-design.md` (status line)

- [ ] **Step 1: Format + lint + unit tests (matches CI)**

Run:
```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: fmt clean, no clippy warnings, all unit tests pass. (Two `mn-cli` `auth_integration` loopback tests are known to fail in this sandbox — not a regression.)

- [ ] **Step 2: Integration tests (needs Postgres)**

Run: `cargo test -p mn-server --features integration --test admin_ingest_endpoints --test body_limit`
Expected: PASS. If no Postgres/Docker is available in this environment, record that these were not run and must pass in CI before merge — do not claim they passed.

- [ ] **Step 3: Manual end-to-end embedding check (optional but recommended)**

Against a LOCAL or disposable server only (never production), with the model pulled (`mnm models pull`):
```bash
cargo run -p mn-cli -- --server http://localhost:8080 ingest run \
  /tmp/mn-ingest/midnight-ledger/hierarchy.yaml --source-slug oom-test --yes --batch-size 10
```
Expected: completes; server logs show **no** `embedder worker promoted chunks` lines (chunks land `ready` directly); search returns results immediately.

- [ ] **Step 4: Update the spec status**

In `docs/superpowers/specs/2026-06-01-cli-embedding-oom-fix-design.md`, change the status line:

```markdown
- **Status:** Implemented
```

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/specs/2026-06-01-cli-embedding-oom-fix-design.md
git commit -m "docs(spec): mark CLI-embedding OOM fix implemented"
```

---

## Notes for the executor

- **Do NOT run the ingest against production** (`https://midnight-manual.midnightntwrk.expert`). The CLI defaults to production; always pass `--server http://localhost:8080` for manual checks.
- **Do NOT `fly deploy`** until Task 1 has landed (otherwise the deploy reverts the live machine to 1 GB).
- Sentry is intentionally out of scope (separate follow-up branch).
- `mn_embedding::BGE_BASE_DIM` and `mn_embedding::Embedder` are re-exported at the crate root; `mn_embedding::embedder::{global, MODEL_NAME}` and `mn_embedding::cache::{StdEnv, resolve}` are reached via their modules.

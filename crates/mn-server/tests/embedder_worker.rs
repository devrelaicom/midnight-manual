//! End-to-end exercises for the Phase-11a embedder background worker.
//!
//! Drives `jobs::embedder::embed_once` against a real Postgres + pgvector via
//! the testcontainer harness, using a fake [`EmbedFn`] so no ONNX model has
//! to load inside CI.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use mn_core::provenance::Provenance;
use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mn_server::jobs::embedder::{embed_once, EmbedFn, EmbedFuture};
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use sqlx::PgPool;
use uuid::Uuid;

struct ConstantEmbedder {
    dim: usize,
    fill: f32,
    calls: Arc<AtomicUsize>,
}

impl EmbedFn for ConstantEmbedder {
    fn embed(&self, texts: Vec<String>) -> EmbedFuture<'_> {
        let dim = self.dim;
        let fill = self.fill;
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(texts.iter().map(|_| vec![fill; dim]).collect()) })
    }
}

struct WrongSizedEmbedder;

impl EmbedFn for WrongSizedEmbedder {
    fn embed(&self, _texts: Vec<String>) -> EmbedFuture<'_> {
        // Always returns one less vector than requested.
        Box::pin(async move { Ok(vec![vec![0.0_f32; 768]]) })
    }
}

struct FailingEmbedder;

impl EmbedFn for FailingEmbedder {
    fn embed(&self, _texts: Vec<String>) -> EmbedFuture<'_> {
        Box::pin(async move { Err("model unavailable".to_owned()) })
    }
}

async fn seed_embed_failed_chunks(pool: &PgPool, count: usize) -> (Uuid, Uuid, Vec<Uuid>) {
    // Each test gets its own embedding_model row so list_embed_failed_batch
    // (scoped by model_id) sees only this test's chunks. The CI Postgres is
    // shared across test binaries — without per-test isolation the worker
    // would pick up rows seeded by sibling tests.
    let model_name = format!("test-emb-{}", Uuid::new_v4());
    let model_id = embedding_model::upsert(pool, &model_name, 1, 768, "test")
        .await
        .unwrap();
    let slug = format!("embedder-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Embedder Test", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, "0.1.0", "h")
        .await
        .unwrap();
    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let mut chunk_ids = Vec::with_capacity(count);
    for i in 0..count {
        let doc_node = node::insert(
            pool,
            sv_id,
            Some(root),
            NodeKind::Document,
            &format!("doc-{i}.md"),
            i32::try_from(i).unwrap(),
        )
        .await
        .unwrap();
        let doc_id = document::insert(
            pool,
            document::NewDocument {
                source_version_id: sv_id,
                node_id: doc_node,
                kind: DocumentKind::Markdown,
                source_url: None,
                published_url: None,
                source_path: &format!("doc-{i}.md"),
                language: Some("en"),
                content_hash: &format!("doc-hash-{i}"),
                source_modified_at: None,
                frontmatter: None,
                provenance: &Provenance::default(),
                package_id: None,
                char_count: 100,
                token_count: 20,
            },
        )
        .await
        .unwrap();
        let chunk_node =
            node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, &format!("chunk-{i}"), 0)
                .await
                .unwrap();
        let chunk_id = chunk::insert(
            pool,
            chunk::NewChunk {
                source_version_id: sv_id,
                document_id: doc_id,
                node_id: chunk_node,
                chunk_index: 0,
                total_chunks: 1,
                content: &format!("This is chunk {i}"),
                content_hash: &format!("chunk-hash-{i}"),
                embedding: None,
                embedding_model_id: model_id,
                heading_path: &[],
                symbol_path: &[],
                start_byte: 0,
                end_byte: 100,
                token_count: 20,
                status: ChunkStatus::EmbedFailed,
            },
        )
        .await
        .unwrap();
        chunk_ids.push(chunk_id);
    }
    (sv_id, model_id, chunk_ids)
}

async fn count_with_status(pool: &PgPool, sv_id: Uuid, status: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM chunk WHERE source_version_id = $1 AND status = $2",
    )
    .bind(sv_id)
    .bind(status)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn embed_once_promotes_embed_failed_to_ready() {
    let h = common::boot().await;
    let (sv_id, model_id, _) = seed_embed_failed_chunks(&h.pool, 5).await;

    let calls = Arc::new(AtomicUsize::new(0));
    let emb = ConstantEmbedder {
        dim: 768,
        fill: 0.25,
        calls: Arc::clone(&calls),
    };
    let promoted = embed_once(&h.pool, &emb, model_id, 16).await.unwrap();
    assert_eq!(promoted, 5);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "single batched embed call");
    assert_eq!(count_with_status(&h.pool, sv_id, "ready").await, 5);
    assert_eq!(count_with_status(&h.pool, sv_id, "embed_failed").await, 0);
}

#[tokio::test]
async fn embed_once_respects_batch_size() {
    let h = common::boot().await;
    let (_, model_id, _) = seed_embed_failed_chunks(&h.pool, 5).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let emb = ConstantEmbedder {
        dim: 768,
        fill: 0.1,
        calls: Arc::clone(&calls),
    };
    let promoted = embed_once(&h.pool, &emb, model_id, 2).await.unwrap();
    assert_eq!(promoted, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn embed_once_skips_chunks_with_different_model() {
    let h = common::boot().await;
    let (_, _seeded_model, _) = seed_embed_failed_chunks(&h.pool, 3).await;
    // Register a second, unrelated model and ask the worker to process it —
    // there are no embed_failed chunks under it so promoted = 0.
    let other_name = format!("test-other-{}", Uuid::new_v4());
    let other_model = embedding_model::upsert(&h.pool, &other_name, 1, 768, "test")
        .await
        .unwrap();
    let emb = ConstantEmbedder {
        dim: 768,
        fill: 0.1,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let promoted = embed_once(&h.pool, &emb, other_model, 16).await.unwrap();
    assert_eq!(promoted, 0);
}

#[tokio::test]
async fn empty_batch_returns_zero_without_calling_embedder() {
    let h = common::boot().await;
    // Use a fresh model id so there are guaranteed to be no chunks under it
    // (the shared CI Postgres has rows from other tests under the canonical
    // bge-base model).
    let model_name = format!("test-empty-{}", Uuid::new_v4());
    let model_id = embedding_model::upsert(&h.pool, &model_name, 1, 768, "test")
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let emb = ConstantEmbedder {
        dim: 768,
        fill: 0.0,
        calls: Arc::clone(&calls),
    };
    let promoted = embed_once(&h.pool, &emb, model_id, 16).await.unwrap();
    assert_eq!(promoted, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn embedder_failure_leaves_rows_embed_failed_for_retry() {
    let h = common::boot().await;
    let (sv_id, model_id, _) = seed_embed_failed_chunks(&h.pool, 3).await;
    let emb = FailingEmbedder;
    let promoted = embed_once(&h.pool, &emb, model_id, 16).await.unwrap();
    assert_eq!(promoted, 0);
    assert_eq!(count_with_status(&h.pool, sv_id, "embed_failed").await, 3);
}

#[tokio::test]
async fn embedder_size_mismatch_drops_batch_without_promotion() {
    let h = common::boot().await;
    let (sv_id, model_id, _) = seed_embed_failed_chunks(&h.pool, 3).await;
    let emb = WrongSizedEmbedder;
    let promoted = embed_once(&h.pool, &emb, model_id, 16).await.unwrap();
    assert_eq!(promoted, 0);
    assert_eq!(count_with_status(&h.pool, sv_id, "embed_failed").await, 3);
}

#[tokio::test]
async fn promoted_chunks_are_visible_to_search_path() {
    let h = common::boot().await;
    let (sv_id, model_id, chunk_ids) = seed_embed_failed_chunks(&h.pool, 1).await;
    // Promote the SV active so search would touch it (the search route filters
    // by is_active; we're not running search here — just confirming the
    // chunk became searchable per the embed_failed exclusion test).
    source_version::finalize(&h.pool, sv_id).await.unwrap();

    let emb = ConstantEmbedder {
        dim: 768,
        fill: 0.3,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let promoted = embed_once(&h.pool, &emb, model_id, 16).await.unwrap();
    assert_eq!(promoted, 1);

    // The promoted chunk MUST now be visible via get_by_id_ready (which the
    // /v1/chunks/:id route uses). Previously it was hidden.
    let _ = chunk::get_by_id_ready(&h.pool, chunk_ids[0])
        .await
        .expect("promoted chunk must be visible to readers");
}

// Confirms the trait object is `Send + Sync` so `Arc<dyn EmbedFn>` works
// in the spawn loop. Compile-time check; intentionally has no body.
fn _assert_send_sync() {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Arc<dyn EmbedFn>>();
    // Suppress unused-import warning under cfg(test).
    let _: Option<Pin<Box<dyn std::future::Future<Output = ()>>>> = None;
}

//! Embedder worker — fills `chunk.embedding` for chunks left in
//! `embed_failed` state by the ingest pipeline (Phase 11a / FR-038).
//!
//! The ingest write protocol inserts new chunks in `embed_failed` because
//! the CLI doesn't run the embedder. This background task polls for those
//! rows, encodes their content via [`mn_embedding::embedder`], writes the
//! vector back, and flips the status to `ready` so search starts including
//! the chunk. Carried-forward chunks already have a valid embedding; this
//! worker leaves them alone.
//!
//! The embed function is injected so tests can drive the worker without
//! loading the ~100 MB ONNX bundle. Production wires the real
//! [`mn_embedding::embedder::Embedder`] in [`spawn`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use mn_store::entities::chunk::{self, EmbedFailedChunk};
use sqlx::PgPool;
use uuid::Uuid;

/// Default poll interval — short enough that a fresh ingest's chunks are
/// queryable within a minute, long enough that an idle server doesn't burn
/// cycles. Override via `ServerConfig::embedder_interval_ms`.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

/// Default batch size. The embedder is most efficient on batches of 8-32
/// items; 16 is a safe middle.
pub const DEFAULT_BATCH_SIZE: i64 = 16;

/// Type alias for the boxed future returned by [`EmbedFn::embed`].
pub type EmbedFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>, String>> + Send + 'a>>;

/// One-call abstraction over the local embedder. Production wraps
/// [`mn_embedding::embedder::Embedder`]; tests inject canned vectors so the
/// ONNX runtime never has to load.
///
/// Uses a hand-rolled futures-based shape (not `async fn in trait`) so the
/// trait object remains `dyn`-safe — required because we pass it through
/// `Arc<dyn EmbedFn>` to the spawn loop.
pub trait EmbedFn: Send + Sync {
    /// Encode `texts` into vectors. Implementations MUST return one vector
    /// per input text in the same order; length mismatch is treated as an
    /// embed failure for the whole batch.
    fn embed(&self, texts: Vec<String>) -> EmbedFuture<'_>;
}

/// One pass through the worker: pull a batch, embed, write back.
///
/// Returns the number of chunks promoted to `ready` this pass. A return of
/// 0 means the worker found nothing to do — the spawn loop uses that as a
/// signal to wait the full interval before retrying.
///
/// Errors are surfaced for the spawn loop to log; the worker does not crash
/// the process on one bad batch.
///
/// # Errors
///
/// Returns the underlying store error on database failure. Embed-call
/// failures are caught internally, logged, and counted as a no-op for the
/// affected rows.
pub async fn embed_once(
    pool: &PgPool,
    embed_fn: &dyn EmbedFn,
    model_id: Uuid,
    batch_size: i64,
) -> Result<usize, mn_store::StoreError> {
    embed_once_scoped(pool, embed_fn, model_id, None, batch_size).await
}

/// Variant of [`embed_once`] with an optional `source_version_id` filter —
/// intended for integration tests against a shared CI Postgres where
/// concurrent sibling tests would otherwise pollute the batch.
///
/// # Errors
///
/// Same as [`embed_once`].
pub async fn embed_once_scoped(
    pool: &PgPool,
    embed_fn: &dyn EmbedFn,
    model_id: Uuid,
    source_version_filter: Option<Uuid>,
    batch_size: i64,
) -> Result<usize, mn_store::StoreError> {
    let batch =
        chunk::list_embed_failed_batch(pool, model_id, source_version_filter, batch_size).await?;
    if batch.is_empty() {
        return Ok(0);
    }
    let texts: Vec<String> = batch.iter().map(|c| c.content.clone()).collect();
    let vectors = match embed_fn.embed(texts).await {
        Ok(v) => v,
        Err(msg) => {
            tracing::warn!(error = %msg, batch_size = batch.len(), "embedder failed for batch");
            return Ok(0);
        }
    };
    if vectors.len() != batch.len() {
        tracing::warn!(
            requested = batch.len(),
            returned = vectors.len(),
            "embedder returned wrong-sized result; dropping batch",
        );
        return Ok(0);
    }

    let mut promoted = 0_usize;
    for (EmbedFailedChunk { id, .. }, vector) in batch.into_iter().zip(vectors.into_iter()) {
        match chunk::set_embedding(pool, id, vector).await {
            Ok(true) => promoted += 1,
            Ok(false) => {
                // Row state changed under us; let the next pass handle it.
            }
            Err(e) => {
                tracing::warn!(chunk_id = %id, error = %e, "set_embedding failed");
            }
        }
    }
    Ok(promoted)
}

/// Spawn the periodic embedder task. Returns a `JoinHandle` the caller
/// keeps alive for the duration of the server.
///
/// The first tick fires immediately so a newly-started server picks up any
/// chunks left over from a previous boot.
#[must_use]
pub fn spawn(
    pool: PgPool,
    embed_fn: Arc<dyn EmbedFn>,
    model_id: Uuid,
    interval: Duration,
    batch_size: i64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        loop {
            tick.tick().await;
            match embed_once(&pool, embed_fn.as_ref(), model_id, batch_size).await {
                Ok(promoted) => {
                    if promoted > 0 {
                        tracing::info!(promoted, "embedder worker promoted chunks");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "embedder worker tick failed; will retry");
                }
            }
        }
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ConstantEmbedder {
        dim: usize,
    }

    impl EmbedFn for ConstantEmbedder {
        fn embed(&self, texts: Vec<String>) -> EmbedFuture<'_> {
            let dim = self.dim;
            Box::pin(async move { Ok(texts.iter().map(|_| vec![0.5_f32; dim]).collect()) })
        }
    }

    #[tokio::test]
    async fn constant_embedder_returns_one_vector_per_text() {
        let e = ConstantEmbedder { dim: 4 };
        let v = e
            .embed(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()])
            .await
            .unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].len(), 4);
    }

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
}

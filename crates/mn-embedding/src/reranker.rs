//! `bge-reranker-base` cross-encoder wrapper (D2).
//!
//! Lazy singleton behind a `OnceCell`. The reranker is used MCP-side only; the
//! cloud server never sees a reranker invocation. It is the only fastembed
//! model left in the corpus path — the embedder is now VoyageAI (remote).

use std::path::PathBuf;
use std::sync::Arc;

use fastembed::{RerankInitOptions, RerankerModel, TextRerank};
use tokio::sync::OnceCell;

use crate::error::{EmbeddingError, Result};

/// Canonical wire name for the v1 reranker.
pub const MODEL_NAME: &str = "bge-reranker-base";

/// Reranker handle. Cheap to clone; the heavy `TextRerank` lives behind an
/// `Arc` and is initialized once.
#[derive(Clone)]
pub struct Reranker {
    inner: Arc<TextRerank>,
}

impl std::fmt::Debug for Reranker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reranker")
            .field("model", &MODEL_NAME)
            .finish()
    }
}

/// One reranker result. Higher `score` means more relevant.
#[derive(Debug, Clone, PartialEq)]
pub struct RerankResult {
    /// The input document index this result corresponds to.
    pub index: usize,
    /// The cross-encoder relevance logit (not normalized).
    pub score: f32,
}

impl Reranker {
    /// Build a new reranker. First call downloads ~270 MB.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Init`] if fastembed fails to instantiate the
    /// model (network failure, corrupted cache, etc.).
    pub fn try_new(cache_dir: PathBuf) -> Result<Self> {
        let opts = RerankInitOptions::new(RerankerModel::BGERerankerBase).with_cache_dir(cache_dir);
        let model = TextRerank::try_new(opts).map_err(|e| EmbeddingError::Init {
            model: MODEL_NAME.to_owned(),
            message: e.to_string(),
        })?;
        Ok(Self { inner: Arc::new(model) })
    }

    /// Rerank `documents` against `query`. Returns one [`RerankResult`] per
    /// input document in the input order (not sorted — callers usually sort
    /// by score descending and take the top K).
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Inference`] on tokenizer or ONNX runtime
    /// failure.
    pub fn rerank(
        &self,
        query: &str,
        documents: &[String],
        batch_size: Option<usize>,
    ) -> Result<Vec<RerankResult>> {
        let doc_refs: Vec<&str> = documents.iter().map(String::as_str).collect();
        let results = self
            .inner
            .rerank(query, doc_refs, false, batch_size)
            .map_err(|e| EmbeddingError::Inference {
                model: MODEL_NAME.to_owned(),
                message: e.to_string(),
            })?;
        Ok(results
            .into_iter()
            .map(|r| RerankResult { index: r.index, score: r.score })
            .collect())
    }

    /// Async-friendly variant of [`Reranker::rerank`] that offloads the
    /// CPU-bound cross-encoder inference to a blocking thread.
    ///
    /// # Errors
    ///
    /// Same as [`Reranker::rerank`].
    pub async fn rerank_blocking(
        &self,
        query: String,
        documents: Vec<String>,
        batch_size: Option<usize>,
    ) -> Result<Vec<RerankResult>> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.rerank(&query, &documents, batch_size))
            .await
            .map_err(|e| EmbeddingError::Inference {
                model: MODEL_NAME.to_owned(),
                message: format!("blocking task failed: {e}"),
            })?
    }
}

/// Process-wide lazy singleton. First call loads the model; concurrent callers
/// all wait on the same `OnceCell::get_or_try_init` future.
static GLOBAL: OnceCell<Reranker> = OnceCell::const_new();

/// Get the process-wide reranker, initializing on first call.
///
/// # Errors
///
/// See [`Reranker::try_new`].
pub async fn global(cache_dir: PathBuf) -> Result<Reranker> {
    GLOBAL
        .get_or_try_init(|| async move {
            tokio::task::spawn_blocking(move || Reranker::try_new(cache_dir))
                .await
                .map_err(|e| EmbeddingError::Init {
                    model: MODEL_NAME.to_owned(),
                    message: format!("blocking init task failed: {e}"),
                })?
        })
        .await
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerank_result_round_trips_into_top_k_select() {
        // Smoke test on the RerankResult shape: sorting by score descending
        // and taking the top-K is the canonical caller flow.
        let mut results = vec![
            RerankResult { index: 0, score: 0.1 },
            RerankResult { index: 1, score: 0.9 },
            RerankResult { index: 2, score: 0.5 },
        ];
        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        assert_eq!(results[0].index, 1);
        assert_eq!(results[1].index, 2);
        assert_eq!(results[2].index, 0);
    }
}

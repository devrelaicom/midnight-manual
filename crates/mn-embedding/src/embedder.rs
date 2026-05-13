//! `bge-base-en-v1.5` embedder wrapper (D14, R-1).
//!
//! Lazy singleton: the underlying `fastembed::TextEmbedding` is constructed
//! once via [`tokio::sync::OnceCell`] so concurrent first-callers share a
//! single ~450 MB ONNX load. Subsequent calls reuse the in-memory model.

use std::path::PathBuf;
use std::sync::Arc;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use tokio::sync::OnceCell;

use crate::error::{EmbeddingError, Result};

/// Output dimension of `bge-base-en-v1.5`.
pub const BGE_BASE_DIM: usize = 768;

/// Canonical wire name for the v1 embedder.
pub const MODEL_NAME: &str = "bge-base-en-v1.5";

/// Embedder handle. Cheap to clone; the heavy `TextEmbedding` lives behind an
/// `Arc` and is initialized once.
#[derive(Clone)]
pub struct Embedder {
    inner: Arc<TextEmbedding>,
}

impl std::fmt::Debug for Embedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Embedder")
            .field("model", &MODEL_NAME)
            .field("dim", &BGE_BASE_DIM)
            .finish()
    }
}

impl Embedder {
    /// Build a new embedder backed by `bge-base-en-v1.5`. The first call in a
    /// process downloads ~450 MB; subsequent calls reuse the cache directory.
    ///
    /// `cache_dir` is the on-disk location where ONNX model files are stored
    /// (see [`crate::cache::resolve`]).
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Init`] if fastembed fails to bootstrap the
    /// model (typically network failure during the first download, or a
    /// corrupted cache).
    pub fn try_new(cache_dir: PathBuf) -> Result<Self> {
        let opts = InitOptions::new(EmbeddingModel::BGEBaseENV15).with_cache_dir(cache_dir);
        let model = TextEmbedding::try_new(opts).map_err(|e| EmbeddingError::Init {
            model: MODEL_NAME.to_owned(),
            message: e.to_string(),
        })?;
        Ok(Self { inner: Arc::new(model) })
    }

    /// Embed a batch of texts. Returns one 768-dim `Vec<f32>` per input.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Inference`] on tokenizer or ONNX runtime
    /// failure, or [`EmbeddingError::DimensionMismatch`] if the model returned
    /// vectors of unexpected dimension (defense in depth).
    pub fn embed(&self, texts: &[String], batch_size: Option<usize>) -> Result<Vec<Vec<f32>>> {
        let vectors = self.inner.embed(texts.to_vec(), batch_size).map_err(|e| {
            EmbeddingError::Inference {
                model: MODEL_NAME.to_owned(),
                message: e.to_string(),
            }
        })?;
        for v in &vectors {
            if v.len() != BGE_BASE_DIM {
                return Err(EmbeddingError::DimensionMismatch {
                    got: v.len(),
                    expected: BGE_BASE_DIM,
                });
            }
        }
        Ok(vectors)
    }

    /// Convenience for embedding a single text.
    ///
    /// # Errors
    ///
    /// See [`Embedder::embed`].
    pub fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embed(&[text.to_owned()], None)?;
        Ok(v.pop().unwrap_or_default())
    }
}

/// Process-wide lazy singleton for the embedder. First call loads the model;
/// concurrent callers all wait on the same `OnceCell::get_or_try_init` future.
///
/// MCP servers and CLI commands obtain their embedder handle through
/// [`global`], not [`Embedder::try_new`], so a long-running process never
/// double-loads.
static GLOBAL: OnceCell<Embedder> = OnceCell::const_new();

/// Get the process-wide embedder, initializing on first call.
///
/// # Errors
///
/// Returns whatever [`Embedder::try_new`] returns. After a successful init,
/// subsequent calls always return the cached handle without retrying.
pub async fn global(cache_dir: PathBuf) -> Result<Embedder> {
    GLOBAL
        .get_or_try_init(|| async move { Embedder::try_new(cache_dir) })
        .await
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_corpus_schema() {
        // The Phase-2 schema migrations seeded bge-base-en-v1.5@1 with dim=768.
        // If these constants drift, server-side searches will start failing
        // the embedding_model trigger (EC-10).
        assert_eq!(BGE_BASE_DIM, 768);
        assert_eq!(MODEL_NAME, "bge-base-en-v1.5");
    }
}

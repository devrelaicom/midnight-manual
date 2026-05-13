//! Typed errors for the embedder + reranker.

use thiserror::Error;

/// All the ways embedding / reranking can fail.
#[derive(Debug, Error)]
pub enum EmbeddingError {
    /// The cache directory could not be resolved (no `HOME` or `XDG_DATA_HOME`).
    #[error(
        "could not resolve model cache directory; set MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME"
    )]
    NoCacheDir,
    /// fastembed failed to initialize the model (download error, ONNX runtime
    /// failure, missing files, etc.).
    #[error("failed to initialize model `{model}`: {message}")]
    Init {
        /// The model identifier we were trying to instantiate.
        model: String,
        /// The underlying fastembed error message.
        message: String,
    },
    /// fastembed returned an error during inference (batch encode or rerank).
    #[error("inference failed for model `{model}`: {message}")]
    Inference {
        /// The model that failed.
        model: String,
        /// The underlying fastembed error message.
        message: String,
    },
    /// Output dimension didn't match what we expected.
    #[error("unexpected embedding dimension: got {got}, expected {expected}")]
    DimensionMismatch {
        /// What we actually got.
        got: usize,
        /// What we expected.
        expected: usize,
    },
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, EmbeddingError>;

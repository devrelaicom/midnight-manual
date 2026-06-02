//! `mn-embedding` — fastembed-rs wrapper for the bge-base-en-v1.5 embedder and
//! the bge-reranker-base cross-encoder.
//!
//! Phase 3b lands the wrappers + lazy `tokio::sync::OnceCell` singletons.
//! Actual model-load tests are gated behind the `models` feature so default
//! CI doesn't have to download ~700 MB of ONNX files on every run.

#![doc(html_root_url = "https://docs.rs/mn-embedding/0.1.0")]
#![allow(clippy::doc_markdown, clippy::useless_vec)]

pub mod cache;
pub mod client;
pub mod embedder;
pub mod error;
pub mod reranker;
pub mod voyage;

pub use embedder::{Embedder, BGE_BASE_DIM, MODEL_NAME as EMBEDDER_MODEL_NAME};
pub use error::{EmbeddingError, Result};
pub use reranker::{RerankResult, Reranker, MODEL_NAME as RERANKER_MODEL_NAME};

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

//! `mn-embedding` — VoyageAI embedding client plus the fastembed-rs
//! `bge-reranker-base` cross-encoder.
//!
//! The corpus is embedded with VoyageAI (client-side BYOK or via the cloud
//! server's `/v1/embeddings` proxy); see [`voyage`] and [`client`]. The local
//! fastembed dependency now backs only the [`reranker`] — a lazy
//! `tokio::sync::OnceCell` singleton whose model-load is exercised by the
//! opt-in reranker smoke test rather than on every CI run.

#![doc(html_root_url = "https://docs.rs/mn-embedding/0.1.0")]
#![allow(clippy::doc_markdown, clippy::useless_vec)]

pub mod cache;
pub mod client;
pub mod error;
pub mod reranker;
pub mod reranker_catalog;
pub mod voyage;

pub use error::{EmbeddingError, Result};
pub use reranker::{RerankResult, Reranker, MODEL_NAME as RERANKER_MODEL_NAME};

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

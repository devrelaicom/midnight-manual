//! `mn-embedding` — VoyageAI embedding + reranking client.
//!
//! The corpus is embedded with VoyageAI (client-side BYOK or via the cloud
//! server's `/v1/embeddings` proxy); see [`voyage`] and [`client`]. Reranking is
//! also VoyageAI (server inline or client BYOK) — the old local cross-encoder
//! subsystem was removed (design doc §5), and [`reranker`] now holds only the
//! shared [`RerankResult`] type.

#![doc(html_root_url = "https://docs.rs/mn-embedding/0.1.0")]
#![allow(clippy::doc_markdown, clippy::useless_vec)]

pub mod cache;
pub mod client;
pub mod contextualized;
pub mod error;
pub mod reranker;
pub mod voyage;

pub use error::{EmbeddingError, Result};
pub use reranker::RerankResult;

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

//! `mn-embedding` — fastembed-rs wrapper for embedder and cross-encoder reranker.
//!
//! Phase-3 (US2) lands the embedder for `bge-base-en-v1.5`. Phase-5 (US5) adds the
//! `bge-reranker-base` cross-encoder used by the local MCP server. See
//! [`specs/001-rag-platform/tasks.md`](../../../specs/001-rag-platform/tasks.md).

#![doc(html_root_url = "https://docs.rs/mn-embedding/0.1.0")]

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

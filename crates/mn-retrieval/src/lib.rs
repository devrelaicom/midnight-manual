//! `mn-retrieval` — hybrid query construction, RRF merging, and confidence scoring.
//!
//! Phase-4 (US4) lands FTS + pgvector queries and the RRF (k=60) merger.
//! Phase-9 (US6) lands the trust × relevance confidence model. See
//! [`specs/001-rag-platform/tasks.md`](../../../specs/001-rag-platform/tasks.md).

#![doc(html_root_url = "https://docs.rs/mn-retrieval/0.1.0")]

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

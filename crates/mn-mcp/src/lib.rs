//! `mn-mcp` — MCP JSON-RPC server (stdio framing) + the seven retrieval tools.
//!
//! Phase 5b landed the transport, protocol types, server loop, and two tools
//! (`status`, `pull_models`). Phase 5c lands the remaining five — `search`,
//! `get_chunk`, `get_chunk_siblings`, `get_chunk_parents`, `list_sources` —
//! by wiring in an HTTP client to the cloud server and using the local
//! embedder + reranker for the search path.

#![doc(html_root_url = "https://docs.rs/mn-mcp/0.1.0")]
#![allow(
    clippy::doc_markdown,
    clippy::manual_let_else,
    clippy::manual_pattern_char_comparison,
    clippy::too_long_first_doc_paragraph
)]

pub mod cloud_client;
pub mod protocol;
pub mod server;
pub mod tools;
pub mod transport;

pub use cloud_client::{CloudClient, CloudError};
pub use server::{run, ServerConfig};

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

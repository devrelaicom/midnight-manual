//! `mn-mcp` — MCP JSON-RPC server (stdio framing) + the retrieval tool surface.
//!
//! Eleven tools across three categories:
//!
//! - Local-only: `status`, `pull_models`.
//! - Cloud search: `search` (embeds locally, posts to `/v1/search`, optional
//!   local rerank).
//! - Cloud reads (pass-through): `get_chunk`, `get_chunk_next`,
//!   `get_chunk_prev`, `get_chunk_parents`, `get_document`,
//!   `get_document_full`, `get_document_chunks`, `list_sources`.

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

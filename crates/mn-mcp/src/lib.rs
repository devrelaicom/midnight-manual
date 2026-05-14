//! `mn-mcp` — MCP JSON-RPC server (stdio framing) + the seven retrieval tools.
//!
//! Phase 5b lands the transport, protocol types, server loop, and two tools
//! (`status`, `pull_models`). The full search / chunk / sources tool surface
//! lands in follow-up PRs once the cloud HTTP client is wired in.

#![doc(html_root_url = "https://docs.rs/mn-mcp/0.1.0")]
#![allow(
    clippy::doc_markdown,
    clippy::manual_let_else,
    clippy::manual_pattern_char_comparison,
    clippy::too_long_first_doc_paragraph
)]

pub mod protocol;
pub mod server;
pub mod tools;
pub mod transport;

pub use server::{run, ServerConfig};

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

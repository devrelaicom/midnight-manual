//! `mn-mcp` — MCP JSON-RPC server (stdio framing) and the seven retrieval tools.
//!
//! Phase-5 (US5) lands the transport, server loop, lazy ML model load, and tool
//! implementations. See [`specs/001-rag-platform/tasks.md`](../../../specs/001-rag-platform/tasks.md).

#![doc(html_root_url = "https://docs.rs/mn-mcp/0.1.0")]

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

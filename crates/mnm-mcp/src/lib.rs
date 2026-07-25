//! `mnm-mcp` — MCP JSON-RPC server (stdio framing and stateless Streamable
//! HTTP) + the retrieval tool surface.
//!
//! Two transports over one message-handling core: newline-delimited stdio
//! ([`run`]) and Streamable HTTP ([`run_http`] — `POST /mcp` + `GET /healthz`,
//! loopback-guarded by default).
//!
//! Thirteen tools across four categories:
//!
//! - Diagnostics: `status` (concurrent probes — cloud `/readyz`, `/v1/me`
//!   auth + rate/token limits, VoyageAI key validity, local reranker state).
//! - Cloud search: `search` + `advanced_search` (embed locally, post to
//!   `/v1/search`, optional local rerank; `advanced_search` adds multi-query
//!   fusion, facet filters, and the rerank toggle).
//! - Cloud reads + discovery (pass-through): `get_chunks`, `get_chunk_next`,
//!   `get_chunk_prev`, `get_chunk_neighbors`, `get_chunk_parents`,
//!   `get_document`, `get_document_chunks`, `list_sources`, `facets`.
//! - Local install: `install_skill` (writes bundled skills' `SKILL.md` into the
//!   user's AI harness(es); a `skill` enum selects bundles, omit = all).

// The `advanced_search` tool's `input_schema` is one deeply nested
// `serde_json::json!` literal (typed per-facet `filters`), so its macro
// expansion exceeds the default 128-frame recursion limit.
#![recursion_limit = "256"]
#![doc(html_root_url = "https://docs.rs/mnm-mcp/0.1.0")]
#![allow(
    clippy::doc_markdown,
    clippy::manual_let_else,
    clippy::manual_pattern_char_comparison,
    clippy::too_long_first_doc_paragraph
)]

pub mod cloud_client;
pub mod http;
pub mod prompts;
pub mod protocol;
pub mod render;
pub mod schemas;
pub mod server;
pub mod status;
pub mod tools;
pub mod transport;

pub use cloud_client::{CloudClient, CloudError};
pub use http::run_http;
pub use server::{run, ServerConfig};

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

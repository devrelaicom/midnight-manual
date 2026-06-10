//! `mn-server` — cloud HTTP server library + binary.
//!
//! The library half exposes [`app::build`] for end-to-end tests; the binary
//! half (`src/main.rs`) wires logging + DB + listener.

#![doc(html_root_url = "https://docs.rs/mn-server/0.1.0")]
// Several stylistic lints are too noisy for an HTTP server scaffold;
// re-enabled selectively once the routes mature.
#![allow(
    clippy::double_must_use,
    clippy::doc_markdown,
    clippy::too_long_first_doc_paragraph,
    clippy::needless_pass_by_value,
    clippy::redundant_clone,
    clippy::match_same_arms
)]

pub mod app;
pub mod code_model;
pub mod config;
pub mod corpus_model;
pub mod error;
pub mod jobs;
pub mod middleware;
pub mod ratelimit;
pub mod routes;
pub mod tokenlimit;
pub mod tokenlimit_override;

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

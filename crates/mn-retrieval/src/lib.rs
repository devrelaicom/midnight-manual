//! `mn-retrieval` — hybrid (FTS + vector) query construction, RRF merging, and confidence scoring.
//!
//! Phase-4 lands the pure-logic core:
//! - [`rrf`] — Reciprocal Rank Fusion with the canonical k=60 constant.
//! - [`filters`] — typed shape for `POST /v1/search` filters with serde JSON round-tripping.
//!
//! The actual hybrid SQL (FTS + pgvector) lives next door in mn-server's query
//! handlers; this crate keeps the pure functions on their own so they can be
//! benched and proptested without a database.

#![doc(html_root_url = "https://docs.rs/mn-retrieval/0.1.0")]

pub mod filters;
pub mod rrf;

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

//! `mn-content` — Markdown / code chunking and package detection for midnight-manual.
//!
//! Phase-3 lands the Markdown side: heading-based chunker with fallback windowing,
//! frontmatter parser, manifest loader, content-hash. The tree-sitter code
//! chunkers and Compact module scanner land in Phase 6.

#![doc(html_root_url = "https://docs.rs/mn-content/0.1.0")]

pub mod chunk;
pub mod code;
pub mod content_hash;
pub mod frontmatter;
pub mod ingest;
pub mod language;
pub mod manifest;
pub mod markdown;
pub mod tokens;

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

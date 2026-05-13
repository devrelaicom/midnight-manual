//! `mn-content` — Markdown / code chunking and package detection for midnight-manual.
//!
//! Phase-3 (US2) lands the Markdown chunker, frontmatter parser, and manifest loader.
//! Phase-6 (US3) lands the tree-sitter code chunkers and Compact module scanner.
//! See [`specs/001-rag-platform/tasks.md`](../../../specs/001-rag-platform/tasks.md).

#![doc(html_root_url = "https://docs.rs/mn-content/0.1.0")]

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

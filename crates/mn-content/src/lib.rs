//! `mn-content` — Markdown / code chunking and package detection for midnight-manual.
//!
//! Markdown (heading-based), code (tree-sitter per language; Compact via the
//! `compactp` parser behind the `compact` feature), and a line-window fallback,
//! plus frontmatter, manifest loading, content-hash, and package detection.

#![doc(html_root_url = "https://docs.rs/mn-content/0.1.0")]

pub mod chunk;
pub mod code;
pub mod content_hash;
pub mod context_group;
pub mod extract;
pub mod frontmatter;
pub mod ingest;
pub mod language;
pub mod manifest;
pub mod markdown;
pub mod package;
pub mod tokens;

/// Detect Compact module-based package membership from file contents.
///
/// Returns `None` when the `compact` feature is disabled, or when the file
/// declares zero or multiple top-level modules (see [`code::compact`]).
// When the `compact` feature is off, the body reduces to `None` and clippy
// flags this as const-able; with the feature on it calls a non-const parser,
// so it cannot be `const`. Allow the feature-gated false positive.
#[allow(clippy::missing_const_for_fn)]
#[must_use]
pub fn detect_compact_package(body: &str) -> Option<mn_core::types::PackageRef> {
    #[cfg(feature = "compact")]
    {
        crate::code::compact::detect_module_package(body)
    }
    #[cfg(not(feature = "compact"))]
    {
        let _ = body;
        None
    }
}

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

//! `mnm-content` — Markdown / code chunking and package detection for midnight-manual.
//!
//! Markdown (heading-based), code (tree-sitter per language; Compact via the
//! `compactp` parser behind the `compact` feature), and a line-window fallback,
//! plus frontmatter, manifest loading, content-hash, and package detection.

#![doc(html_root_url = "https://docs.rs/mnm-content/0.1.0")]

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
pub mod preprocess;
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
pub fn detect_compact_package(body: &str) -> Option<mnm_core::types::PackageRef> {
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

/// Detect the Compact language-version constraint declared by a file's
/// `pragma language_version` (spec §1.1).
///
/// Returns the normalised constraint expression (e.g. `">=0.23"`) or `None`
/// when the `compact` feature is disabled, or when the file declares no
/// `language_version` pragma (see [`code::compact::detect_language_version`]).
// With the `compact` feature off the body reduces to `None`, which clippy
// would flag as const-able; with the feature on it calls a non-const parser,
// so it cannot be `const`. Allow the feature-gated false positive.
#[allow(clippy::missing_const_for_fn)]
#[must_use]
pub fn detect_language_version(body: &str) -> Option<String> {
    #[cfg(feature = "compact")]
    {
        crate::code::compact::detect_language_version(body)
    }
    #[cfg(not(feature = "compact"))]
    {
        let _ = body;
        None
    }
}

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

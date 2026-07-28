//! Pre-chunk preprocessing: strips boilerplate noise (license headers,
//! decorative comment banners, MDX plumbing, badges) and detects generated
//! files, BEFORE chunking/embedding.
//!
//! Spec: docs/superpowers/specs/2026-07-27-preprocess-license-design.md

pub mod comment_syntax;
pub mod lexer;

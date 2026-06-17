//! Entity modules — one per table group in the data-model schema (corpus tables §0002, admin tables §0004).
//!
//! Doc-markdown lint allowed crate-wide for these modules because their docs
//! describe SQL schema concepts using snake_case identifiers in prose; wrapping
//! every column name in backticks would harm readability without adding signal.

#![allow(clippy::doc_markdown)]

pub mod chunk;
pub mod document;
pub mod embedding_model;
pub mod node;
pub mod package;
pub mod rate_limit_override;
pub mod source;
pub mod source_version;
pub mod token_limit_override;

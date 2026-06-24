//! `mnm-core` — shared primitives for the midnight-manual workspace.
//!
//! Phase 2 lands the foundational shared types: typed error envelope
//! ([`error`]), embedding-model wire id ([`model_id`]), provenance / content
//! / source / chunk types ([`provenance`], [`types`]), XDG config discovery
//! ([`config`]), auth-file reader ([`auth_file`]), and the scoring-policy
//! loader ([`scoring_policy`]). Each module is independently usable and
//! independently testable.

#![doc(html_root_url = "https://docs.rs/mnm-core/0.1.0")]

pub mod auth_file;
pub mod config;
pub mod embedder_identity;
pub mod error;
pub mod ingest;
pub mod injection;
pub mod introspect;
pub mod limits;
pub mod model_id;
pub mod paths;
pub mod provenance;
pub mod rerank;
pub mod scoring;
pub mod scoring_policy;
pub mod types;
pub mod version_match;

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

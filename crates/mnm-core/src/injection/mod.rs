//! Shared prompt-injection protection primitives (issue #103).
//!
//! This module hosts the two layers of the prompt-injection defense, both pure
//! and shared across the workspace's server, CLI, and MCP surfaces:
//!
//! 1. **Server-side scoring** (ingest time). [`normalize`] de-obfuscates
//!    untrusted text, [`pattern::detect`] runs a curated literal+regex ruleset
//!    over it to produce a risk score, and [`policy::InjectionPolicy`] blends
//!    that pattern score with an optional model score and decides whether to
//!    reject the content (and which source tiers warrant the model pass).
//!
//! 2. **Client-side guarding** (response time). [`security::SecurityLevel`]
//!    decides, per source attribution and verification status, whether
//!    server-returned content is wrapped in a nonce-tagged untrusted block via
//!    [`security::wrap_untrusted`] before the model sees it — and at the
//!    strictest level, whether flagged content is removed.
//!
//! Keeping both layers here lets the server and clients agree byte-for-byte on
//! normalization, the ruleset, and the wrapping format.

pub mod normalize;
pub mod pattern;
pub mod policy;
pub mod report;
pub mod security;

pub use normalize::{normalize, Normalized};
pub use pattern::{detect, PatternMatch, PatternResult, Technique};
pub use policy::{FailMode, InjectionPolicy, InjectionPolicyError, SCHEMA_VERSION};
pub use report::{FlaggedWindow, ModelReport, ScanReport, Verdict};
pub use security::{new_nonce, untrusted_inner, wrap_untrusted, SecurityLevel};

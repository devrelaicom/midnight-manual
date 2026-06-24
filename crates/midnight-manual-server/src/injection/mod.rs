//! Server-side prompt-injection protection (issue #103, server half).
//!
//! Two submodules carry the ingest-time scan:
//!
//! - [`model_client`] — the hosted Hugging Face text-classification client
//!   (Llama-Prompt-Guard-2) that scores ≤512-token windows of untrusted text.
//! - [`scan`] — the [`scan::InjectionState`] that combines the pure pattern
//!   detector (from `mnm-core`) with the optional model leg and applies the
//!   [`mnm_core::injection::InjectionPolicy`] to produce an accept/reject
//!   verdict.
//!
//! The pure detector ruleset, the policy shape, and the report wire types all
//! live in `mnm-core` so the server, CLI, and MCP surfaces agree byte-for-byte.

pub mod model_client;
pub mod scan;

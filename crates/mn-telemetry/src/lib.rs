//! `mn-telemetry` — event schemas, batched flush client, and the three-mechanism opt-out resolver.
//!
//! Phase-12 (US11) lands the full implementation, including the six event types,
//! FIFO drop semantics, and the privacy canary suite. See
//! [`specs/001-rag-platform/tasks.md`](../../../specs/001-rag-platform/tasks.md).

#![doc(html_root_url = "https://docs.rs/mn-telemetry/0.1.0")]

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

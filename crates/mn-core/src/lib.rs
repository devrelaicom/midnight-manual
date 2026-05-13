//! `mn-core` — shared primitives for the midnight-manual workspace.
//!
//! See [`specs/001-rag-platform/plan.md`](../../../specs/001-rag-platform/plan.md) for the crate's
//! intended responsibilities. This is the workspace skeleton produced by `/sdd:plan` Phase 2;
//! implementation modules will land via `/sdd:tasks` → `/sdd:implement`.

#![doc(html_root_url = "https://docs.rs/mn-core/0.1.0")]

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

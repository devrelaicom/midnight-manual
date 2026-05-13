//! `mn-store` — Postgres + pgvector storage for midnight-manual.
//!
//! See [`specs/001-rag-platform/data-model.md`](../../../specs/001-rag-platform/data-model.md)
//! for schema details. Entity modules, queries, and migrations land in Phase 2 of
//! `specs/001-rag-platform/tasks.md`.

#![doc(html_root_url = "https://docs.rs/mn-store/0.1.0")]

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

//! `mnm-store` — Postgres + pgvector storage for midnight-manual.
//!
//! Schema lives in `crates/mnm-store/migrations/`; typed entity APIs in
//! [`entities`]; the `PgPool` builder and `sqlx::migrate!()` runner in
//! [`pool`]. Errors are mapped through [`error::StoreError`] so callers don't
//! depend on sqlx internals.
//!
//! See the data-model schema reference for schema details.

#![doc(html_root_url = "https://docs.rs/mnm-store/0.1.0")]

pub mod entities;
pub mod error;
pub mod pool;

pub use error::{Result, StoreError};
pub use pool::{connect, run_migrations, MIGRATOR};

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

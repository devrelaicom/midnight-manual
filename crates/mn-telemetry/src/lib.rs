//! `mn-telemetry` — typed event schemas, opt-out resolver, and client surface
//! for the privacy-canary-gated telemetry pipeline (US11 / FR-107..114).
//!
//! Phase 8a (this revision) lands:
//!
//! - The closed set of event types matching migration `0005`'s CHECK constraint.
//! - The three-mechanism opt-out resolver (env var, config flag, runtime toggle).
//! - The [`Client`] trait + a [`NoopClient`] default so call sites can adopt
//!   the API immediately, even though the HTTP-backed buffered client lands
//!   in Phase 8b.
//! - Top-level canary-set constants exposed via [`canary`] so canary tests
//!   (FR-112) can probe every code path with the same forbidden strings.
//!
//! What is intentionally NOT here yet (deferred to Phase 8b):
//!
//! - Buffered HTTP client with FIFO drop (FR-113).
//! - `POST /v1/telemetry` server route + validator.
//! - `mnm telemetry disable` CLI subcommand (the toggle is exposed
//!   programmatically via [`optout::set_runtime_disabled`]).

#![doc(html_root_url = "https://docs.rs/mn-telemetry/0.1.0")]
#![allow(clippy::doc_markdown)]

#[cfg(test)]
pub(crate) mod test_lock {
    //! One process-wide `Mutex` shared by every test that touches the
    //! `optout::RUNTIME_DISABLED` static. `cargo test` runs tests in
    //! parallel within a single binary; without a shared lock the toggle
    //! tests race the resolver tests and produce flaky failures.
    use std::sync::Mutex;

    pub static LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the lock, recovering from a previous panic that poisoned it.
    pub fn lock() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub mod canary;
pub mod client;
pub mod events;
pub mod optout;

pub use client::{Client, NoopClient};
pub use events::{
    CliCommandName, Component, Event, EventPayload, McpToolName, ModelState, Outcome,
};
pub use optout::{is_enabled, DISABLE_ENV_VAR, HELP_TEXT};

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

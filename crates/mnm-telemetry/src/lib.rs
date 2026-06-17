//! `mnm-telemetry` — typed event schemas, opt-out resolver, and client surface
//! for the privacy-canary-gated telemetry pipeline (US11 / FR-107..114).
//!
//! Phase 8a + 8b (this revision) lands:
//!
//! - The closed set of event types matching migration `0005`'s CHECK constraint.
//! - The three-mechanism opt-out resolver (env var, config flag, runtime toggle).
//! - The [`Client`] trait + a [`NoopClient`] default so call sites that
//!   haven't opted in yet remain ergonomic.
//! - The [`HttpClient`] buffered batching client (FR-108 / FR-113) that
//!   accumulates events in-memory and POSTs them as JSON arrays to the
//!   configured cloud endpoint with jittered exponential backoff on 5xx
//!   and network errors.
//! - The [`TelemetryClient`] boot-time handle that resolves opt-out and
//!   either spawns a real [`HttpClient`] flusher or selects a cheap
//!   `Disabled` no-op branch.
//! - Top-level canary-set constants exposed via [`canary`] so canary tests
//!   (FR-112) can probe every code path with the same forbidden strings.

#![doc(html_root_url = "https://docs.rs/mnm-telemetry/0.1.0")]
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

pub use client::{
    Client, HttpClient, HttpClientConfig, HttpClientError, NoopClient, TelemetryClient,
    DEFAULT_FLUSH_INTERVAL, DEFAULT_FLUSH_THRESHOLD, DEFAULT_REQUEST_TIMEOUT, MAX_RETRY_ATTEMPTS,
    RETRY_BUDGET,
};
pub use events::{
    CliCommandName, Component, Event, EventPayload, McpToolName, ModelState, Outcome,
};
pub use optout::{is_enabled, DISABLE_ENV_VAR, HELP_TEXT};

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

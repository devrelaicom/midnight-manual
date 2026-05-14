//! Telemetry client surface.
//!
//! [`Client`] is the trait every call site touches. Phase 8a ships
//! [`NoopClient`] as the only impl — it consults the opt-out resolver and
//! drops the event. The buffered HTTP-backed client (FR-113) lands in Phase
//! 8b; introducing the trait now lets every emit-site take a `&dyn Client`
//! and avoid a churn-PR when the real client lands.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::events::Event;
use crate::optout;

/// Single-method client surface.
///
/// `emit` MUST be cheap (lock-free fast path on the opt-out check) and MUST
/// NOT block the caller — telemetry is best-effort by definition. Any
/// implementation that opens network connections MUST do so off the caller's
/// task (FR-113).
#[async_trait]
pub trait Client: Send + Sync {
    /// Emit one event. The client is responsible for honouring the opt-out
    /// resolver (FR-108); failures are silently dropped on the floor.
    async fn emit(&self, event: Event);

    /// Total events the client has accepted for emission (after opt-out
    /// filtering). Exposed for canary-test and counter assertions.
    fn accepted_count(&self) -> u64 {
        0
    }

    /// Total events the opt-out resolver rejected.
    fn dropped_by_optout(&self) -> u64 {
        0
    }
}

/// Null implementation — counts events, honours the opt-out resolver, but
/// never actually sends them anywhere. Default for every component until the
/// buffered HTTP client lands.
#[derive(Debug, Default)]
pub struct NoopClient {
    config_enabled: bool,
    accepted: AtomicU64,
    dropped: AtomicU64,
}

impl NoopClient {
    /// Construct a NoopClient with the given config-side enabled flag.
    /// `config_enabled = true` is the default; pass `false` to model a
    /// caller that has already resolved telemetry-disabled-via-config and
    /// wants the resolver to short-circuit.
    #[must_use]
    pub const fn new(config_enabled: bool) -> Self {
        Self {
            config_enabled,
            accepted: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Shareable handle.
    #[must_use]
    pub fn shared(config_enabled: bool) -> Arc<dyn Client> {
        Arc::new(Self::new(config_enabled))
    }
}

#[async_trait]
impl Client for NoopClient {
    async fn emit(&self, _event: Event) {
        // FR-108: opt-out resolver decides; on disabled, the event MUST be
        // discarded and no network connection opened.
        if optout::is_enabled(&optout::StdEnv, self.config_enabled) {
            self.accepted.fetch_add(1, Ordering::Relaxed);
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn accepted_count(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    fn dropped_by_optout(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Component, EventPayload, ModelState};
    use crate::test_lock::lock as test_lock;

    /// Resets the process-wide runtime-disabled flag on drop so a panicking
    /// assertion in any test cannot leak state into sibling tests.
    struct ResetGuard;
    impl Drop for ResetGuard {
        fn drop(&mut self) {
            optout::set_runtime_disabled(false);
        }
    }

    fn sample_event() -> Event {
        Event::new(
            Component::Mcp,
            "0.1.0",
            EventPayload::McpStartup {
                startup_ms: 1,
                model_state: ModelState::Missing,
            },
        )
    }

    // `NoopClient::emit` is fully synchronous (no internal `.await`), so
    // holding `test_lock` across the emit call is safe — the await point is
    // only there to satisfy the `Client` trait shape. We allow the lint
    // per-test rather than at the crate level so a future async emit path
    // surfaces for review.

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn enabled_client_accepts() {
        let _g = test_lock();
        let _r = ResetGuard;
        optout::set_runtime_disabled(false);
        let c = NoopClient::new(true);
        c.emit(sample_event()).await;
        c.emit(sample_event()).await;
        assert_eq!(c.accepted_count(), 2);
        assert_eq!(c.dropped_by_optout(), 0);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn config_disabled_drops_via_optout() {
        let _g = test_lock();
        let _r = ResetGuard;
        optout::set_runtime_disabled(false);
        let c = NoopClient::new(false);
        c.emit(sample_event()).await;
        c.emit(sample_event()).await;
        assert_eq!(c.accepted_count(), 0);
        assert_eq!(c.dropped_by_optout(), 2);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn runtime_toggle_drops_events() {
        let _g = test_lock();
        let _r = ResetGuard;
        let c = NoopClient::new(true);
        optout::set_runtime_disabled(true);
        c.emit(sample_event()).await;
        assert_eq!(c.accepted_count(), 0);
        assert_eq!(c.dropped_by_optout(), 1);
        optout::set_runtime_disabled(false);
        c.emit(sample_event()).await;
        assert_eq!(c.accepted_count(), 1);
    }
}

//! Telemetry client surface.
//!
//! [`Client`] is the trait every call site touches. Two production
//! implementations exist:
//!
//! - [`NoopClient`] — counts events, honours the opt-out resolver, but never
//!   opens a connection. Used by tests and as the "disabled" branch of
//!   [`TelemetryClient`].
//! - [`HttpClient`] — buffered batching client that POSTs JSON arrays of
//!   events to a configured cloud endpoint (FR-110 / FR-113). Flushes when
//!   either the in-memory queue reaches `flush_threshold` events OR the
//!   `flush_interval` elapses, whichever is sooner. Retries on 5xx + network
//!   errors with jittered exponential backoff, capped at three attempts and
//!   a ten-second wall-clock ceiling per batch.
//!
//! Every emit-site holds a `&dyn Client` (or `Arc<dyn Client>`) so swapping
//! the no-op for the HTTP client at boot is a one-line change with no
//! handler-side churn.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex;
use url::Url;

use crate::events::Event;
use crate::optout;

/// Default events-per-batch threshold (FR-108).
pub const DEFAULT_FLUSH_THRESHOLD: usize = 100;

/// Default flush-on-timer interval (FR-108).
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// Default per-request HTTP timeout for one flush attempt.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of retry attempts per batch (FR-113).
pub const MAX_RETRY_ATTEMPTS: u32 = 3;

/// Wall-clock ceiling for the retry loop (FR-113).
pub const RETRY_BUDGET: Duration = Duration::from_secs(10);

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

    /// Total batches the client has successfully POSTed (HTTP backends only).
    fn batches_sent(&self) -> u64 {
        0
    }

    /// Total batches the client has dropped after exhausting retries.
    fn batches_dropped(&self) -> u64 {
        0
    }

    /// Force any buffered events out to the configured sink. No-op for
    /// non-batching backends; HTTP backends drain the queue and POST it.
    async fn flush(&self) {}
}

/// Null implementation — counts events, honours the opt-out resolver, but
/// never actually sends them anywhere. Default for every component until the
/// HTTP-backed client is wired up at boot.
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

/// Tunable knobs for [`HttpClient`].
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    /// Resolved sink URL. Typically `https://manual.midnight.network/v1/telemetry/events`.
    pub endpoint: Url,
    /// `config_enabled` plumbed to the opt-out resolver. `false` short-circuits
    /// before any network I/O.
    pub config_enabled: bool,
    /// Number of buffered events that triggers a flush.
    pub flush_threshold: usize,
    /// Wall-clock between forced flushes when the threshold isn't hit.
    pub flush_interval: Duration,
    /// Per-attempt HTTP timeout.
    pub request_timeout: Duration,
}

impl HttpClientConfig {
    /// Build a config with all the documented defaults filled in.
    ///
    /// # Errors
    ///
    /// Returns [`HttpClientError::Url`] when `endpoint` is not a valid URL.
    pub fn new(endpoint: &str, config_enabled: bool) -> Result<Self, HttpClientError> {
        let endpoint = Url::parse(endpoint).map_err(|e| HttpClientError::Url(e.to_string()))?;
        Ok(Self {
            endpoint,
            config_enabled,
            flush_threshold: DEFAULT_FLUSH_THRESHOLD,
            flush_interval: DEFAULT_FLUSH_INTERVAL,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        })
    }
}

/// All the ways [`HttpClient`] can fail to construct.
#[derive(Debug, thiserror::Error)]
pub enum HttpClientError {
    /// The endpoint URL did not parse.
    #[error("invalid endpoint url: {0}")]
    Url(String),
    /// Underlying [`reqwest::Client`] could not be built (TLS init failure, etc.).
    #[error("http client build failed: {0}")]
    Transport(String),
}

/// Internal counters — shared between the foreground emit task and the
/// background flusher.
#[derive(Debug, Default)]
struct Counters {
    accepted: AtomicU64,
    dropped_by_optout: AtomicU64,
    batches_sent: AtomicU64,
    batches_dropped: AtomicU64,
}

/// Buffered HTTP telemetry client (FR-108 / FR-110 / FR-113).
#[derive(Debug)]
pub struct HttpClient {
    cfg: HttpClientConfig,
    http: reqwest::Client,
    queue: Arc<Mutex<Vec<Event>>>,
    counters: Arc<Counters>,
}

impl HttpClient {
    /// Construct an `HttpClient` from a `HttpClientConfig`.
    ///
    /// # Errors
    ///
    /// Returns [`HttpClientError::Transport`] when the underlying reqwest
    /// client cannot be built.
    pub fn new(cfg: HttpClientConfig) -> Result<Self, HttpClientError> {
        let http = reqwest::Client::builder()
            .timeout(cfg.request_timeout)
            .user_agent(concat!("midnight-manual-telemetry/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| HttpClientError::Transport(e.to_string()))?;
        Ok(Self {
            cfg,
            http,
            queue: Arc::new(Mutex::new(Vec::with_capacity(DEFAULT_FLUSH_THRESHOLD))),
            counters: Arc::new(Counters::default()),
        })
    }

    /// Wrap an `HttpClient` in `Arc` and spawn its background flush timer.
    /// The returned handle keeps the timer alive for the program lifetime —
    /// when the last `Arc` drops, the timer notices the strong-count = 1
    /// invariant and exits.
    #[must_use]
    pub fn spawn(self) -> Arc<Self> {
        let arc = Arc::new(self);
        let weak = Arc::downgrade(&arc);
        let interval = arc.cfg.flush_interval;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            // The first tick fires immediately; skip it so the very first
            // flush honours the configured interval.
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(client) = weak.upgrade() else {
                    break;
                };
                client.flush_inner().await;
            }
        });
        arc
    }

    async fn enqueue(&self, event: Event) {
        let mut should_flush = false;
        {
            let mut q = self.queue.lock().await;
            q.push(event);
            if q.len() >= self.cfg.flush_threshold {
                should_flush = true;
            }
        }
        self.counters.accepted.fetch_add(1, Ordering::Relaxed);
        if should_flush {
            self.flush_inner().await;
        }
    }

    async fn flush_inner(&self) {
        let drained: Vec<Event> = {
            let mut q = self.queue.lock().await;
            if q.is_empty() {
                return;
            }
            std::mem::take(&mut *q)
        };
        let batch_size = drained.len();
        match self.send_with_retries(&drained).await {
            FlushOutcome::Sent => {
                self.counters.batches_sent.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(events = batch_size, "telemetry batch posted");
            }
            FlushOutcome::Dropped => {
                self.counters
                    .batches_dropped
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(events = batch_size, "telemetry batch dropped after retries");
            }
        }
    }

    async fn send_with_retries(&self, batch: &[Event]) -> FlushOutcome {
        let deadline = Instant::now() + RETRY_BUDGET;
        for attempt in 0..MAX_RETRY_ATTEMPTS {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let response = self
                .http
                .post(self.cfg.endpoint.clone())
                .json(batch)
                .send()
                .await;
            match classify(response) {
                AttemptResult::Success => return FlushOutcome::Sent,
                AttemptResult::DropPermanent => return FlushOutcome::Dropped,
                AttemptResult::Retry => {
                    if attempt + 1 >= MAX_RETRY_ATTEMPTS {
                        break;
                    }
                    let backoff = backoff_for_attempt(attempt);
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    tokio::time::sleep(backoff.min(remaining)).await;
                }
            }
        }
        FlushOutcome::Dropped
    }
}

#[async_trait]
impl Client for HttpClient {
    async fn emit(&self, event: Event) {
        if !optout::is_enabled(&optout::StdEnv, self.cfg.config_enabled) {
            // FR-108: zero events MAY leave the machine; the queue is not
            // even consulted when disabled.
            self.counters
                .dropped_by_optout
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.enqueue(event).await;
    }

    fn accepted_count(&self) -> u64 {
        self.counters.accepted.load(Ordering::Relaxed)
    }

    fn dropped_by_optout(&self) -> u64 {
        self.counters.dropped_by_optout.load(Ordering::Relaxed)
    }

    fn batches_sent(&self) -> u64 {
        self.counters.batches_sent.load(Ordering::Relaxed)
    }

    fn batches_dropped(&self) -> u64 {
        self.counters.batches_dropped.load(Ordering::Relaxed)
    }

    async fn flush(&self) {
        self.flush_inner().await;
    }
}

#[derive(Debug)]
enum FlushOutcome {
    Sent,
    Dropped,
}

#[derive(Debug)]
enum AttemptResult {
    Success,
    Retry,
    DropPermanent,
}

fn classify(result: Result<reqwest::Response, reqwest::Error>) -> AttemptResult {
    match result {
        Ok(resp) => {
            let s = resp.status();
            if s.is_success() {
                AttemptResult::Success
            } else if s.is_server_error() {
                AttemptResult::Retry
            } else {
                // 4xx: spec says drop the batch — the server already rejected
                // it on shape grounds, so re-sending is futile.
                AttemptResult::DropPermanent
            }
        }
        Err(e) => {
            // Connect / DNS / TLS / timeout errors are all transient enough
            // to warrant a retry. The retry budget caps total wall-clock.
            tracing::debug!(error = %e, "telemetry post failed (transient)");
            AttemptResult::Retry
        }
    }
}

/// Backoff schedule: 100ms, 500ms, 2s. Each value gets +/- 25% jitter,
/// derived from a simple pseudo-random source so we don't pull a full RNG
/// dep just for jitter.
fn backoff_for_attempt(attempt: u32) -> Duration {
    let base_ms: u64 = match attempt {
        0 => 100,
        1 => 500,
        _ => 2_000,
    };
    // Pseudo-random jitter in the range [-25%, +25%]. Source: low bits of
    // the wall-clock subsec nanos — monotonic enough for spreading retries
    // across siblings, and no RNG dep needed.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    let jitter_pct: i64 = i64::try_from(nanos % 51).unwrap_or(0) - 25; // -25..=25
    let base_i: i64 = i64::try_from(base_ms).unwrap_or(i64::MAX);
    let adjusted = jitter_pct.saturating_mul(base_i) / 100;
    let final_i = base_i.saturating_add(adjusted).max(0);
    Duration::from_millis(u64::try_from(final_i).unwrap_or(0))
}

/// Top-level telemetry handle held by every component (CLI / MCP / server).
///
/// Constructed at process boot from the resolved opt-out state and the
/// configured sink URL. When opt-out is in effect, the `Disabled` variant is
/// chosen and `emit` immediately returns without consulting the queue or
/// allocating; the variant pattern lets callers hold `&TelemetryClient`
/// without trait-object indirection.
#[derive(Debug)]
pub enum TelemetryClient {
    /// Real, network-backed batched client.
    Real(Arc<HttpClient>),
    /// Compile-time-cheap no-op when the resolver has decided "off".
    Disabled,
}

impl TelemetryClient {
    /// Build the real client and spawn its background flusher. Returns
    /// `Disabled` when the resolver says off regardless of the sink URL.
    ///
    /// # Errors
    ///
    /// Returns [`HttpClientError`] when the endpoint URL fails to parse or
    /// the underlying reqwest client cannot be constructed.
    pub fn boot(endpoint: &str, config_enabled: bool) -> Result<Self, HttpClientError> {
        if !optout::is_enabled(&optout::StdEnv, config_enabled) {
            return Ok(Self::Disabled);
        }
        let cfg = HttpClientConfig::new(endpoint, config_enabled)?;
        let client = HttpClient::new(cfg)?.spawn();
        Ok(Self::Real(client))
    }

    /// Disabled handle — used by tests and by code that has resolved opt-out
    /// before reaching this layer.
    #[must_use]
    pub const fn disabled() -> Self {
        Self::Disabled
    }

    /// Emit one event. Cheap fast path on `Disabled`.
    pub async fn emit(&self, event: Event) {
        match self {
            Self::Disabled => {}
            Self::Real(c) => c.emit(event).await,
        }
    }

    /// Force any buffered events out. No-op on `Disabled`.
    pub async fn flush(&self) {
        if let Self::Real(c) = self {
            c.flush().await;
        }
    }

    /// Total batches successfully sent.
    #[must_use]
    pub fn batches_sent(&self) -> u64 {
        match self {
            Self::Disabled => 0,
            Self::Real(c) => c.batches_sent(),
        }
    }

    /// Total batches dropped after retries.
    #[must_use]
    pub fn batches_dropped(&self) -> u64 {
        match self {
            Self::Disabled => 0,
            Self::Real(c) => c.batches_dropped(),
        }
    }

    /// Whether the resolver has put this handle in the no-op branch.
    #[must_use]
    pub const fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
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

    #[tokio::test]
    async fn backoff_values_are_bounded_within_jitter_window() {
        for attempt in 0u32..3 {
            for _ in 0..20 {
                let d = backoff_for_attempt(attempt);
                let ms = d.as_millis();
                let (lo, hi) = match attempt {
                    0 => (75, 125),
                    1 => (375, 625),
                    _ => (1500, 2500),
                };
                assert!((lo..=hi).contains(&ms), "attempt {attempt}: {ms}ms outside [{lo}, {hi}]",);
            }
        }
    }

    // The next four tests hold `test_lock()` to serialise access to the
    // process-wide opt-out atomic, but the test body uses async `.await`s on
    // the HTTP client's Tokio mutex. The lock guard is dropped at scope end;
    // none of these tests await another #[tokio::test] under the lock, so
    // the deadlock the clippy lint guards against cannot occur here.

    #[tokio::test]
    #[allow(clippy::await_holding_lock, clippy::significant_drop_tightening)]
    async fn http_client_threshold_flush_drains_queue() {
        let _g = test_lock();
        let _r = ResetGuard;
        optout::set_runtime_disabled(false);
        // Fire the flush right after the third event; the receiver is a
        // black hole at this URL, so the batch will fail but the queue
        // MUST still be drained for the next batch to accumulate.
        let mut cfg = HttpClientConfig::new("http://127.0.0.1:1/never", true).unwrap();
        cfg.flush_threshold = 3;
        cfg.request_timeout = Duration::from_millis(10);
        let c = HttpClient::new(cfg).unwrap();
        for _ in 0..3 {
            c.emit(sample_event()).await;
        }
        // Threshold fires inline; after the third emit the queue is drained
        // and the (failed) batch counted as dropped.
        let q = c.queue.lock().await;
        assert!(q.is_empty(), "queue must be drained after threshold flush");
        // Three events accepted, one batch dropped after retries.
        assert_eq!(c.accepted_count(), 3);
        assert!(c.batches_dropped() >= 1, "drop count is {}", c.batches_dropped());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock, clippy::significant_drop_tightening)]
    async fn http_client_opt_out_short_circuits_before_queue() {
        let _g = test_lock();
        let _r = ResetGuard;
        let cfg = HttpClientConfig::new("http://127.0.0.1:1/never", false).unwrap();
        let c = HttpClient::new(cfg).unwrap();
        c.emit(sample_event()).await;
        c.emit(sample_event()).await;
        assert_eq!(c.accepted_count(), 0);
        assert_eq!(c.dropped_by_optout(), 2);
        let q = c.queue.lock().await;
        assert!(q.is_empty(), "no events should reach the queue when opted out");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn http_client_flush_noop_on_empty_queue() {
        let _g = test_lock();
        let _r = ResetGuard;
        optout::set_runtime_disabled(false);
        let cfg = HttpClientConfig::new("http://127.0.0.1:1/never", true).unwrap();
        let c = HttpClient::new(cfg).unwrap();
        // No events emitted — flush should not POST or count a dropped batch.
        c.flush().await;
        assert_eq!(c.batches_sent(), 0);
        assert_eq!(c.batches_dropped(), 0);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn telemetry_client_disabled_is_cheap_no_op() {
        let _g = test_lock();
        let _r = ResetGuard;
        // Resolver says off via config_enabled=false; the boot path must
        // pick `Disabled` rather than spawning a flusher task.
        let c = TelemetryClient::boot("http://127.0.0.1:1/never", false).unwrap();
        assert!(c.is_disabled());
        c.emit(sample_event()).await; // returns instantly
        c.flush().await;
        assert_eq!(c.batches_sent(), 0);
        assert_eq!(c.batches_dropped(), 0);
    }
}

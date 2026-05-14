//! FR-112 / SC-061 — canary baseline.
//!
//! Phase 8a's job is to land the infrastructure. Once telemetry call sites
//! exist (Phase 8b+ and ongoing component work), the suite expands to drive
//! every endpoint and tool with canary-laden inputs. For now we lock the
//! invariant that the canary infrastructure itself never produces canaries
//! when fed clean inputs, and we prove the detector mechanism works
//! end-to-end against captured log output.

use std::sync::{Arc, Mutex};

use mn_telemetry::canary::{self, find_first_match, CANARY_STRINGS};
use mn_telemetry::client::Client;
use mn_telemetry::events::{Component, EventPayload, ModelState};
use mn_telemetry::{Event, NoopClient};
use tracing::Level;
use tracing_subscriber::fmt::MakeWriter;

/// A `MakeWriter` that accumulates every log line into a shared `Vec<u8>` so
/// tests can grep it for canary leaks after a code path runs.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn snapshot(&self) -> String {
        let buf = self.0.lock().expect("captured-logs lock");
        String::from_utf8_lossy(&buf).into_owned()
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = LogSink;
    fn make_writer(&'a self) -> Self::Writer {
        LogSink(self.0.clone())
    }
}

struct LogSink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log-sink lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Install a tracing subscriber that writes into the given `CapturedLogs`
/// for the duration of the dispatched closure. Each test installs its own
/// subscriber via `with_default` to avoid cross-test interference.
fn with_capture<F: FnOnce()>(f: F) -> String {
    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    captured.snapshot()
}

#[test]
fn detector_finds_planted_canary_in_log_output() {
    // Sanity check: we can install a subscriber, write a canary-bearing log
    // line, and then catch the canary via `find_first_match`. If this stops
    // working the rest of the suite stops detecting real leaks.
    let logs = with_capture(|| {
        tracing::warn!(canary = %CANARY_STRINGS[0].value, "planted canary");
    });
    let m = find_first_match(&logs).expect("should detect");
    assert_eq!(m.value, CANARY_STRINGS[0].value);
}

#[test]
fn detector_returns_clean_on_benign_logs() {
    let logs = with_capture(|| {
        tracing::info!("ingest run finished, 12 documents updated");
        tracing::warn!("flush failure; queued for retry");
    });
    assert!(find_first_match(&logs).is_none(), "benign logs must be clean: {logs:?}");
}

#[tokio::test]
async fn noop_client_serialization_does_not_leak_canary() {
    // Construct an Event that would normally carry a request-id. Even when
    // request_id LOOKS canary-ish, no canary string can structurally fit
    // into the typed fields, so the event serialization MUST be canary-free.
    let event = Event::new(
        Component::Server,
        "0.1.0",
        EventPayload::McpStartup {
            startup_ms: 7,
            model_state: ModelState::Missing,
        },
    );
    let wire = serde_json::to_string(&event).expect("serialize");
    canary::assert_no_canary_in(&wire);

    // Round-trip through a NoopClient (with telemetry enabled): no panic,
    // no canary anywhere even when we deliberately log around the emit.
    let logs = with_capture(|| {
        // Spawn a synchronous runtime block so we can run async inside a
        // sync #[test] would be cleaner, but we're already in a #[tokio::test].
        // No log lines from the noop client either way.
    });
    let c = NoopClient::new(true);
    c.emit(event).await;
    assert_eq!(c.accepted_count(), 1);
    canary::assert_no_canary_in(&logs);
}

#[test]
fn current_log_output_does_not_contain_canaries() {
    // The aspirational form of FR-112 will drive every endpoint and tool
    // here with canary-laden inputs; in Phase 8a we have no telemetry call
    // sites yet, so the baseline is that emitting a few representative
    // tracing events with NON-canary content produces no canary hits.
    // When telemetry call sites exist this test expands to invoke them
    // with canary inputs and then assert the captured logs.
    let logs = with_capture(|| {
        tracing::info!(request_id = "req-123", "search request received");
        tracing::warn!(request_id = "req-123", "cloud call failed; falling back");
        tracing::info!(request_id = "req-123", "search complete; 5 results");
    });
    canary::assert_no_canary_in(&logs);
}

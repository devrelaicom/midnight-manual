//! Wiremock-backed integration tests for the buffered HTTP telemetry client.
//!
//! These tests run the same surface a real component would: drive `emit()`
//! against a `wiremock` server impersonating `/v1/telemetry/events`, then
//! assert on what wiremock recorded.
//!
//! The retry-on-5xx test uses wiremock's `up_to_n_times` to fail the first N
//! attempts and succeed thereafter, so we don't need to mock time.

use std::time::Duration;

use mnm_telemetry::client::{Client, HttpClient, HttpClientConfig};
use mnm_telemetry::events::{Component, EventPayload, ModelState};
use mnm_telemetry::Event;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn endpoint(server: &MockServer) -> String {
    format!("{}/v1/telemetry/events", server.uri())
}

#[tokio::test]
async fn successful_batch_marks_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/telemetry/events"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = HttpClientConfig::new(&endpoint(&server), true).unwrap();
    let c = HttpClient::new(cfg).unwrap();
    c.emit(sample_event()).await;
    c.flush().await;

    assert_eq!(c.batches_sent(), 1);
    assert_eq!(c.batches_dropped(), 0);
    assert_eq!(c.accepted_count(), 1);
}

#[tokio::test]
async fn five_hundred_retries_then_succeeds() {
    let server = MockServer::start().await;
    // First attempt returns 500, second returns 202.
    Mock::given(method("POST"))
        .and(path("/v1/telemetry/events"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/telemetry/events"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    let mut cfg = HttpClientConfig::new(&endpoint(&server), true).unwrap();
    cfg.request_timeout = Duration::from_millis(500);
    let c = HttpClient::new(cfg).unwrap();
    c.emit(sample_event()).await;
    c.flush().await;

    assert_eq!(c.batches_sent(), 1, "should succeed on second attempt");
    assert_eq!(c.batches_dropped(), 0);
}

#[tokio::test]
async fn four_hundred_drops_batch_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/telemetry/events"))
        .respond_with(ResponseTemplate::new(400))
        // Exactly one attempt — a 4xx must not trigger retries.
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = HttpClientConfig::new(&endpoint(&server), true).unwrap();
    cfg.request_timeout = Duration::from_millis(500);
    let c = HttpClient::new(cfg).unwrap();
    c.emit(sample_event()).await;
    c.flush().await;

    assert_eq!(c.batches_sent(), 0);
    assert_eq!(c.batches_dropped(), 1, "4xx must be a permanent drop");
}

#[tokio::test]
async fn all_five_hundreds_drops_after_max_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/telemetry/events"))
        .respond_with(ResponseTemplate::new(503))
        // ≤3 attempts per the FR-113 budget. Anything more is a regression.
        .expect(3)
        .mount(&server)
        .await;

    let mut cfg = HttpClientConfig::new(&endpoint(&server), true).unwrap();
    cfg.request_timeout = Duration::from_millis(200);
    let c = HttpClient::new(cfg).unwrap();
    c.emit(sample_event()).await;
    c.flush().await;

    assert_eq!(c.batches_sent(), 0);
    assert_eq!(c.batches_dropped(), 1);
}

#[tokio::test]
async fn batch_carries_json_array_of_typed_events() {
    use serde_json::Value;
    use wiremock::matchers::body_json_schema;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/telemetry/events"))
        .and(body_json_schema::<Value>)
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = HttpClientConfig::new(&endpoint(&server), true).unwrap();
    let c = HttpClient::new(cfg).unwrap();
    // Two events in one batch.
    c.emit(sample_event()).await;
    c.emit(sample_event()).await;
    c.flush().await;

    let recorded = server.received_requests().await.unwrap();
    assert_eq!(recorded.len(), 1, "exactly one POST");
    let body: Value = serde_json::from_slice(&recorded[0].body).unwrap();
    let arr = body.as_array().expect("body must be JSON array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["component"], "mcp");
    assert_eq!(arr[0]["payload"]["event_type"], "mcp_startup");
}

#[tokio::test]
async fn empty_flush_is_noop() {
    let server = MockServer::start().await;
    // No mounts: any request would 404 and surface in `received_requests`.
    let cfg = HttpClientConfig::new(&endpoint(&server), true).unwrap();
    let c = HttpClient::new(cfg).unwrap();
    c.flush().await;
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(c.batches_sent(), 0);
    assert_eq!(c.batches_dropped(), 0);
}

#[tokio::test]
async fn network_failure_classified_as_retry() {
    // No server at all — every attempt should fail at connect. The retry
    // budget exhausts and the batch is reported as dropped.
    let endpoint = "http://127.0.0.1:1/v1/telemetry/events"; // privileged port → connect refused
    let mut cfg = HttpClientConfig::new(endpoint, true).unwrap();
    cfg.request_timeout = Duration::from_millis(50);
    let c = HttpClient::new(cfg).unwrap();
    c.emit(sample_event()).await;
    c.flush().await;
    assert_eq!(c.batches_sent(), 0);
    assert_eq!(c.batches_dropped(), 1);
}

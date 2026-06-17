//! End-to-end exercises for `POST /v1/telemetry/events` (Phase 8b / FR-110).

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use midnight_manual_server::{app, config::ServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn raw_post(app: axum::Router, uri: &str, body: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn cfg() -> ServerConfig {
    ServerConfig::default()
}

fn sample_mcp_startup_event() -> Value {
    json!({
        "component": "mcp",
        "version": "0.1.0",
        "payload": {
            "event_type": "mcp_startup",
            "startup_ms": 42,
            "model_state": "missing"
        }
    })
}

#[tokio::test]
async fn happy_path_persists_batch_and_returns_202() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    // Scope by unique request_id so parallel telemetry tests don't bias
    // the row count we're asserting on.
    let rid = format!("telemetry-test-{}", Uuid::new_v4());
    let event = json!({
        "component": "mcp",
        "version": "0.1.0",
        "request_id": rid.clone(),
        "payload": {
            "event_type": "mcp_startup",
            "startup_ms": 42,
            "model_state": "missing"
        }
    });
    let batch = json!([event.clone(), event]);
    let (status, body) = post(app, "/v1/telemetry/events", batch).await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    assert_eq!(body["accepted"], 2);
    assert_eq!(body["rejected"], 0);

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM telemetry_event_raw WHERE request_id = $1",
    )
    .bind(&rid)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(count, 2, "two rows must be persisted under our request_id");
}

#[tokio::test]
async fn unknown_event_type_is_rejected_per_row() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let batch = json!([{
        "component": "mcp",
        "version": "0.1.0",
        "payload": {
            "event_type": "definitely_not_in_the_allow_list",
            "startup_ms": 1,
            "model_state": "missing"
        }
    }]);
    let (status, body) = post(app, "/v1/telemetry/events", batch).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["accepted"], 0);
    assert_eq!(body["rejected"], 1);
    assert_eq!(body["errors"][0]["reason"], "unknown_event_type");
}

#[tokio::test]
async fn unknown_component_is_rejected_per_row() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let batch = json!([{
        "component": "spookyclient",
        "version": "0.1.0",
        "payload": {
            "event_type": "mcp_startup",
            "startup_ms": 1,
            "model_state": "ready"
        }
    }]);
    let (_, body) = post(app, "/v1/telemetry/events", batch).await;
    assert_eq!(body["rejected"], 1);
    assert_eq!(body["errors"][0]["reason"], "unknown_component");
}

#[tokio::test]
async fn mixed_valid_and_invalid_partial_accept() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let batch = json!([
        sample_mcp_startup_event(),
        {
            "component": "mcp",
            "version": "0.1.0",
            "payload": {
                "event_type": "no_such_thing"
            }
        },
        sample_mcp_startup_event(),
    ]);
    let (status, body) = post(app, "/v1/telemetry/events", batch).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["accepted"], 2);
    assert_eq!(body["rejected"], 1);
    assert_eq!(body["errors"][0]["index"], 1);
}

#[tokio::test]
async fn malformed_json_is_400() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let (status, _) = raw_post(app, "/v1/telemetry/events", "{ not json").await;
    // axum's Json extractor returns 400 for syntactic errors and 422 for shape
    // mismatches — both are client errors and equally fine for our purposes.
    assert!(status.is_client_error(), "got {status}");
}

#[tokio::test]
async fn unknown_top_level_field_is_rejected() {
    // `deny_unknown_fields` on InboundEvent means a stray top-level key
    // must trip the serde decode and 4xx the whole batch.
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");
    let batch = json!([{
        "component": "mcp",
        "version": "0.1.0",
        "payload": { "event_type": "mcp_startup", "startup_ms": 1, "model_state": "ready" },
        "totally_unknown": "rejected"
    }]);
    let (status, _) = post(app, "/v1/telemetry/events", batch).await;
    assert!(status.is_client_error(), "got {status}");
}

#[tokio::test]
async fn batch_carries_request_id() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");
    let batch = json!([{
        "component": "mcp",
        "version": "0.1.0",
        "payload": { "event_type": "mcp_startup", "startup_ms": 1, "model_state": "ready" },
        "request_id": "req-test-8b"
    }]);
    let (_, body) = post(app, "/v1/telemetry/events", batch).await;
    assert_eq!(body["accepted"], 1);
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT request_id FROM telemetry_event_raw WHERE request_id = 'req-test-8b' LIMIT 1",
    )
    .fetch_optional(&h.pool)
    .await
    .unwrap();
    assert_eq!(stored.as_deref(), Some("req-test-8b"));
}

#[tokio::test]
async fn oversize_batch_is_rejected() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");
    // 1001 events — one over the documented boundary cap.
    let batch: Vec<Value> = (0..1001).map(|_| sample_mcp_startup_event()).collect();
    let (status, body) = post(app, "/v1/telemetry/events", json!(batch)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn empty_batch_is_a_noop_202() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");
    let (status, body) = post(app, "/v1/telemetry/events", json!([])).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["accepted"], 0);
    assert_eq!(body["rejected"], 0);
}

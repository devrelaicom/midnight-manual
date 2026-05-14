//! Integration tests for `GET /metrics` (FR-111).

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use mn_server::{app, config::ServerConfig};
use tower::ServiceExt;

async fn get_text(app: axum::Router, uri: &str) -> (StatusCode, String, Option<String>) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    (status, String::from_utf8_lossy(&body).into_owned(), ct)
}

/// Pin a single aggregate row at a deliberately distant day so the
/// resulting Prometheus line is unique to this test and not racing other
/// integration suites running against the shared CI Postgres. Returns the
/// (`event_type`, `component`) tuple used.
async fn seed_unique_aggregate(pool: &sqlx::PgPool, count: i64) -> (&'static str, &'static str) {
    // 200 days ago; (`ingest_complete`, `cli`) is not emitted by any other
    // phase-8c test's emit-site.
    sqlx::query(
        "INSERT INTO telemetry_aggregate_daily (day, event_type, component, count) \
         VALUES (CURRENT_DATE - INTERVAL '200 days', 'ingest_complete', 'cli', $1) \
         ON CONFLICT (day, event_type, component) \
         DO UPDATE SET count = telemetry_aggregate_daily.count + $1",
    )
    .bind(count)
    .execute(pool)
    .await
    .unwrap();
    ("ingest_complete", "cli")
}

#[tokio::test]
async fn metrics_returns_prometheus_content_type_and_help_lines() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let (status, body, ct) = get_text(app, "/metrics").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        ct.as_deref().is_some_and(|s| s.starts_with("text/plain")),
        "content-type: {ct:?}",
    );
    assert!(
        body.contains("# HELP midnight_manual_telemetry_events_total"),
        "missing HELP: {body}",
    );
    assert!(
        body.contains("# TYPE midnight_manual_telemetry_events_total counter"),
        "missing TYPE: {body}",
    );
}

#[tokio::test]
async fn metrics_reflects_aggregate_daily_rows() {
    let h = common::boot().await;
    let (event_type, component) = seed_unique_aggregate(&h.pool, 42).await;

    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");
    let (_, body, _) = get_text(app, "/metrics").await;

    // The lifetime counter sums every day for this (event_type, component);
    // our seed contributed 42 (or 42 + N from any prior re-run), so the
    // total must be ≥ 42.
    let prefix = format!(
        r#"midnight_manual_telemetry_events_total{{event_type="{event_type}",component="{component}"}} "#,
    );
    let line = body
        .lines()
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no metric row matching {prefix:?} in body: {body}"));
    let value: i64 = line
        .trim_start_matches(&prefix)
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("non-numeric metric value in line {line:?}"));
    assert!(value >= 42, "lifetime counter must reflect our seed; got {value}");
}

#[tokio::test]
async fn metrics_is_anonymous_no_bearer_required() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    // No Authorization header — must still 200.
    let (status, _, _) = get_text(app, "/metrics").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn metrics_help_and_type_lines_always_present() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");
    let (status, body, _) = get_text(app, "/metrics").await;
    assert_eq!(status, StatusCode::OK);
    // The HELP / TYPE comments are written before any data rows, so they
    // are emitted regardless of whether the aggregate table is empty or
    // populated by other concurrent tests.
    assert!(body.contains("# HELP midnight_manual_telemetry_events_total"));
    assert!(body.contains("# HELP midnight_manual_telemetry_events_today"));
    assert!(body.contains("# TYPE midnight_manual_telemetry_events_total counter"));
    assert!(body.contains("# TYPE midnight_manual_telemetry_events_today gauge"));
}

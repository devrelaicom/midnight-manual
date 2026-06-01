//! Guard: oversize request bodies are refused with 413 before any handler
//! runs (the OOM-safety bound, `app::MAX_BODY_BYTES`).
#![cfg(feature = "integration")]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mn_server::{app, config::ServerConfig};
use tower::ServiceExt;

#[tokio::test]
async fn oversize_body_is_rejected_with_413() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    // One byte over the configured cap.
    let oversized = vec![b'x'; mn_server::app::MAX_BODY_BYTES + 1];
    let req = Request::builder()
        .method("PUT")
        .uri(
            "/v1/admin/sources/whatever/ingest-runs/00000000-0000-0000-0000-000000000000/documents",
        )
        .header("content-type", "application/json")
        .body(Body::from(oversized))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

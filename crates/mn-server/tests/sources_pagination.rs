//! Integration tests for the keyset-paginated, filterable `GET /v1/sources`.
//!
//! Each test boots a fresh per-test schema (see `common::boot`), so the
//! `source` table contains exactly what the test seeds.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use mn_core::types::SourceKind;
use mn_server::{app, config::ServerConfig};
use mn_store::entities::source;
use tower::ServiceExt;

/// Seed three sources with deterministic slugs / kinds / `created_at`, then
/// retire the third. Layout (ordered by slug):
///
/// | slug      | kind       | created_at           | state   |
/// |-----------|------------|----------------------|---------|
/// | a-docs    | docs_site  | 2026-01-01T00:00:00Z | active  |
/// | b-code    | code_repo  | 2026-02-01T00:00:00Z | active  |
/// | c-retired | standalone | 2026-03-01T00:00:00Z | retired |
async fn seed_three_sources(pool: &sqlx::PgPool) {
    for (slug, kind, created_at) in [
        ("a-docs", SourceKind::DocsSite, "2026-01-01T00:00:00Z"),
        ("b-code", SourceKind::CodeRepo, "2026-02-01T00:00:00Z"),
        ("c-retired", SourceKind::Standalone, "2026-03-01T00:00:00Z"),
    ] {
        source::insert(pool, slug, slug, kind, None, 5)
            .await
            .unwrap();
        sqlx::query("UPDATE source SET created_at = $1::timestamptz WHERE slug = $2")
            .bind(created_at)
            .bind(slug)
            .execute(pool)
            .await
            .unwrap();
    }
    source::retire(pool, "c-retired").await.unwrap();
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (status, v)
}

fn slugs(v: &serde_json::Value) -> Vec<String> {
    v["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn default_call_returns_object_ordered_by_slug() {
    let h = common::boot().await;
    seed_three_sources(&h.pool).await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let (status, v) = get(app, "/v1/sources").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // Active only, slug order, no further pages.
    assert_eq!(slugs(&v), vec!["a-docs", "b-code"]);
    assert_eq!(v["total"], 2, "{v}");
    assert!(v["next_cursor"].is_null(), "{v}");
}

#[tokio::test]
async fn limit_1_walks_all_pages_without_overlap_or_gap() {
    let h = common::boot().await;
    seed_three_sources(&h.pool).await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut hops = 0;
    loop {
        let uri = cursor.as_ref().map_or_else(
            || "/v1/sources?limit=1&retired=true".to_owned(),
            |c| format!("/v1/sources?limit=1&retired=true&cursor={c}"),
        );
        let (status, v) = get(app.clone(), &uri).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["total"], 3, "total is cursor-independent: {v}");
        let page = slugs(&v);
        assert_eq!(page.len(), 1, "limit=1 must yield single-row pages: {v}");
        seen.extend(page);
        match v["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
        hops += 1;
        assert!(hops < 10, "cursor walk did not terminate");
    }
    // No overlaps, no gaps, slug order end to end.
    assert_eq!(seen, vec!["a-docs", "b-code", "c-retired"]);
}

#[tokio::test]
async fn kind_filter_selects_matching_sources_only() {
    let h = common::boot().await;
    seed_three_sources(&h.pool).await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let (status, v) = get(app, "/v1/sources?kind=docs_site").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(slugs(&v), vec!["a-docs"]);
    assert_eq!(v["total"], 1, "{v}");
}

#[tokio::test]
async fn retired_flag_includes_retired_rows() {
    let h = common::boot().await;
    seed_three_sources(&h.pool).await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    let (status, v) = get(app, "/v1/sources?retired=true").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(slugs(&v), vec!["a-docs", "b-code", "c-retired"]);
    assert_eq!(v["total"], 3, "{v}");
}

#[tokio::test]
async fn created_at_filters_are_strict_bounds() {
    let h = common::boot().await;
    seed_three_sources(&h.pool).await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    // Midpoint between a-docs and b-code; retired row included to show the
    // filters compose with `retired`.
    let (status, v) =
        get(app.clone(), "/v1/sources?created_after=2026-01-15T00:00:00Z&retired=true").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(slugs(&v), vec!["b-code", "c-retired"]);

    let (status, v) = get(app, "/v1/sources?created_before=2026-01-15T00:00:00Z").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(slugs(&v), vec!["a-docs"]);
}

#[tokio::test]
async fn invalid_params_return_typed_400() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), ServerConfig::default()).expect("build app");

    for uri in [
        "/v1/sources?limit=0",
        "/v1/sources?limit=101",
        "/v1/sources?cursor=!!!not-base64!!!",
        "/v1/sources?created_after=yesterday",
        "/v1/sources?created_before=2026-01-02",
        "/v1/sources?kind=github",
    ] {
        let (status, v) = get(app.clone(), uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} → {v}");
        assert_eq!(v["error"]["code"], "invalid_request", "{uri} → {v}");
        assert!(v["error"]["remediation"].is_string(), "{uri} → {v}");
    }
}

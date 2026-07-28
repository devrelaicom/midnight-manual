//! Integration tests for the multi-query rate-limit cost (Phase 18, D25).
//!
//! Covers the cap (EC-88, with token refund), duplicate-query dedup (EC-90),
//! the `max(1, distinct)` token charge (acceptance #5), and the insufficient-
//! budget 429 (EC-92). Each test uses a distinct `Fly-Client-IP` so buckets
//! never collide across tests or with the shared CI Postgres.

#![cfg(feature = "integration")]
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_precision_loss,
    clippy::suboptimal_flops
)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{HeaderMap, Request, StatusCode};
use midnight_manual_server::app;
use midnight_manual_server::config::ServerConfig;
use midnight_manual_server::ratelimit::RateLimiter;
use midnight_manual_server::tokenlimit::TokenUsageLimiter;
use mnm_core::provenance::Provenance;
use mnm_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mnm_store::entities::{chunk, document, embedding_model, node, source, source_version};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

fn unit_vector(seed: f32) -> Vec<f32> {
    (0..1024_i32).map(|i| seed + (i as f32) * 0.0001).collect()
}

fn rl_cfg(anonymous_rps: u32, max_queries: u32) -> ServerConfig {
    ServerConfig {
        rate_limit_enabled: true,
        rate_limit_anonymous_rps: anonymous_rps,
        max_queries_per_request: max_queries,
        ..Default::default()
    }
}

/// Resolve the corpus model from the DB (the seeded voyage-code-3@1 active
/// source_version) into the `Shared` handle `build_with_limiter` expects, so
/// the search handler's model-mismatch + dim guards see voyage-code-3@1/1024.
async fn resolved_cm(pool: &sqlx::PgPool) -> midnight_manual_server::corpus_model::Shared {
    let cm = midnight_manual_server::corpus_model::resolve(pool)
        .await
        .ok();
    std::sync::Arc::new(std::sync::RwLock::new(cm))
}

fn unique_ip() -> String {
    let b = Uuid::new_v4().into_bytes();
    format!("198.51.{}.{}", b[0], b[1])
}

/// Seed one queryable chunk so valid searches return 200.
async fn seed(pool: &sqlx::PgPool) {
    let model_id = embedding_model::upsert(pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let slug = format!("mq-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "MQ", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, None, "0.1.0", "h")
        .await
        .unwrap();
    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let doc_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, "doc.md", 0)
        .await
        .unwrap();
    let provenance = Provenance::default();
    let doc_id = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: None,
            source_path: "doc.md",
            language: Some("en"),
            content_hash: "h",
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 0,
            token_count: 0,
            license: None,
        },
    )
    .await
    .unwrap();
    let chunk_node = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "a", 0)
        .await
        .unwrap();
    chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node,
            chunk_index: 0,
            total_chunks: 1,
            content: "chunk content",
            content_hash: "ha",
            embedding: Some(unit_vector(0.10)),
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 13,
            token_count: 4,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .unwrap();
    source_version::finalize(pool, sv_id).await.unwrap();
}

async fn search(app: axum::Router, ip: &str, queries: Value) -> (StatusCode, HeaderMap, Value) {
    let body = json!({
        "queries": queries,
        "client_embedding_model": "voyage-code-3@1",
        "code_mode": "off",
        "limit": 5,
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header("content-type", "application/json")
                .header("fly-client-ip", ip)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, v)
}

fn q(seed: f32) -> Value {
    json!({ "text": format!("query-{seed}"), "vector": unit_vector(seed) })
}

#[tokio::test]
async fn over_cap_returns_400_and_refunds_the_base_token() {
    let h = common::boot().await;
    seed(&h.pool).await;
    // anon floor of 1 token, cap of 2 queries.
    let cfg = rl_cfg(1, 2);
    let limiter = RateLimiter::from_config(&cfg);
    let token_limiter = TokenUsageLimiter::from_config(&cfg);
    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        limiter,
        resolved_cm(&h.pool).await,
        token_limiter,
        None,
        None,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
        std::sync::Arc::new(std::sync::RwLock::new(None)),
    )
    .expect("build");
    let ip = unique_ip();

    // 3 queries > cap 2 → 400 multi_query_limit_exceeded.
    let (s1, _, b1) = search(app.clone(), &ip, json!([q(0.1), q(0.2), q(0.3)])).await;
    assert_eq!(s1, StatusCode::BAD_REQUEST, "{b1}");
    assert_eq!(b1["error"]["code"], "multi_query_limit_exceeded");
    assert!(
        b1["error"]["remediation"].as_str().unwrap().contains("50"),
        "remediation names the hard ceiling: {b1}"
    );

    // The refund means the single anon token is still available: a valid
    // single-query request on the SAME IP succeeds (would 429 if not refunded).
    let (s2, _, b2) = search(app, &ip, json!([q(0.11)])).await;
    assert_eq!(s2, StatusCode::OK, "base token must have been refunded: {b2}");
}

#[tokio::test]
async fn duplicate_queries_do_not_inflate_cost() {
    let h = common::boot().await;
    seed(&h.pool).await;
    let cfg = rl_cfg(2, 10);
    let limiter = RateLimiter::from_config(&cfg);
    let token_limiter = TokenUsageLimiter::from_config(&cfg);
    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        limiter,
        resolved_cm(&h.pool).await,
        token_limiter,
        None,
        None,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
        std::sync::Arc::new(std::sync::RwLock::new(None)),
    )
    .expect("build");
    let ip = unique_ip();

    // 3 identical queries → dedup to 1 → cost 1 (base only), 200.
    let dup = q(0.1);
    let (s, _, b) = search(app, &ip, json!([dup.clone(), dup.clone(), dup])).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["search_metadata"]["deduplicated_count"], 2, "two dups dropped: {b}");
}

#[tokio::test]
async fn distinct_queries_charge_n_tokens() {
    let h = common::boot().await;
    seed(&h.pool).await;
    let cfg = rl_cfg(5, 10);
    let limiter = RateLimiter::from_config(&cfg);
    let token_limiter = TokenUsageLimiter::from_config(&cfg);
    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        limiter,
        resolved_cm(&h.pool).await,
        token_limiter,
        None,
        None,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
        std::sync::Arc::new(std::sync::RwLock::new(None)),
    )
    .expect("build");
    let ip = unique_ip();

    // 3 distinct queries → cost 3 → remaining 5 - 3 = 2 (post-charge balance).
    let (s, headers, b) = search(app, &ip, json!([q(0.1), q(0.5), q(0.9)])).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["search_metadata"]["deduplicated_count"], 0);
    assert_eq!(
        headers.get("x-ratelimit-remaining").unwrap(),
        "2",
        "3 distinct queries must charge 3 tokens (base + 2 extra)"
    );
}

#[tokio::test]
async fn insufficient_budget_returns_429() {
    let h = common::boot().await;
    seed(&h.pool).await;
    // Capacity 2, but 3 distinct queries cost 3 → over budget.
    let cfg = rl_cfg(2, 10);
    let limiter = RateLimiter::from_config(&cfg);
    let token_limiter = TokenUsageLimiter::from_config(&cfg);
    let app = app::build_with_limiter(
        h.pool.clone(),
        cfg,
        limiter,
        resolved_cm(&h.pool).await,
        token_limiter,
        None,
        None,
        std::sync::Arc::new(std::sync::RwLock::new(None)),
        std::sync::Arc::new(std::sync::RwLock::new(None)),
    )
    .expect("build");
    let ip = unique_ip();

    let (s, headers, b) = search(app, &ip, json!([q(0.1), q(0.5), q(0.9)])).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{b}");
    assert_eq!(b["error"]["code"], "rate_limited");
    assert!(
        b["error"]["message"]
            .as_str()
            .unwrap()
            .contains("3 distinct queries"),
        "message names the cost: {b}"
    );
    assert!(headers.contains_key("x-ratelimit-remaining"), "remaining header present");
}

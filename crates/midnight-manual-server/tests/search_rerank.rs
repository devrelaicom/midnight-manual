//! POST /v1/search inline rerank (spec §1–2): applied path, token-budget
//! degrade, provider-error degrade, rerank=none passthrough.
//
// Uses the same gating/harness as search_route.rs (CI-only; needs Postgres).

#![cfg(feature = "integration")]
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::suboptimal_flops,
    clippy::missing_const_for_fn
)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use midnight_manual_server::{app, config::ServerConfig};
use mnm_core::provenance::Provenance;
use mnm_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mnm_store::entities::{chunk, document, embedding_model, node, source, source_version};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn unit_vector(seed: f32) -> Vec<f32> {
    // Deterministic 1024-dim vector for tests (voyage-code-3 width). Not
    // normalized; pgvector cosine handles arbitrary magnitudes.
    (0..1024_i32).map(|i| seed + (i as f32) * 0.0001).collect()
}

/// Seed three ready chunks (one document) and return their ids in seed order.
/// Slug is randomized per call so parallel CI runs don't collide.
async fn seed3(pool: &sqlx::PgPool) -> [Uuid; 3] {
    let model_id = embedding_model::upsert(pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let slug = format!("search-rerank-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Rerank", SourceKind::DocsSite, None, 5)
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

    // Three chunks with distinct content + vectors at increasing distance from
    // a 0.10-seed query, and non-overlapping byte ranges so dedup never trims
    // them. Vector seeds (0.10, 0.50, 0.90) keep RRF order = seed order.
    let specs: [(&str, &str, f32, i32, i32); 3] = [
        ("alpha chunk about midnight network compilers", "ha", 0.10, 0, 45),
        ("beta chunk about zswap shielded coin transfers", "hb", 0.50, 50, 95),
        ("gamma chunk about proof server witness data", "hc", 0.90, 100, 145),
    ];
    let mut ids = [Uuid::nil(); 3];
    for (i, (content, hash, seed, start, end)) in specs.into_iter().enumerate() {
        let cn =
            node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, &format!("c{i}"), i as i32)
                .await
                .unwrap();
        ids[i] = chunk::insert(
            pool,
            chunk::NewChunk {
                source_version_id: sv_id,
                document_id: doc_id,
                node_id: cn,
                chunk_index: i as i32,
                total_chunks: 3,
                content,
                content_hash: hash,
                embedding: Some(unit_vector(seed)),
                embedding_model_id: model_id,
                code_embedding: None,
                heading_path: &[],
                symbol_path: &[],
                start_byte: start,
                end_byte: end,
                token_count: 8,
                status: ChunkStatus::Ready,
            },
        )
        .await
        .unwrap();
    }

    source_version::finalize(pool, sv_id).await.unwrap();
    ids
}

/// Mock Voyage /v1/rerank: reverses the document order with descending scores
/// and reports 1000 total_tokens.
async fn rerank_mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let n = body["documents"].as_array().unwrap().len();
            let data: Vec<serde_json::Value> = (0..n)
                .map(|i| {
                    serde_json::json!({
                        "index": n - 1 - i,
                        "relevance_score": 0.9 - (i as f64) * 0.1
                    })
                })
                .collect();
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data": data, "usage": {"total_tokens": 1000}}))
        })
        .mount(&server)
        .await;
    server
}

/// Mock Voyage /v1/rerank that always fails with HTTP 500.
async fn rerank_error_mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream boom"))
        .mount(&server)
        .await;
    server
}

/// Build a config pointed at `mock_uri` with a fake Voyage key. Optional tiny
/// token ceilings drive the budget-exhaustion path.
fn cfg_with_voyage(mock_uri: &str, tiny_budget: bool) -> ServerConfig {
    let mut cfg = ServerConfig {
        voyage_api_key: Some("test-key".to_owned()),
        voyage_base_url: Some(mock_uri.to_owned()),
        ..Default::default()
    };
    if tiny_budget {
        // One token of headroom: the rerank estimate (query×docs + docs) far
        // exceeds this, so the reservation is rejected before any Voyage call.
        cfg.token_limit_anon_hourly = 1;
        cfg.token_limit_anon_daily = 1;
    }
    cfg
}

async fn post_search(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

async fn get_me(app: axum::Router) -> Value {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn search_body(rerank: &str) -> Value {
    serde_json::json!({
        "query": "alpha-ish content about midnight",
        "vector": unit_vector(0.11),
        "client_embedding_model": "voyage-code-3@1",
        "code_mode": "off",
        "limit": 100,
        "rerank": rerank,
    })
}

#[tokio::test]
async fn rerank_applied_reverses_order_and_stamps_scores() {
    let h = common::boot().await;
    let ids = seed3(&h.pool).await;
    let mock = rerank_mock().await;
    let app = app::build_resolved(h.pool.clone(), cfg_with_voyage(&mock.uri(), false))
        .await
        .expect("build app");

    let (status, v) = post_search(app, search_body("rerank-2.5")).await;
    assert_eq!(status, StatusCode::OK, "{v}");

    // Metadata: applied, model named, no reason.
    assert_eq!(
        v["search_metadata"]["rerank"],
        serde_json::json!({"applied": true, "model": "rerank-2.5"}),
        "{v}"
    );

    let results = v["results"].as_array().unwrap();
    assert!(results.len() >= 3, "expected ≥3 results, got {}", results.len());

    // Every reranked result carries a rerank_score and relevance_source=rerank.
    for r in results {
        assert!(
            r["scores"]["rerank_score"].is_number(),
            "each reranked result must stamp rerank_score: {r}"
        );
        assert_eq!(
            r["scores"]["confidence_factors"]["relevance_source"], "rerank",
            "relevance_source must be rerank: {r}"
        );
    }

    // The mock reverses pool order with descending scores, so the chunk that
    // ranked WORST by RRF (gamma, the 0.90-seed, farthest from the 0.11 query)
    // now wins. Assert this test's gamma chunk (ids[2]) ranks ahead of its
    // alpha chunk (ids[0]).
    let pos = |id: Uuid| {
        results
            .iter()
            .position(|r| r["chunk_id"].as_str() == Some(id.to_string().as_str()))
    };
    let (pa, pg) = (pos(ids[0]), pos(ids[2]));
    if let (Some(pa), Some(pg)) = (pa, pg) {
        assert!(
            pg < pa,
            "rerank reversal must rank gamma (worst RRF) ahead of alpha: gamma@{pg} alpha@{pa}"
        );
    }

    // The top result's rerank_score is the maximum across results.
    let top = results[0]["scores"]["rerank_score"].as_f64().unwrap();
    let max = results
        .iter()
        .filter_map(|r| r["scores"]["rerank_score"].as_f64())
        .fold(f64::MIN, f64::max);
    assert!(
        (top - max).abs() < 1e-9,
        "top result must have the highest rerank_score (top {top}, max {max})"
    );
}

#[tokio::test]
async fn rerank_degrades_on_token_budget_without_calling_voyage() {
    let h = common::boot().await;
    seed3(&h.pool).await;
    let mock = rerank_mock().await;
    let app = app::build_resolved(h.pool.clone(), cfg_with_voyage(&mock.uri(), true))
        .await
        .expect("build app");

    let (status, v) = post_search(app, search_body("rerank-2.5")).await;
    assert_eq!(status, StatusCode::OK, "search must not hard-fail: {v}");

    let results = v["results"].as_array().unwrap();
    assert!(!results.is_empty(), "RRF results must still be returned: {v}");
    // Degraded to RRF order: no rerank_score stamped.
    assert!(
        results
            .iter()
            .all(|r| r["scores"]["rerank_score"].is_null()),
        "budget-degraded results must not carry rerank_score: {v}"
    );
    assert_eq!(
        v["search_metadata"]["rerank"],
        serde_json::json!({
            "applied": false, "model": "rerank-2.5",
            "reason": "token_budget_exhausted"
        }),
        "{v}"
    );

    // The reservation was never attempted: zero /v1/rerank requests reached the
    // mock (we never pay Voyage when the pre-gate rejects).
    let received = mock.received_requests().await.unwrap();
    assert!(
        received.iter().all(|r| r.url.path() != "/v1/rerank"),
        "no /v1/rerank request must be made when the budget pre-gate rejects"
    );
}

#[tokio::test]
async fn rerank_degrades_on_provider_error_and_releases_reservation() {
    let h = common::boot().await;
    seed3(&h.pool).await;
    let mock = rerank_error_mock().await;
    let app = app::build_resolved(h.pool.clone(), cfg_with_voyage(&mock.uri(), false))
        .await
        .expect("build app");

    let (status, v) = post_search(app.clone(), search_body("rerank-2.5")).await;
    assert_eq!(status, StatusCode::OK, "provider error must not fail search: {v}");

    let results = v["results"].as_array().unwrap();
    assert!(!results.is_empty(), "RRF results must still be returned: {v}");
    assert!(
        results
            .iter()
            .all(|r| r["scores"]["rerank_score"].is_null()),
        "provider-degraded results must not carry rerank_score: {v}"
    );
    assert_eq!(
        v["search_metadata"]["rerank"],
        serde_json::json!({
            "applied": false, "model": "rerank-2.5", "reason": "provider_error"
        }),
        "{v}"
    );

    // The reservation was released on the provider error: the anon budget is
    // NOT debited. (Same anon subject — no client-IP headers on either call.)
    let me = get_me(app).await;
    let tl = &me["token_limits"];
    assert_eq!(
        tl["hourly"]["remaining"], tl["hourly"]["limit"],
        "a released reservation must leave the budget undebited: {me}"
    );
}

#[tokio::test]
async fn rerank_none_passes_through_and_lite_debits_half() {
    let h = common::boot().await;
    seed3(&h.pool).await;
    let mock = rerank_mock().await;

    // -- rerank=none: not_requested, no Voyage call, no rerank_score. --
    {
        let app = app::build_resolved(h.pool.clone(), cfg_with_voyage(&mock.uri(), false))
            .await
            .expect("build app");
        let (status, v) = post_search(app, search_body("none")).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(
            v["search_metadata"]["rerank"],
            serde_json::json!({"applied": false, "reason": "not_requested"}),
            "{v}"
        );
        let results = v["results"].as_array().unwrap();
        assert!(
            results
                .iter()
                .all(|r| r["scores"]["rerank_score"].is_null()),
            "rerank=none must not stamp rerank_score: {v}"
        );
    }
    let received = mock.received_requests().await.unwrap();
    let none_calls = received
        .iter()
        .filter(|r| r.url.path() == "/v1/rerank")
        .count();
    assert_eq!(none_calls, 0, "rerank=none must make zero /v1/rerank calls");

    // -- rerank-2.5-lite: the mock reports 1000 tokens; lite bills ceil/2 = 500. --
    {
        let app = app::build_resolved(h.pool.clone(), cfg_with_voyage(&mock.uri(), false))
            .await
            .expect("build app");
        let before = get_me(app.clone()).await["token_limits"]["hourly"]["remaining"]
            .as_u64()
            .unwrap();
        let (status, v) = post_search(app.clone(), search_body("rerank-2.5-lite")).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(
            v["search_metadata"]["rerank"],
            serde_json::json!({"applied": true, "model": "rerank-2.5-lite"}),
            "{v}"
        );
        let after = get_me(app).await["token_limits"]["hourly"]["remaining"]
            .as_u64()
            .unwrap();
        assert_eq!(
            before - after,
            500,
            "rerank-2.5-lite must debit ceil(1000/2) = 500 tokens (before {before}, after {after})"
        );
    }
}

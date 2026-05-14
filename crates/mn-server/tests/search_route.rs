//! Integration tests for `POST /v1/search` (Phase 4c).
//!
//! Seeds two documents/chunks with synthetic vectors, then exercises the
//! pgvector-only retrieval path. Real-embedding tests land alongside
//! mn-embedding in a later phase.

#![cfg(feature = "integration")]
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_precision_loss,
    clippy::suboptimal_flops,
    clippy::missing_const_for_fn
)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use mn_core::provenance::Provenance;
use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mn_server::{app, config::ServerConfig};
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use tower::ServiceExt;
use uuid::Uuid;

fn unit_vector(seed: f32) -> Vec<f32> {
    // Deterministic 768-dim vector for tests. Not normalized; pgvector cosine
    // operator handles arbitrary magnitudes.
    #[allow(clippy::cast_precision_loss)]
    (0..768_i32).map(|i| seed + (i as f32) * 0.0001).collect()
}

async fn seed(pool: &sqlx::PgPool) -> (Uuid, Uuid) {
    // Returns (chunk_id_a, chunk_id_b). Slug is randomized per call so parallel
    // CI test runs don't collide on the unique constraint.
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("search-route-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Search", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, "0.1.0", "h")
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
        },
    )
    .await
    .unwrap();
    let chunk_node_a = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "a", 0)
        .await
        .unwrap();
    let chunk_node_b = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "b", 1)
        .await
        .unwrap();

    let chunk_a = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node_a,
            chunk_index: 0,
            total_chunks: 2,
            content: "alpha chunk content about midnight network",
            content_hash: "ha",
            embedding: Some(unit_vector(0.10)),
            embedding_model_id: model_id,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 40,
            token_count: 8,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .unwrap();
    let chunk_b = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node_b,
            chunk_index: 1,
            total_chunks: 2,
            content: "beta chunk content about zswap shielded coins",
            content_hash: "hb",
            embedding: Some(unit_vector(0.90)),
            embedding_model_id: model_id,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 41,
            end_byte: 80,
            token_count: 8,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .unwrap();

    source_version::finalize(pool, sv_id).await.unwrap();
    (chunk_a, chunk_b)
}

fn cfg() -> ServerConfig {
    ServerConfig {
        database_url: String::new(),
        port: 0,
        auto_migrate: false,
        // Tests bypass the boot-time resolver so we pin the corpus model
        // explicitly. Matches the seeded `embedding_model` row in migration 0006.
        corpus_model: Some("bge-base-en-v1.5@1".to_owned()),
    }
}

#[tokio::test]
async fn search_returns_nearest_chunk_first() {
    let h = common::boot().await;
    let (a, _b) = seed(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg());

    // Query vector very close to chunk_a's seed (0.10) — chunk_a should rank first.
    let body = serde_json::json!({
        "queries": [{
            "text": "alpha-ish content",
            "vector": unit_vector(0.11),
        }],
        "client_embedding_model": "bge-base-en-v1.5@1",
        "limit": 5,
    });
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
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let results = v["results"].as_array().unwrap();
    assert!(!results.is_empty(), "search must return at least one result");
    // Under parallel CI other tests' seed() calls leave chunks with identical
    // vectors in the corpus, so we can't assert this test's chunk_a is at
    // position 0 — only that the top result is at distance ~ that of chunk_a
    // (i.e. a 0.10-seed neighbour) AND that THIS test's chunk_a appears in
    // the result set somewhere.
    let top_sim = results[0]["scores"]["vector_similarity"].as_f64().unwrap();
    assert!(
        top_sim > 0.99,
        "top result must be a 0.10-seed-neighbour of the 0.11 query, got similarity {top_sim}"
    );
    let a_present = results
        .iter()
        .any(|r| r["chunk_id"].as_str() == Some(a.to_string().as_str()));
    assert!(a_present, "this test's chunk_a must appear in the results");
    assert!(v["search_metadata"]["per_query"].is_array());
}

#[tokio::test]
async fn search_returns_409_on_model_mismatch() {
    let h = common::boot().await;
    let _ = seed(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg());

    let body = serde_json::json!({
        "queries": [{ "text": "x", "vector": unit_vector(0.0) }],
        "client_embedding_model": "bge-small-en-v1.5@1",
        "limit": 5,
    });
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
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "embedding_model_mismatch");
    assert_eq!(v["error"]["context"]["corpus_model"], "bge-base-en-v1.5@1");
    assert_eq!(v["error"]["context"]["client_model"], "bge-small-en-v1.5@1");
}

#[tokio::test]
async fn search_returns_400_on_wrong_dim() {
    let h = common::boot().await;
    let _ = seed(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg());

    let body = serde_json::json!({
        "queries": [{ "text": "x", "vector": vec![0.0_f32; 128] }],
        "client_embedding_model": "bge-base-en-v1.5@1",
        "limit": 5,
    });
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
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn search_returns_400_on_empty_queries() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg());

    let body = serde_json::json!({
        "queries": [],
        "client_embedding_model": "bge-base-en-v1.5@1",
    });
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
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn search_respects_limit_cap() {
    let h = common::boot().await;
    let _ = seed(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg());

    let body = serde_json::json!({
        "queries": [{ "text": "x", "vector": unit_vector(0.5) }],
        "client_embedding_model": "bge-base-en-v1.5@1",
        "limit": 1,
    });
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
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["results"].as_array().unwrap().len(), 1);
}

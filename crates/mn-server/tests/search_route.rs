//! Integration tests for `POST /v1/search`.
//!
//! Seeds chunks with synthetic vectors and content, then exercises hybrid
//! FTS + pgvector retrieval and the RRF fusion across modes and queries.
//! Real-embedding tests land alongside mn-embedding in a later phase.

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
        // Tests bypass the boot-time resolver so we pin the corpus model
        // explicitly. Matches the seeded `embedding_model` row in migration 0006.
        corpus_model: Some("bge-base-en-v1.5@1".to_owned()),
        ..Default::default()
    }
}

#[tokio::test]
async fn search_returns_nearest_chunk_first() {
    let h = common::boot().await;
    let (a, _b) = seed(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    // Query vector very close to chunk_a's seed (0.10) — chunk_a should rank
    // first. `limit` is generous because parallel CI shares one schema: every
    // other `seed()` call leaves a chunk with the SAME 0.10 vector AND the same
    // content, so the FTS + vector candidate pool fills with indistinguishable
    // ties. A small limit could truncate this test's own chunk_a out of the
    // fused top-N purely by tie-break order; 100 keeps every true neighbour.
    let body = serde_json::json!({
        "queries": [{
            "text": "alpha-ish content",
            "vector": unit_vector(0.11),
        }],
        "client_embedding_model": "bge-base-en-v1.5@1",
        "limit": 100,
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
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

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
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

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
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

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
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

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

/// Seed one active source_version with two chunks: an *embedded* chunk with a
/// unique vector seed (no FTS-matching tokens) and an *FTS-only* chunk with a
/// `NULL` embedding whose content carries a globally-rare token. Returns
/// `(embedded_chunk_id, fts_only_chunk_id)`.
async fn seed_hybrid(pool: &sqlx::PgPool) -> (Uuid, Uuid) {
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("search-hybrid-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Hybrid", SourceKind::DocsSite, None, 5)
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
    let cn_a = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "a", 0)
        .await
        .unwrap();
    let cn_b = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "b", 1)
        .await
        .unwrap();

    // Embedded chunk: unique vector seed (0.4242), content has no FTS tokens
    // shared with the test queries — so it is only ever vector-matched.
    let embedded = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: cn_a,
            chunk_index: 0,
            total_chunks: 2,
            content: "the quick brown fox vector chunk",
            content_hash: "he",
            embedding: Some(unit_vector(0.4242)),
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

    // FTS-only chunk: NULL embedding (the vector query filters
    // `embedding IS NOT NULL`, so it can ONLY surface via the FTS half) and a
    // globally-rare token so `websearch_to_tsquery('english', 'zzqxftsonly')`
    // matches just this chunk family.
    let fts_only = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: cn_b,
            chunk_index: 1,
            total_chunks: 2,
            content: "zzqxftsonly rare token document",
            content_hash: "hf",
            embedding: None,
            embedding_model_id: model_id,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 41,
            end_byte: 80,
            token_count: 5,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .unwrap();

    source_version::finalize(pool, sv_id).await.unwrap();
    (embedded, fts_only)
}

async fn post_search(app: axum::Router, body: serde_json::Value) -> serde_json::Value {
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
    let body = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn fts_only_chunk_appears_via_hybrid_union() {
    let h = common::boot().await;
    let (_embedded, fts_only) = seed_hybrid(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    // Text matches the FTS-only chunk; vector targets the embedded chunk. The
    // FTS-only chunk has a NULL embedding, so it can only appear via the FTS
    // half of hybrid retrieval.
    let v = post_search(
        app,
        serde_json::json!({
            "queries": [{ "text": "zzqxftsonly", "vector": unit_vector(0.4243) }],
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 50,
        }),
    )
    .await;

    let results = v["results"].as_array().unwrap();
    let fts_str = fts_only.to_string();
    let r = results
        .iter()
        .find(|r| r["chunk_id"].as_str() == Some(fts_str.as_str()))
        .expect("FTS-only chunk (NULL embedding) must appear via the FTS half of hybrid retrieval");

    // It was found only by the FTS half of query 0.
    assert_eq!(r["scores"]["matched_queries"].as_array().unwrap(), &vec![serde_json::json!(0)]);
    // Never vector-matched, so vector_similarity is 0.0.
    assert!((r["scores"]["vector_similarity"].as_f64().unwrap() - 0.0).abs() < 1e-12);
    assert!(r["scores"]["rrf_score"].as_f64().unwrap() > 0.0);

    let pq = &v["search_metadata"]["per_query"][0];
    assert!(pq["fts_candidates"].as_u64().unwrap() >= 1);
    assert!(pq["fts_latency_ms"].is_number());
    assert!(pq["vector_candidates"].is_number());
}

#[tokio::test]
async fn matched_queries_reflects_contributing_queries() {
    let h = common::boot().await;
    let (embedded, fts_only) = seed_hybrid(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    // q0 FTS-matches the FTS-only chunk; both queries vector-match the embedded
    // chunk (their vectors bracket its 0.4242 seed). q1's text matches nothing.
    let v = post_search(
        app,
        serde_json::json!({
            "queries": [
                { "text": "zzqxftsonly",       "vector": unit_vector(0.4243) },
                { "text": "nomatchtoken99zz",  "vector": unit_vector(0.4244) },
            ],
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 50,
        }),
    )
    .await;

    let results = v["results"].as_array().unwrap();
    let emb_str = embedded.to_string();
    let emb = results
        .iter()
        .find(|r| r["chunk_id"].as_str() == Some(emb_str.as_str()))
        .expect("embedded chunk must appear");
    // Vector-matched by BOTH distinct queries.
    assert_eq!(
        emb["scores"]["matched_queries"].as_array().unwrap(),
        &vec![serde_json::json!(0), serde_json::json!(1)]
    );

    let fts_str = fts_only.to_string();
    let fts = results
        .iter()
        .find(|r| r["chunk_id"].as_str() == Some(fts_str.as_str()))
        .expect("FTS-only chunk must appear");
    // Only query 0 contributed (via FTS).
    assert_eq!(
        fts["scores"]["matched_queries"].as_array().unwrap(),
        &vec![serde_json::json!(0)]
    );

    // One per-query record per distinct query, each carrying the FTS fields.
    let pq = v["search_metadata"]["per_query"].as_array().unwrap();
    assert_eq!(pq.len(), 2);
    for (i, rec) in pq.iter().enumerate() {
        assert_eq!(rec["query_index"].as_u64().unwrap(), i as u64);
        assert!(rec["fts_candidates"].is_number());
        assert!(rec["fts_latency_ms"].is_number());
        assert!(rec["vector_candidates"].is_number());
        assert!(rec["vector_latency_ms"].is_number());
    }
}

#[tokio::test]
async fn results_carry_rrf_score_in_descending_order() {
    let h = common::boot().await;
    let _ = seed_hybrid(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let v = post_search(
        app,
        serde_json::json!({
            "queries": [{ "text": "zzqxftsonly rare", "vector": unit_vector(0.4243) }],
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 50,
        }),
    )
    .await;

    let results = v["results"].as_array().unwrap();
    assert!(!results.is_empty(), "hybrid search must return results");
    let mut prev = f64::INFINITY;
    for r in results {
        let s = r["scores"]["rrf_score"].as_f64().unwrap();
        assert!(s <= prev + 1e-12, "rrf_score must be non-increasing: {s} > {prev}");
        prev = s;
    }
}

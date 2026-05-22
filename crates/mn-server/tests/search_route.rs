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
use mn_core::provenance::{Attribution, ContentType, LanguageTarget, Provenance, SdkDependency};
use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, PackageKind, SourceKind};
use mn_server::{app, config::ServerConfig};
use mn_store::entities::{chunk, document, embedding_model, node, package, source, source_version};
use time::{Duration, OffsetDateTime};
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

/// Seed one active source_version with two chunks that share an identical
/// vector and a globally-rare FTS token (so retrieval relevance is the same for
/// both), but live under documents with very different provenance:
/// `high` (Foundation, verified, fresh) and `low` (Unknown, unverified, stale).
/// Returns `(high_chunk_id, low_chunk_id, rare_token)`.
async fn seed_scored(pool: &sqlx::PgPool) -> (Uuid, Uuid, String) {
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("search-scored-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Scored", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, "0.1.0", "h")
        .await
        .unwrap();
    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();

    // A token unique to this seed call so only these two chunks FTS-match it,
    // immune to parallel-CI pollution from other seeds.
    let token = format!("scoretok{}", Uuid::new_v4().simple());
    let content = format!("{token} shared scoring content");
    let vector = unit_vector(0.314);

    let high_prov = Provenance {
        attribution: Attribution::Foundation,
        verified: true,
        verified_by: Some("midnight-foundation".into()),
        language_targets: vec![LanguageTarget {
            name: "compact".into(),
            version_constraint: Some(">=0.23".into()),
        }],
        ..Provenance::default()
    };
    let low_prov = Provenance {
        attribution: Attribution::Unknown,
        verified: false,
        ..Provenance::default()
    };

    let high = seed_scored_chunk(
        pool,
        sv_id,
        root,
        model_id,
        "high.md",
        &high_prov,
        Some(OffsetDateTime::now_utc() - Duration::days(14)),
        &content,
        &vector,
        0,
    )
    .await;
    let low = seed_scored_chunk(
        pool,
        sv_id,
        root,
        model_id,
        "low.md",
        &low_prov,
        Some(OffsetDateTime::now_utc() - Duration::days(800)),
        &content,
        &vector,
        1,
    )
    .await;

    source_version::finalize(pool, sv_id).await.unwrap();
    (high, low, token)
}

#[allow(clippy::too_many_arguments)]
async fn seed_scored_chunk(
    pool: &sqlx::PgPool,
    sv_id: Uuid,
    root: Uuid,
    model_id: Uuid,
    name: &str,
    provenance: &Provenance,
    source_modified_at: Option<OffsetDateTime>,
    content: &str,
    vector: &[f32],
    order: i32,
) -> Uuid {
    let doc_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, name, order)
        .await
        .unwrap();
    let doc_id = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: None,
            source_path: name,
            language: Some("en"),
            content_hash: name,
            source_modified_at,
            frontmatter: None,
            provenance,
            package_id: None,
            char_count: 0,
            token_count: 0,
        },
    )
    .await
    .unwrap();
    let chunk_node = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "c", 0)
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
            content,
            content_hash: name,
            embedding: Some(vector.to_vec()),
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
    .unwrap()
}

/// Seed one fully-tagged chunk for exercising every filter dimension. The chunk
/// is Foundation-attributed, verified, content_type=tutorial, targets compact
/// `>=0.23`, depends on npm `@midnight-ntwrk/midnight-js >=1.0.0`, and belongs
/// to a rust package. Returns `(chunk_id, source_slug, package_name, rare_token)`.
async fn seed_filter_fixture(pool: &sqlx::PgPool) -> (Uuid, String, String, String) {
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("filter-fixture-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Filter", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, "0.1.0", "h")
        .await
        .unwrap();
    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let pkg_name = format!("pkg-{}", Uuid::new_v4().simple());
    let package_id =
        package::upsert(pool, sv_id, PackageKind::Rust, &pkg_name, Some("1.0.0"), None)
            .await
            .unwrap();
    let doc_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, "doc.md", 0)
        .await
        .unwrap();
    let provenance = Provenance {
        attribution: Attribution::Foundation,
        verified: true,
        verified_by: Some("midnight-foundation".into()),
        content_type: ContentType::Tutorial,
        language_targets: vec![LanguageTarget {
            name: "compact".into(),
            version_constraint: Some(">=0.23".into()),
        }],
        sdk_dependencies: vec![SdkDependency {
            kind: "npm".into(),
            name: "@midnight-ntwrk/midnight-js".into(),
            version_constraint: Some(">=1.0.0".into()),
        }],
        ..Provenance::default()
    };
    let token = format!("filtertok{}", Uuid::new_v4().simple());
    let content = format!("{token} filter fixture content");
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
            source_modified_at: Some(OffsetDateTime::now_utc() - Duration::days(10)),
            frontmatter: None,
            provenance: &provenance,
            package_id: Some(package_id),
            char_count: 0,
            token_count: 0,
        },
    )
    .await
    .unwrap();
    let chunk_node = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "c", 0)
        .await
        .unwrap();
    let chunk_id = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node,
            chunk_index: 0,
            total_chunks: 1,
            content: &content,
            content_hash: "hc",
            embedding: Some(unit_vector(0.271)),
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
    source_version::finalize(pool, sv_id).await.unwrap();
    (chunk_id, slug, pkg_name, token)
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

#[tokio::test]
async fn convenience_form_is_accepted_end_to_end() {
    // Acceptance #6: the single-query `{query, vector}` form is processed the
    // same as `queries: [{text, vector}]`. (The strict byte-identical guarantee
    // is covered by the `normalize_queries` unit test; here we confirm the route
    // accepts the shape end-to-end and returns the expected chunk.)
    let h = common::boot().await;
    let (_embedded, fts_only) = seed_hybrid(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let v = post_search(
        app,
        serde_json::json!({
            "query": "zzqxftsonly",
            "vector": unit_vector(0.4243),
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 50,
        }),
    )
    .await;

    let results = v["results"].as_array().unwrap();
    let fts_str = fts_only.to_string();
    assert!(
        results
            .iter()
            .any(|r| r["chunk_id"].as_str() == Some(fts_str.as_str())),
        "convenience form must drive the same hybrid retrieval as the canonical form"
    );
    assert_eq!(v["search_metadata"]["per_query"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn results_carry_confidence_fields() {
    // Acceptance #1 / #7: every result carries trust_score, confidence, and a
    // confidence_factors breakdown whose relevance_source is "rrf" on the cloud.
    let h = common::boot().await;
    let (_high, _low, token) = seed_scored(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let v = post_search(
        app,
        serde_json::json!({
            "query": token,
            "vector": unit_vector(0.314),
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 50,
        }),
    )
    .await;

    let results = v["results"].as_array().unwrap();
    assert!(!results.is_empty());
    for r in results {
        let s = &r["scores"];
        let trust = s["trust_score"].as_f64().unwrap();
        let conf = s["confidence"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&trust), "trust out of range: {trust}");
        assert!((0.0..=1.0).contains(&conf), "confidence out of range: {conf}");
        assert_eq!(s["confidence_factors"]["relevance_source"], "rrf");
        assert!(s["confidence_factors"]["attribution"].is_string());
        assert!(s["confidence_factors"]["age_days"].is_number());
    }
    assert!(v["search_metadata"]["filtered_by_confidence"].is_number());
}

#[tokio::test]
async fn higher_trust_outranks_under_default_confidence_sort() {
    // Acceptance #2/#9: with identical relevance, the Foundation+verified+fresh
    // chunk outranks the Unknown+unverified+stale one under the default
    // confidence sort.
    let h = common::boot().await;
    let (high, low, token) = seed_scored(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let v = post_search(
        app,
        serde_json::json!({
            "query": token,
            "vector": unit_vector(0.314),
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 50,
        }),
    )
    .await;

    let results = v["results"].as_array().unwrap();
    let pos = |id: Uuid| {
        results
            .iter()
            .position(|r| r["chunk_id"].as_str() == Some(id.to_string().as_str()))
    };
    let hi = pos(high).expect("high-trust chunk present");
    let lo = pos(low).expect("low-trust chunk present");
    assert!(hi < lo, "high-trust chunk (idx {hi}) must rank above low-trust (idx {lo})");

    let hi_conf = results[hi]["scores"]["confidence"].as_f64().unwrap();
    let lo_conf = results[lo]["scores"]["confidence"].as_f64().unwrap();
    assert!(hi_conf > lo_conf, "high confidence {hi_conf} must exceed low {lo_conf}");
    let hi_factors = &results[hi]["scores"]["confidence_factors"];
    assert!(hi_factors["verified"].as_bool().unwrap());
}

#[tokio::test]
async fn version_match_boost_applies_with_filter() {
    // Acceptance #6: a language_target filter with a satisfied version lifts the
    // matching chunk's version_match_multiplier above neutral.
    let h = common::boot().await;
    let (high, _low, token) = seed_scored(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let v = post_search(
        app,
        serde_json::json!({
            "query": token,
            "vector": unit_vector(0.314),
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 50,
            "filters": { "language_target": { "name": "compact", "version_constraint_satisfies": "0.31" } },
        }),
    )
    .await;

    let results = v["results"].as_array().unwrap();
    let hi = results
        .iter()
        .find(|r| r["chunk_id"].as_str() == Some(high.to_string().as_str()))
        .expect("high-trust chunk present");
    let factors = &hi["scores"]["confidence_factors"];
    let vmm = factors["version_match_multiplier"].as_f64().unwrap();
    assert!(vmm > 1.0, "satisfied version should boost (>1.0), got {vmm}");
    assert_eq!(factors["language_target_query"]["version_constraint_satisfies"], "0.31");
}

#[tokio::test]
async fn min_confidence_filters_before_limit() {
    // Acceptance #10: an unreachable confidence floor drops every candidate and
    // reports the count in filtered_by_confidence.
    let h = common::boot().await;
    let (_high, _low, token) = seed_scored(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let v = post_search(
        app,
        serde_json::json!({
            "query": token,
            "vector": unit_vector(0.314),
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 50,
            // Confidence is strictly < 1.0 (the relevance term never reaches 1),
            // so a floor of 1.0 filters everything.
            "min_confidence": 1.0,
        }),
    )
    .await;

    assert!(v["results"].as_array().unwrap().is_empty());
    let filtered = v["search_metadata"]["filtered_by_confidence"]
        .as_u64()
        .unwrap();
    let total = v["search_metadata"]["total_candidates"].as_u64().unwrap();
    assert!(filtered > 0, "expected some filtered, got {filtered}");
    assert_eq!(filtered, total, "all candidates should be filtered by the 1.0 floor");
}

#[tokio::test]
async fn include_scores_false_omits_scores() {
    // The scores object is omitted when include_scores=false, but ranking still
    // happens server-side.
    let h = common::boot().await;
    let (_high, _low, token) = seed_scored(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let v = post_search(
        app,
        serde_json::json!({
            "query": token,
            "vector": unit_vector(0.314),
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 50,
            "include_scores": false,
        }),
    )
    .await;

    let results = v["results"].as_array().unwrap();
    assert!(!results.is_empty());
    for r in results {
        assert!(r.get("scores").is_none(), "scores must be omitted");
        assert!(r["chunk_id"].is_string());
    }
}

/// Whether `results` contains a chunk with the given id.
fn contains_chunk(v: &serde_json::Value, id: Uuid) -> bool {
    v["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["chunk_id"].as_str() == Some(id.to_string().as_str()))
}

/// Build a fresh app and POST a search body (each `oneshot` consumes the app).
async fn run_filtered(
    pool: &sqlx::PgPool,
    token: &str,
    filters: serde_json::Value,
) -> serde_json::Value {
    let app = app::build(pool.clone(), cfg()).expect("build app");
    post_search(
        app,
        serde_json::json!({
            "query": token,
            "vector": unit_vector(0.271),
            "client_embedding_model": "bge-base-en-v1.5@1",
            "limit": 100,
            "filters": filters,
        }),
    )
    .await
}

#[tokio::test]
async fn scalar_filters_include_and_exclude() {
    // Acceptance #11: attribution / verified / content_type / source_slug /
    // package each restrict results, AND across keys.
    let h = common::boot().await;
    let (chunk, slug, pkg, token) = seed_filter_fixture(&h.pool).await;

    // Matching values include the chunk.
    assert!(contains_chunk(
        &run_filtered(&h.pool, &token, serde_json::json!({ "attribution": ["foundation"] })).await,
        chunk
    ));
    assert!(contains_chunk(
        &run_filtered(&h.pool, &token, serde_json::json!({ "verified": true })).await,
        chunk
    ));
    assert!(contains_chunk(
        &run_filtered(&h.pool, &token, serde_json::json!({ "content_type": ["tutorial"] })).await,
        chunk
    ));
    assert!(contains_chunk(
        &run_filtered(&h.pool, &token, serde_json::json!({ "source_slug": [slug] })).await,
        chunk
    ));
    assert!(contains_chunk(
        &run_filtered(
            &h.pool,
            &token,
            serde_json::json!({ "package": [{ "kind": "rust", "name": pkg }] })
        )
        .await,
        chunk
    ));

    // Non-matching values exclude it.
    assert!(!contains_chunk(
        &run_filtered(
            &h.pool,
            &token,
            serde_json::json!({ "attribution": ["partner", "community"] })
        )
        .await,
        chunk
    ));
    assert!(!contains_chunk(
        &run_filtered(&h.pool, &token, serde_json::json!({ "verified": false })).await,
        chunk
    ));
    assert!(!contains_chunk(
        &run_filtered(&h.pool, &token, serde_json::json!({ "content_type": ["doc"] })).await,
        chunk
    ));
    assert!(!contains_chunk(
        &run_filtered(&h.pool, &token, serde_json::json!({ "source_slug": ["some-other-slug"] }))
            .await,
        chunk
    ));
    assert!(!contains_chunk(
        &run_filtered(
            &h.pool,
            &token,
            serde_json::json!({ "package": [{ "kind": "rust", "name": "nonexistent-pkg" }] })
        )
        .await,
        chunk
    ));
}

#[tokio::test]
async fn and_across_keys_excludes_on_any_miss() {
    // AND semantics: a matching attribution but a mismatched content_type still
    // excludes the chunk.
    let h = common::boot().await;
    let (chunk, _slug, _pkg, token) = seed_filter_fixture(&h.pool).await;
    let v = run_filtered(
        &h.pool,
        &token,
        serde_json::json!({ "attribution": ["foundation"], "content_type": ["doc"] }),
    )
    .await;
    assert!(!contains_chunk(&v, chunk), "AND: a single failing key must exclude");
}

#[tokio::test]
async fn semver_filters_language_target_and_sdk_dependency() {
    // Acceptance #11 / FR-033: version_constraint_satisfies is evaluated
    // server-side for language_target and sdk_dependency.
    let h = common::boot().await;
    let (chunk, _slug, _pkg, token) = seed_filter_fixture(&h.pool).await;

    // language_target: chunk targets compact >=0.23.
    assert!(contains_chunk(&run_filtered(&h.pool, &token, serde_json::json!({ "language_target": { "name": "compact", "version_constraint_satisfies": "0.31" } })).await, chunk));
    assert!(!contains_chunk(&run_filtered(&h.pool, &token, serde_json::json!({ "language_target": { "name": "compact", "version_constraint_satisfies": "0.10" } })).await, chunk));
    // name mismatch excludes.
    assert!(!contains_chunk(
        &run_filtered(
            &h.pool,
            &token,
            serde_json::json!({ "language_target": { "name": "rust" } })
        )
        .await,
        chunk
    ));

    // sdk_dependency: chunk declares npm @midnight-ntwrk/midnight-js >=1.0.0.
    assert!(contains_chunk(&run_filtered(&h.pool, &token, serde_json::json!({ "sdk_dependency": [{ "kind": "npm", "name": "@midnight-ntwrk/midnight-js", "version_constraint_satisfies": "1.4.0" }] })).await, chunk));
    assert!(!contains_chunk(&run_filtered(&h.pool, &token, serde_json::json!({ "sdk_dependency": [{ "kind": "npm", "name": "@midnight-ntwrk/midnight-js", "version_constraint_satisfies": "0.9.0" }] })).await, chunk));
}

#[tokio::test]
async fn search_returns_400_when_all_text_empty() {
    // Acceptance #7: every query with empty/whitespace text is rejected.
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");

    let body = serde_json::json!({
        "queries": [
            { "text": "", "vector": unit_vector(0.1) },
            { "text": "   ", "vector": unit_vector(0.2) },
        ],
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
    let body = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "invalid_request");
}

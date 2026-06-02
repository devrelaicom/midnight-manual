//! Integration test for the corpus-model candidate filter on `POST /v1/search`.
//!
//! Proves that chunks belonging to a source_version encoded with a *different*
//! embedding model are excluded from results, even when that source_version is
//! active and the chunk would otherwise match the query. The corpus model is
//! pinned explicitly (via `build_with_limiter`) and search filters candidates
//! by `sv.embedding_model_id`.

#![cfg(feature = "integration")]
#![allow(
    missing_docs,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_precision_loss,
    clippy::suboptimal_flops
)]

mod common;

use std::sync::{Arc, RwLock};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use mn_core::provenance::Provenance;
use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mn_server::corpus_model::CorpusModel;
use mn_server::{app, config::ServerConfig};
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use tower::ServiceExt;
use uuid::Uuid;

fn unit_vector(seed: f32) -> Vec<f32> {
    // Deterministic 768-dim vector for tests. Both models in this test use dim
    // 768 so the same shape is valid for either; the filter — not the dim —
    // is what excludes the off-model chunk.
    #[allow(clippy::cast_precision_loss)]
    (0..768_i32).map(|i| seed + (i as f32) * 0.0001).collect()
}

/// Seed one active source_version + one ready chunk on `model_id`, under a
/// fresh source so two active versions on different models can coexist (the
/// active-version uniqueness constraint is per-source). Both chunks share the
/// same rare FTS token + vector seed so they are equally retrievable; only the
/// corpus-model filter distinguishes them. Returns the chunk id. The caller
/// controls finalize ordering via this returned id's source_version.
async fn seed_on_model(
    pool: &sqlx::PgPool,
    model_id: Uuid,
    slug_prefix: &str,
    token: &str,
    vector: &[f32],
) -> (Uuid, Uuid) {
    let slug = format!("{slug_prefix}-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Filter", SourceKind::DocsSite, None, 5)
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
    let chunk_node = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "c", 0)
        .await
        .unwrap();
    let content = format!("{token} shared retrievable content");
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
    .unwrap();
    (sv_id, chunk_id)
}

#[tokio::test]
async fn off_model_chunks_excluded_from_results() {
    let h = common::boot().await;

    // Corpus model (bge, 768) and a second model on a different id, also 768 so
    // the dim guard can't be what excludes it — the model-id filter must.
    let bge_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let other_id = embedding_model::upsert(&h.pool, "other-model", 1, 768, "other")
        .await
        .unwrap();

    // Identical token + vector so both chunks would surface via FTS *and*
    // vector retrieval if the model filter were absent.
    let token = format!("modelfiltertok{}", Uuid::new_v4().simple());
    let vector = unit_vector(0.357);

    // Both source_versions are finalized (active) so the off-model chunk is a
    // genuine candidate that only the model-id filter — not `is_active` — can
    // exclude. Finalize order is irrelevant here: the corpus model is pinned
    // explicitly below, so the test does not depend on `get_active` tie-breaking.
    let (off_sv, off_chunk) =
        seed_on_model(&h.pool, other_id, "model-filter-off", &token, &vector).await;
    source_version::finalize(&h.pool, off_sv).await.unwrap();

    let (on_sv, on_chunk) =
        seed_on_model(&h.pool, bge_id, "model-filter-on", &token, &vector).await;
    source_version::finalize(&h.pool, on_sv).await.unwrap();

    // Pin the corpus model explicitly to bge so this test exercises the
    // model-id filter deterministically, independent of `get_active` ordering.
    let corpus_model = Arc::new(RwLock::new(Some(CorpusModel {
        wire: "bge-base-en-v1.5@1".to_owned(),
        id: bge_id,
        dim: 768,
    })));
    let cfg = ServerConfig::default();
    let limiter = mn_server::ratelimit::RateLimiter::from_config(&cfg);
    let app =
        app::build_with_limiter(h.pool.clone(), cfg, limiter, corpus_model).expect("build app");

    let body = serde_json::json!({
        "query": token,
        "vector": vector,
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
    let body = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let results = v["results"].as_array().unwrap();

    let on_present = results
        .iter()
        .any(|r| r["chunk_id"].as_str() == Some(on_chunk.to_string().as_str()));
    let off_present = results
        .iter()
        .any(|r| r["chunk_id"].as_str() == Some(off_chunk.to_string().as_str()));

    assert!(on_present, "on-model chunk must appear in results");
    assert!(
        !off_present,
        "off-model chunk (different embedding_model_id) must be filtered out"
    );
}

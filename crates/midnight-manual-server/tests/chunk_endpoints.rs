//! Integration tests for `GET /v1/chunks?ids=` + `/v1/chunks/:id` + `/parents`.

#![cfg(feature = "integration")]
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::missing_const_for_fn
)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use midnight_manual_server::{app, config::ServerConfig};
use mnm_core::provenance::Provenance;
use mnm_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mnm_store::entities::{chunk, document, embedding_model, node, source, source_version};
use tower::ServiceExt;
use uuid::Uuid;

struct Seed {
    /// Source slug (unique per test run).
    slug: String,
    /// The seeded document's id.
    doc_id: Uuid,
    /// First chunk id (`chunk_index` 0).
    chunk_a: Uuid,
    /// Second chunk id (`chunk_index` 1).
    chunk_b: Uuid,
}

async fn seed_two_chunks(pool: &sqlx::PgPool) -> Seed {
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("chunks-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Chunks", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, None, "0.1.0", "h")
        .await
        .unwrap();
    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let group = node::insert(pool, sv_id, Some(root), NodeKind::Group, "guides", 0)
        .await
        .unwrap();
    let doc_node = node::insert(pool, sv_id, Some(group), NodeKind::Document, "doc.md", 0)
        .await
        .unwrap();
    let prov = Provenance::default();
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
            provenance: &prov,
            package_id: None,
            char_count: 0,
            token_count: 0,
            license: None,
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
    let a = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: cn_a,
            chunk_index: 0,
            total_chunks: 2,
            content: "first",
            content_hash: "ha",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 5,
            token_count: 1,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .unwrap();
    let b = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: cn_b,
            chunk_index: 1,
            total_chunks: 2,
            content: "second",
            content_hash: "hb",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 6,
            end_byte: 12,
            token_count: 1,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .unwrap();
    source_version::finalize(pool, sv_id).await.unwrap();
    Seed {
        slug,
        doc_id,
        chunk_a: a,
        chunk_b: b,
    }
}

fn cfg() -> ServerConfig {
    ServerConfig::default()
}

#[tokio::test]
async fn get_chunk_round_trips() {
    let h = common::boot().await;
    let a = seed_two_chunks(&h.pool).await.chunk_a;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/chunks/{a}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 16 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["id"].as_str().unwrap(), a.to_string());
    assert_eq!(v["content"], "first");
}

#[tokio::test]
async fn get_chunk_returns_404_for_unknown_id() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");
    let id = Uuid::new_v4();
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/chunks/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_chunks_batch_preserves_input_order_and_reports_missing() {
    let h = common::boot().await;
    let seed = seed_two_chunks(&h.pool).await;
    let (a, b) = (seed.chunk_a, seed.chunk_b);
    let app = app::build(h.pool.clone(), cfg()).expect("build app");
    let unknown = Uuid::new_v4();
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/chunks?ids={b},{a},{unknown}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let chunks = v["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 2);
    // Input order preserved: b was requested first.
    assert_eq!(chunks[0]["id"].as_str().unwrap(), b.to_string());
    assert_eq!(chunks[1]["id"].as_str().unwrap(), a.to_string());
    let missing = v["missing"].as_array().unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].as_str().unwrap(), unknown.to_string());
}

#[tokio::test]
async fn get_chunks_batch_rejects_invalid_ids_with_400() {
    let h = common::boot().await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/chunks?ids=not-a-uuid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_chunk_parents_walks_to_root_with_document_ids_and_source() {
    let h = common::boot().await;
    let seed = seed_two_chunks(&h.pool).await;
    let a = seed.chunk_a;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/chunks/{a}/parents"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 16 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Response is an object, not the old bare array.
    assert!(v.is_object());
    let parents = v["parents"].as_array().unwrap();
    // chunk -> doc.md -> guides -> root
    assert_eq!(parents.len(), 3);
    // First entry is the document node carrying the fetchable document id.
    assert_eq!(parents[0]["kind"], "document");
    assert_eq!(parents[0]["name"], "doc.md");
    assert_eq!(parents[0]["document_id"].as_str().unwrap(), seed.doc_id.to_string());
    // Group node has no document id.
    assert_eq!(parents[1]["kind"], "group");
    assert_eq!(parents[1]["name"], "guides");
    assert!(parents[1]["document_id"].is_null());
    // Last entry is the root, with no document id.
    assert_eq!(parents[2]["kind"], "root");
    assert_eq!(parents[2]["name"], "root");
    assert!(parents[2]["document_id"].is_null());
    // Owning source rides along at the top level.
    assert_eq!(v["source"]["slug"].as_str().unwrap(), seed.slug);
    assert_eq!(v["source"]["display_name"], "Chunks");
}

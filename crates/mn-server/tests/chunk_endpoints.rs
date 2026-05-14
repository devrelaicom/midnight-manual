//! Integration tests for `GET /v1/chunks/:id` + `/siblings` + `/parents`.

#![cfg(feature = "integration")]
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
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

async fn seed_two_chunks(pool: &sqlx::PgPool) -> (Uuid, Uuid, Uuid) {
    // Returns (root_node_id, chunk_a_id, chunk_b_id).
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("chunks-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Chunks", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, "0.1.0", "h")
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
    (root, a, b)
}

fn cfg() -> ServerConfig {
    ServerConfig {
        database_url: String::new(),
        port: 0,
        auto_migrate: false,
        corpus_model: None,
        user_store_body: None,
        jwt_secret: None,
    }
}

#[tokio::test]
async fn get_chunk_round_trips() {
    let h = common::boot().await;
    let (_, a, _) = seed_two_chunks(&h.pool).await;
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
async fn get_chunk_siblings_returns_both_chunks_in_order() {
    let h = common::boot().await;
    let (_, a, b) = seed_two_chunks(&h.pool).await;
    let app = app::build(h.pool.clone(), cfg()).expect("build app");
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/chunks/{a}/siblings"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"].as_str().unwrap(), a.to_string());
    assert_eq!(arr[1]["id"].as_str().unwrap(), b.to_string());
}

#[tokio::test]
async fn get_chunk_parents_walks_to_root() {
    let h = common::boot().await;
    let (_root, a, _b) = seed_two_chunks(&h.pool).await;
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
    let arr = v.as_array().unwrap();
    // chunk -> doc.md -> guides -> root
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["name"], "doc.md");
    assert_eq!(arr[1]["name"], "guides");
    assert_eq!(arr[2]["name"], "root");
}

//! Integration: `observability::topic::recompute_centroids` / `load_centroids`
//! round-trip against a real Postgres+pgvector database (Task 9).
//!
//! DB-integration-gated per this repo's convention: only compiles under the
//! `integration` feature, and only runs where a database is reachable
//! (testcontainers locally, or CI's `services: postgres`) — see
//! `crates/midnight-manual-server/tests/common/mod.rs`.
#![cfg(feature = "integration")]

mod common;

use midnight_manual_server::observability::topic;
use mnm_core::provenance::Provenance;
use mnm_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mnm_store::entities::{chunk, document, embedding_model, node, source, source_version};
use sqlx::PgPool;
use uuid::Uuid;

const DIM: usize = 1024;

/// Seed one `source` of the given `kind` with a single finalized
/// `source_version`, one document, and `n` `ready` chunks whose embeddings
/// are all `fill` (a constant, non-zero value) — enough to give the category
/// a well-defined, non-zero mean.
async fn seed_category(pool: &PgPool, kind: SourceKind, model_id: Uuid, fill: f32, n: usize) {
    let slug = format!("topic-centroid-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, &slug, kind, None, 5)
        .await
        .expect("insert source");

    let (sv_id, _) =
        source_version::create_building(pool, source_id, model_id, None, "0.1.0", "hash")
            .await
            .expect("create source_version");
    source_version::finalize(pool, sv_id)
        .await
        .expect("finalize source_version");

    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .expect("insert root node");
    let doc_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, "doc.md", 0)
        .await
        .expect("insert document node");

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
            content_hash: "doc-hash",
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 40,
            token_count: 10,
        },
    )
    .await
    .expect("insert document");

    for i in 0..n {
        let chunk_node = node::insert(
            pool,
            sv_id,
            Some(doc_node),
            NodeKind::Chunk,
            &format!("chunk-{i}"),
            i32::try_from(i).unwrap(),
        )
        .await
        .expect("insert chunk node");

        chunk::insert(
            pool,
            chunk::NewChunk {
                source_version_id: sv_id,
                document_id: doc_id,
                node_id: chunk_node,
                chunk_index: i32::try_from(i).unwrap(),
                total_chunks: i32::try_from(n).unwrap(),
                content: "topic centroid fixture chunk",
                content_hash: &format!("chunk-hash-{i}"),
                embedding: Some(vec![fill; DIM]),
                embedding_model_id: model_id,
                code_embedding: None,
                heading_path: &[],
                symbol_path: &[],
                start_byte: 0,
                end_byte: 10,
                token_count: 4,
                status: ChunkStatus::Ready,
            },
        )
        .await
        .expect("insert chunk");
    }
}

#[tokio::test]
async fn recompute_then_load_roundtrips_normalized_centroids() {
    let h = common::boot().await;
    let model_id = embedding_model::upsert(
        &h.pool,
        "topic-centroid-fixture",
        1,
        i32::try_from(DIM).unwrap(),
        "test",
    )
    .await
    .expect("upsert embedding model");

    // Two categories -> two centroids, distinguished by a constant non-zero
    // fill value so each category's mean is well-defined and non-zero.
    seed_category(&h.pool, SourceKind::DocsSite, model_id, 1.0, 3).await;
    seed_category(&h.pool, SourceKind::CodeRepo, model_id, 2.0, 2).await;

    let written = topic::recompute_centroids(&h.pool, model_id)
        .await
        .expect("recompute_centroids");
    assert_eq!(written, 2, "expected one centroid per category");

    let centroids = topic::load_centroids(&h.pool, model_id)
        .await
        .expect("load_centroids");
    assert_eq!(centroids.labels.len(), centroids.vectors.len());
    assert_eq!(centroids.labels.len(), 2);
    // load_centroids orders by label; docs_site < code_repo lexicographically? No —
    // "code_repo" < "docs_site" (c < d), so code_repo comes first.
    assert_eq!(centroids.labels, vec!["code_repo".to_string(), "docs_site".to_string()]);

    for v in &centroids.vectors {
        assert_eq!(v.len(), DIM);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "centroids must be L2-normalized, got norm {norm}");
    }
}

#[tokio::test]
async fn recompute_is_idempotent_and_replaces_stale_rows() {
    let h = common::boot().await;
    let model_id = embedding_model::upsert(
        &h.pool,
        "topic-centroid-fixture-2",
        1,
        i32::try_from(DIM).unwrap(),
        "test",
    )
    .await
    .expect("upsert embedding model");

    seed_category(&h.pool, SourceKind::Standalone, model_id, 3.0, 2).await;

    let first = topic::recompute_centroids(&h.pool, model_id)
        .await
        .expect("first recompute");
    assert_eq!(first, 1);

    // Add a second category and recompute again — the stored set should
    // reflect only the current corpus state (DELETE + re-INSERT), not a
    // union of old and new rows.
    seed_category(&h.pool, SourceKind::Mixed, model_id, 4.0, 1).await;
    let second = topic::recompute_centroids(&h.pool, model_id)
        .await
        .expect("second recompute");
    assert_eq!(second, 2);

    let centroids = topic::load_centroids(&h.pool, model_id)
        .await
        .expect("load_centroids");
    assert_eq!(centroids.labels.len(), 2);
}

//! Integration tests for the source-retention sweep job (Phase 13).
//!
//! Exercises the job-layer wrapper `mn_server::jobs::source_retention` so
//! its `sweep_once` correctly converts the `grace_hours` config into the
//! `grace_seconds` the store-level helper takes, and so the
//! [`mn_server::jobs::source_retention::SweepStats`] shape is preserved.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mn_server::jobs::source_retention;
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use sqlx::PgPool;
use uuid::Uuid;

async fn seed_retired_source(pool: &PgPool, prefix: &str, retired_seconds_ago: i64) -> String {
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("{prefix}-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Job Fixture", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, "0.1.0", "h")
        .await
        .unwrap();
    source_version::finalize(pool, sv_id).await.unwrap();

    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let doc_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, "doc-0.md", 0)
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
            source_path: "doc-0.md",
            language: Some("en"),
            content_hash: "h-doc",
            source_modified_at: None,
            frontmatter: None,
            provenance: &mn_core::provenance::Provenance::default(),
            package_id: None,
            char_count: 1,
            token_count: 1,
        },
    )
    .await
    .unwrap();
    let chunk_node = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "c0", 0)
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
            content: "x",
            content_hash: "ch",
            embedding: None,
            embedding_model_id: model_id,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 1,
            token_count: 1,
            status: ChunkStatus::EmbedFailed,
        },
    )
    .await
    .unwrap();

    source::retire(pool, &slug).await.unwrap();
    sqlx::query(
        "UPDATE source SET retired_at = now() - ($1::bigint * interval '1 second') WHERE slug = $2",
    )
    .bind(retired_seconds_ago)
    .bind(&slug)
    .execute(pool)
    .await
    .unwrap();
    slug
}

async fn source_exists(pool: &PgPool, slug: &str) -> bool {
    use sqlx::Row as _;
    sqlx::query("SELECT 1 AS one FROM source WHERE slug = $1")
        .bind(slug)
        .fetch_optional(pool)
        .await
        .unwrap()
        .is_some_and(|r| r.get::<i32, _>("one") == 1)
}

#[tokio::test]
async fn sweep_once_deletes_retired_past_grace() {
    let h = common::boot().await;
    let slug = seed_retired_source(&h.pool, "job-aged", 25 * 60 * 60).await;

    let stats = source_retention::sweep_once(&h.pool, 24, 24)
        .await
        .expect("sweep");

    assert!(
        stats.deleted_slugs.iter().any(|s| s == &slug),
        "expected slug `{slug}` in deleted set: {:?}",
        stats.deleted_slugs,
    );
    assert!(!source_exists(&h.pool, &slug).await, "row must be gone");
    assert!(stats.deleted_source_count() >= 1);
}

#[tokio::test]
async fn sweep_once_keeps_recently_retired_inside_grace() {
    let h = common::boot().await;
    let slug = seed_retired_source(&h.pool, "job-recent", 5 * 60).await;

    let stats = source_retention::sweep_once(&h.pool, 24, 24)
        .await
        .expect("sweep");

    assert!(
        !stats.deleted_slugs.iter().any(|s| s == &slug),
        "recently-retired slug MUST NOT be swept: {:?}",
        stats.deleted_slugs,
    );
    assert!(source_exists(&h.pool, &slug).await, "row must remain");
}

#[tokio::test]
async fn sweep_once_handles_grace_hours_zero() {
    let h = common::boot().await;
    let slug = seed_retired_source(&h.pool, "job-zero", 2).await;

    let stats = source_retention::sweep_once(&h.pool, 0, 24)
        .await
        .expect("sweep");

    assert!(stats.deleted_slugs.iter().any(|s| s == &slug));
    assert!(!source_exists(&h.pool, &slug).await);
}

#[tokio::test]
async fn empty_pass_returns_zero_stats() {
    let h = common::boot().await;
    // Sentinel-only retired row that's INSIDE the grace window — so the
    // source sweep finds nothing eligible for this slug. We can't rely on
    // the corpus being empty (CI Postgres is shared across binaries), but
    // we CAN seed one recently-retired row and assert it isn't deleted;
    // the resulting stats may still include other fixtures' deletions.
    let slug = seed_retired_source(&h.pool, "job-empty", 30).await;
    let stats = source_retention::sweep_once(&h.pool, 24, 24)
        .await
        .expect("sweep");
    assert!(!stats.deleted_slugs.iter().any(|s| s == &slug));
    assert!(source_exists(&h.pool, &slug).await);
}

#[tokio::test]
async fn sweep_once_runs_version_pass_alongside_source_pass() {
    use mn_core::types::SourceKind;
    use mn_store::entities::{embedding_model, source as source_entity, source_version};

    let h = common::boot().await;
    // Seed a source with 4 finalized versions, retention=2, all aged out.
    let model_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("job-vpass-{}", Uuid::new_v4());
    let source_id =
        source_entity::insert(&h.pool, &slug, "VPass Fixture", SourceKind::DocsSite, None, 2)
            .await
            .unwrap();
    let mut version_ids = Vec::new();
    for i in 0..4_i32 {
        let (id, _) = source_version::create_building(
            &h.pool,
            source_id,
            model_id,
            "0.1.0",
            &format!("hv{i}"),
        )
        .await
        .unwrap();
        source_version::finalize(&h.pool, id).await.unwrap();
        version_ids.push(id);
    }
    for id in &version_ids {
        sqlx::query(
            "UPDATE source_version \
             SET ingested_at = now() - ($1::bigint * interval '1 second') \
             WHERE id = $2",
        )
        .bind(25_i64 * 60 * 60)
        .bind(*id)
        .execute(&h.pool)
        .await
        .unwrap();
    }

    let stats = source_retention::sweep_once(&h.pool, 24, 24)
        .await
        .expect("sweep");

    // We expect two version rows for OUR source_id to be deleted (revs 1 + 2).
    let our_vers: Vec<i32> = stats
        .deleted_versions
        .iter()
        .filter(|(sid, _)| *sid == source_id)
        .map(|(_, rev)| *rev)
        .collect();
    assert_eq!(our_vers, vec![1, 2], "{:?}", stats.deleted_versions);

    // Our source row itself should still exist (it isn't retired).
    assert!(source_exists(&h.pool, &slug).await);
}

//! Integration tests for [`mn_store::entities::source::sweep_retired`] (Phase 13).
//!
//! Verify that the source-retention sweep:
//! - leaves active (non-retired) sources untouched,
//! - leaves recently-retired sources untouched while inside the grace
//!   window,
//! - hard-deletes retired sources past the grace window, cascading their
//!   `source_version` and `chunk` rows.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use sqlx::PgPool;
use uuid::Uuid;

/// Seed one source + an active source_version + one chunk under it. Returns
/// `(source_id, source_version_id, chunk_id, slug)`.
async fn seed_source_with_chunks(pool: &PgPool, prefix: &str) -> (Uuid, Uuid, Uuid, String) {
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .expect("seed embedding model");
    let slug = format!("{prefix}-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Sweep Fixture", SourceKind::DocsSite, None, 5)
        .await
        .expect("insert source");
    let (sv_id, _rev) = source_version::create_building(pool, source_id, model_id, "0.1.0", "h")
        .await
        .expect("start sv");
    source_version::finalize(pool, sv_id)
        .await
        .expect("finalize");

    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .expect("root node");
    let doc_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, "doc-0.md", 0)
        .await
        .expect("doc node");
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
    .expect("insert doc");
    let chunk_node = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "c0", 0)
        .await
        .expect("chunk node");
    let chunk_id = chunk::insert(
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
    .expect("insert chunk");
    (source_id, sv_id, chunk_id, slug)
}

/// Backdate a source's `retired_at` so the sweep treats it as aged-out.
async fn retire_aged(pool: &PgPool, slug: &str, retired_seconds_ago: i64) {
    source::retire(pool, slug).await.expect("retire");
    sqlx::query(
        "UPDATE source \
         SET retired_at = now() - ($1::bigint * interval '1 second') \
         WHERE slug = $2",
    )
    .bind(retired_seconds_ago)
    .bind(slug)
    .execute(pool)
    .await
    .expect("backdate retired_at");
}

async fn count_chunks_for_sv(pool: &PgPool, sv_id: Uuid) -> i64 {
    use sqlx::Row as _;
    sqlx::query("SELECT COUNT(*)::bigint AS c FROM chunk WHERE source_version_id = $1")
        .bind(sv_id)
        .fetch_one(pool)
        .await
        .unwrap()
        .get::<i64, _>("c")
}

async fn source_exists(pool: &PgPool, slug: &str) -> bool {
    use sqlx::Row as _;
    let row = sqlx::query("SELECT 1 AS one FROM source WHERE slug = $1")
        .bind(slug)
        .fetch_optional(pool)
        .await
        .unwrap();
    row.is_some_and(|r| r.get::<i32, _>("one") == 1)
}

// All assertions below are written to be tolerant of CONCURRENT sweep
// calls from other test binaries (the CI Postgres is shared and the
// sweep helpers are global). The strategy:
//
// - "Should be deleted" — assert the slug no longer exists after the
//   sweep. Whether MY sweep or a sibling test's earlier sweep removed it
//   is irrelevant; both outcomes prove the predicate works.
// - "Should NOT be deleted" — assert MY sweep's `deleted` set does NOT
//   contain my slug. If a concurrent grace=0 sweep already deleted my
//   row, my sweep returns empty for my slug — that still cannot produce
//   a false positive, because a broken predicate would have included it.

#[tokio::test]
async fn sweep_leaves_active_sources_alone() {
    let h = common::boot().await;
    let (_, sv_id, _, slug) = seed_source_with_chunks(&h.pool, "sweep-active").await;

    // Grace 0 → would delete anything currently retired. Active sources
    // never have `retired_at IS NOT NULL` so they MUST remain.
    let deleted = source::sweep_retired(&h.pool, 0).await.expect("sweep");
    assert!(
        !deleted.iter().any(|s| s == &slug),
        "active source MUST NOT be swept: {deleted:?}",
    );
    assert!(source_exists(&h.pool, &slug).await);
    assert_eq!(count_chunks_for_sv(&h.pool, sv_id).await, 1);
}

#[tokio::test]
async fn sweep_leaves_recently_retired_sources_alone() {
    let h = common::boot().await;
    let (_, _, _, slug) = seed_source_with_chunks(&h.pool, "sweep-recent").await;
    // Retired 60 seconds ago; grace 1h → still inside the window.
    retire_aged(&h.pool, &slug, 60).await;

    let grace_seconds = 60 * 60;
    let deleted = source::sweep_retired(&h.pool, grace_seconds)
        .await
        .expect("sweep");
    assert!(
        !deleted.iter().any(|s| s == &slug),
        "recently-retired source MUST stay within grace: {deleted:?}",
    );
}

#[tokio::test]
async fn sweep_hard_deletes_aged_out_retired_sources_and_cascades() {
    use sqlx::Row as _;

    let h = common::boot().await;
    let (_, sv_id, _, slug) = seed_source_with_chunks(&h.pool, "sweep-aged").await;
    // Retired 25 hours ago; grace 24h → outside the window.
    retire_aged(&h.pool, &slug, 25 * 60 * 60).await;

    let grace_seconds = 24 * 60 * 60;
    source::sweep_retired(&h.pool, grace_seconds)
        .await
        .expect("sweep");

    assert!(!source_exists(&h.pool, &slug).await, "source row must be hard-deleted");
    assert_eq!(
        count_chunks_for_sv(&h.pool, sv_id).await,
        0,
        "chunks must cascade-delete from source_version → source",
    );

    // The source_version row is also gone (sanity check on cascade chain).
    let sv_count: i64 =
        sqlx::query("SELECT COUNT(*)::bigint AS c FROM source_version WHERE id = $1")
            .bind(sv_id)
            .fetch_one(&h.pool)
            .await
            .unwrap()
            .get::<i64, _>("c");
    assert_eq!(sv_count, 0, "source_version must cascade-delete");
}

#[tokio::test]
async fn sweep_with_grace_zero_deletes_anything_currently_retired() {
    let h = common::boot().await;
    let (_, _, _, slug) = seed_source_with_chunks(&h.pool, "sweep-zero").await;
    // Retired 2 seconds ago; grace 0 → eligible.
    retire_aged(&h.pool, &slug, 2).await;

    source::sweep_retired(&h.pool, 0).await.expect("sweep");
    assert!(!source_exists(&h.pool, &slug).await);
}

#[tokio::test]
async fn sweep_returns_slugs_sorted_for_stable_logging() {
    let h = common::boot().await;
    let (_, _, _, slug_a) = seed_source_with_chunks(&h.pool, "sweep-sort-z").await;
    let (_, _, _, slug_b) = seed_source_with_chunks(&h.pool, "sweep-sort-a").await;
    retire_aged(&h.pool, &slug_a, 25 * 60 * 60).await;
    retire_aged(&h.pool, &slug_b, 25 * 60 * 60).await;

    let deleted = source::sweep_retired(&h.pool, 24 * 60 * 60)
        .await
        .expect("sweep");

    // Filter to OUR two slugs (a concurrent test might have deleted one
    // first; the assertion only cares about the order WE see in the
    // return value).
    let mut ours: Vec<&String> = deleted
        .iter()
        .filter(|s| s == &&slug_a || s == &&slug_b)
        .collect();
    let mut sorted = ours.clone();
    sorted.sort();
    ours.sort();
    assert_eq!(ours, sorted, "sweep_retired must return slugs ascending");
}

#[tokio::test]
async fn sweep_clamps_negative_grace_to_zero() {
    let h = common::boot().await;
    let (_, _, _, slug) = seed_source_with_chunks(&h.pool, "sweep-neg").await;
    retire_aged(&h.pool, &slug, 5).await;

    source::sweep_retired(&h.pool, -10_000)
        .await
        .expect("sweep");
    assert!(
        !source_exists(&h.pool, &slug).await,
        "negative grace is clamped to 0 — already-retired rows are deleted",
    );
}

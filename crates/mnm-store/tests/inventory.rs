//! Integration test: `list_active_inventory` and `count_documents` — Task 1 of
//! the incremental-ingest feature (FR-014 / incremental-ingest spec).

#![cfg(feature = "integration")]
#![allow(clippy::doc_markdown)]

#[sqlx::test(migrations = "./migrations")]
async fn inventory_reports_embed_complete_per_document(pool: sqlx::PgPool) {
    // Seed one source_version with: doc A (all chunks ready) and doc B (one embed_failed chunk).
    let fx = mnm_store::test_fixtures::seed_two_doc_version(&pool).await;

    let inv = mnm_store::entities::document::list_active_inventory(&pool, fx.source_version_id)
        .await
        .unwrap();

    let a = inv.iter().find(|d| d.source_path == "a.md").unwrap();
    let b = inv.iter().find(|d| d.source_path == "b.md").unwrap();
    assert!(a.embed_complete, "all-ready doc is embed_complete");
    assert!(!b.embed_complete, "doc with an embed_failed chunk is not embed_complete");
    assert_eq!(
        mnm_store::entities::source_version::count_documents(&pool, fx.source_version_id)
            .await
            .unwrap(),
        2
    );
}

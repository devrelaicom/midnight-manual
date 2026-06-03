//! Integration test: migration 0008 registers voyage-code-3 and widens the
//! embedding column to vector(1024).

#![cfg(feature = "integration")]
mod common;

#[tokio::test]
async fn migration_registers_voyage_and_sets_1024_dim() {
    let h = common::boot().await; // runs all migrations incl. 0008
                                  // voyage-code-3@1 is registered with dim 1024
    let m = mn_store::entities::embedding_model::get_by_name_revision(&h.pool, "voyage-code-3", 1)
        .await
        .expect("voyage-code-3@1 registered");
    assert_eq!(m.dim, 1024);
    assert_eq!(m.provider, "voyageai");
    // chunk.embedding column is vector(1024)
    let dim: i32 = sqlx::query_scalar(
        "SELECT atttypmod FROM pg_attribute \
         WHERE attrelid = 'chunk'::regclass AND attname = 'embedding'",
    )
    .fetch_one(&h.pool)
    .await
    .expect("query embedding typmod");
    assert_eq!(dim, 1024, "pgvector stores the dimension directly in atttypmod");
    // The HNSW index must be recreated after the column retype (catches a
    // partial migration that drops but never rebuilds it).
    let idx_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_indexes \
         WHERE tablename = 'chunk' AND indexname = 'idx_chunk_embedding')",
    )
    .fetch_one(&h.pool)
    .await
    .expect("query idx_chunk_embedding existence");
    assert!(idx_exists, "HNSW index idx_chunk_embedding must exist post-migration");
    // get_active resolves to voyage-code-3 (most-recently-created model, no active sv yet)
    let active = mn_store::entities::embedding_model::get_active(&h.pool)
        .await
        .unwrap();
    assert_eq!(active.name, "voyage-code-3");
}

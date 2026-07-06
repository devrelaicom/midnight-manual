//! `embedding_model::upsert` identity contract (issue #175): idempotent on
//! `(name, revision)`, but a conflicting `dim`/`provider` is rejected rather
//! than silently discarded.

#![cfg(feature = "integration")]
#![allow(clippy::doc_markdown)]

mod common;

use mnm_store::entities::embedding_model;
use mnm_store::StoreError;

#[tokio::test]
async fn upsert_is_idempotent_for_matching_identity() {
    let h = common::boot().await;

    let first = embedding_model::upsert(&h.pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    // Same (name, revision, dim, provider) → same id, no error.
    let second = embedding_model::upsert(&h.pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    assert_eq!(first, second, "matching upsert must return the existing id");
}

#[tokio::test]
async fn upsert_rejects_conflicting_dim() {
    let h = common::boot().await;

    embedding_model::upsert(&h.pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    // Same (name, revision), different dim → must error, not silently return
    // the stored 1024-dim row's id.
    let err = embedding_model::upsert(&h.pool, "voyage-code-3", 1, 512, "voyageai")
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::CheckViolation(_)), "got {err:?}");
}

#[tokio::test]
async fn upsert_rejects_conflicting_provider() {
    let h = common::boot().await;

    embedding_model::upsert(&h.pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    // Same (name, revision, dim), different provider → must error.
    let err = embedding_model::upsert(&h.pool, "voyage-code-3", 1, 1024, "someone-else")
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::CheckViolation(_)), "got {err:?}");
}

//! Integration: `corpus_model::resolve` reads the active model from the DB.
#![cfg(feature = "integration")]
mod common;

#[tokio::test]
async fn resolves_voyage_corpus_model_from_db() {
    let h = common::boot().await;
    let cm = midnight_manual_server::corpus_model::resolve(&h.pool)
        .await
        .expect("resolve");
    // Fresh DB, no active source_version: get_active falls back to the
    // newest registered model — voyage-context-3@1 since migration 0011.
    assert_eq!(cm.wire, "voyage-context-3@1");
    assert_eq!(cm.dim, 1024);
    assert!(!cm.id.is_nil(), "resolved model must carry a real id");
}

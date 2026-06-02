//! Integration: `corpus_model::resolve` reads the active model from the DB.
#![cfg(feature = "integration")]
mod common;

#[tokio::test]
async fn resolves_voyage_corpus_model_from_db() {
    let h = common::boot().await;
    let cm = mn_server::corpus_model::resolve(&h.pool)
        .await
        .expect("resolve");
    assert_eq!(cm.wire, "voyage-code-3@1");
    assert_eq!(cm.dim, 1024);
    assert!(!cm.id.is_nil(), "resolved model must carry a real id");
}

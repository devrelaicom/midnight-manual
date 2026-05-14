//! EC-04 / FR-003: at most one active `source_version` per source. Enforced
//! by the partial unique index `uniq_source_version_active`.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use mn_core::types::SourceKind;
use mn_store::entities::{embedding_model, source, source_version};
use uuid::Uuid;

#[tokio::test]
async fn cannot_have_two_active_versions_for_same_source() {
    let h = common::boot().await;

    let model_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("ec-04-test-{}", Uuid::new_v4());
    let source_id = source::insert(&h.pool, &slug, "EC-04 Test", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();

    let (sv1, _) = source_version::create_building(&h.pool, source_id, model_id, "0.1.0", "h1")
        .await
        .unwrap();
    source_version::finalize(&h.pool, sv1).await.unwrap();

    // Try to force a second active row directly bypassing finalize() — the
    // partial unique index MUST reject it.
    let (sv2, _) = source_version::create_building(&h.pool, source_id, model_id, "0.1.0", "h2")
        .await
        .unwrap();

    let err =
        sqlx::query("UPDATE source_version SET is_active = true, status = 'active' WHERE id = $1")
            .bind(sv2)
            .execute(&h.pool)
            .await;
    assert!(
        err.is_err(),
        "partial unique active index must reject a second active row for the same source"
    );

    // Finalize properly: demotes sv1 in the same tx.
    let (_, demoted) = source_version::finalize(&h.pool, sv2).await.unwrap();
    assert_eq!(demoted, Some(1), "sv1 must be demoted by sv2's finalize");
}

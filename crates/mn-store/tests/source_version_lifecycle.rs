//! Integration tests covering the `source_version` lifecycle (US1 + EC-04).
//!
//! All tests are gated on `--features integration` and require either a running
//! Postgres reachable via `DATABASE_URL` (CI's service) or a working local Docker
//! daemon (testcontainers fallback).

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use mn_core::types::{SourceKind, SourceVersionStatus};
use mn_store::entities::{embedding_model, source, source_version};
use uuid::Uuid;

#[tokio::test]
async fn create_and_finalize_promotes_active_and_demotes_prior() {
    let h = common::boot().await;

    let model_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .expect("seed model");

    let slug = format!("midnight-docs-1-{}", Uuid::new_v4());
    let source_id = source::insert(
        &h.pool,
        &slug,
        "Midnight Docs",
        SourceKind::DocsSite,
        Some("https://github.com/midnight-ntwrk/midnight-docs"),
        5,
    )
    .await
    .expect("insert source");

    // First ingest: revision 1, finalize, becomes active.
    let (sv1_id, rev1) =
        source_version::create_building(&h.pool, source_id, model_id, "0.1.0", "hashA")
            .await
            .expect("start v1");
    assert_eq!(rev1, 1);

    let (promoted, demoted) = source_version::finalize(&h.pool, sv1_id)
        .await
        .expect("finalize v1");
    assert_eq!(promoted, 1);
    assert!(demoted.is_none(), "no prior version to demote on first ingest");

    let active = source_version::get_active(&h.pool, source_id)
        .await
        .expect("get active");
    assert_eq!(active.revision, 1);
    assert_eq!(active.status, SourceVersionStatus::Active);
    assert!(active.is_active);

    // Second ingest: revision 2, finalize, demotes v1.
    let (sv2_id, rev2) =
        source_version::create_building(&h.pool, source_id, model_id, "0.1.0", "hashB")
            .await
            .expect("start v2");
    assert_eq!(rev2, 2);

    let (promoted2, demoted2) = source_version::finalize(&h.pool, sv2_id)
        .await
        .expect("finalize v2");
    assert_eq!(promoted2, 2);
    assert_eq!(demoted2, Some(1), "v1 must be demoted when v2 is promoted");

    let active2 = source_version::get_active(&h.pool, source_id)
        .await
        .expect("get active v2");
    assert_eq!(active2.revision, 2);

    // v1 still queryable as inactive.
    let v1 = source_version::get_by_revision(&h.pool, source_id, 1)
        .await
        .expect("get v1 by revision");
    assert_eq!(v1.status, SourceVersionStatus::Inactive);
    assert!(!v1.is_active);
}

#[tokio::test]
async fn finalize_rejects_non_building_state() {
    let h = common::boot().await;

    let model_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("midnight-docs-2-{}", Uuid::new_v4());
    let source_id = source::insert(&h.pool, &slug, "Midnight Docs", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();

    let (sv_id, _) = source_version::create_building(&h.pool, source_id, model_id, "0.1.0", "h")
        .await
        .unwrap();
    source_version::abort(&h.pool, sv_id).await.expect("abort");

    let err = source_version::finalize(&h.pool, sv_id).await.unwrap_err();
    match err {
        mn_store::StoreError::CheckViolation(msg) => {
            assert!(msg.contains("not in building"), "got: {msg}");
        }
        other => panic!("expected CheckViolation, got {other:?}"),
    }
}

#[tokio::test]
async fn retire_marks_eligible_for_sweep() {
    let h = common::boot().await;
    let model_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("midnight-docs-3-{}", Uuid::new_v4());
    let source_id = source::insert(&h.pool, &slug, "Midnight Docs", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();

    let (sv_id, _) = source_version::create_building(&h.pool, source_id, model_id, "0.1.0", "h")
        .await
        .unwrap();
    source_version::finalize(&h.pool, sv_id).await.unwrap();
    source_version::retire(&h.pool, sv_id)
        .await
        .expect("retire");

    let v = source_version::get_by_revision(&h.pool, source_id, 1)
        .await
        .unwrap();
    assert_eq!(v.status, SourceVersionStatus::Retired);
    assert!(!v.is_active);
    assert!(v.retired_at.is_some());
}

#[tokio::test]
async fn embedding_model_upsert_is_idempotent() {
    let h = common::boot().await;

    let id1 = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let id2 = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    assert_eq!(id1, id2, "upsert must be idempotent on (name, revision)");
}

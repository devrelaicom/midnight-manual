//! Integration tests for the new Phase-14 source_version helpers:
//! `list_for_source`, `promote_by_revision`, and `sweep_aged_inactive`.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use mn_core::types::{SourceKind, SourceVersionStatus};
use mn_store::entities::{embedding_model, source, source_version};
use sqlx::PgPool;
use uuid::Uuid;

async fn seed_source(pool: &PgPool, prefix: &str, retention_count: i32) -> (Uuid, Uuid) {
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("{prefix}-{}", Uuid::new_v4());
    let source_id =
        source::insert(pool, &slug, "SV Fixture", SourceKind::DocsSite, None, retention_count)
            .await
            .unwrap();
    (source_id, model_id)
}

/// Seed N source_versions for `source_id`. The newest revision becomes
/// active; the rest are demoted to `inactive` as each promotion happens.
/// Returns the source_version ids in revision-ascending order.
async fn seed_n_finalized(pool: &PgPool, source_id: Uuid, model_id: Uuid, n: usize) -> Vec<Uuid> {
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let (id, _rev) =
            source_version::create_building(pool, source_id, model_id, "0.1.0", &format!("h{i}"))
                .await
                .unwrap();
        source_version::finalize(pool, id).await.unwrap();
        ids.push(id);
    }
    ids
}

/// Backdate `ingested_at` so the sweep treats a version as aged-out.
async fn age_version(pool: &PgPool, sv_id: Uuid, seconds_ago: i64) {
    sqlx::query(
        "UPDATE source_version \
         SET ingested_at = now() - ($1::bigint * interval '1 second') \
         WHERE id = $2",
    )
    .bind(seconds_ago)
    .bind(sv_id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn list_for_source_returns_versions_newest_first() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-list", 5).await;
    seed_n_finalized(&h.pool, source_id, model_id, 3).await;

    let rows = source_version::list_for_source(&h.pool, source_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    // newest first
    assert_eq!(rows[0].revision, 3);
    assert!(rows[0].is_active);
    assert_eq!(rows[1].revision, 2);
    assert_eq!(rows[1].status, SourceVersionStatus::Inactive);
    assert_eq!(rows[2].revision, 1);
}

#[tokio::test]
async fn promote_by_revision_swaps_active_with_inactive() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-promote", 5).await;
    let _ids = seed_n_finalized(&h.pool, source_id, model_id, 3).await;
    // State: rev 3 active, rev 2 inactive, rev 1 inactive.

    let (promoted, demoted) = source_version::promote_by_revision(&h.pool, source_id, 1)
        .await
        .unwrap();
    assert_eq!(promoted, 1);
    assert_eq!(demoted, Some(3));

    let active = source_version::get_active(&h.pool, source_id)
        .await
        .unwrap();
    assert_eq!(active.revision, 1);

    let rev3 = source_version::get_by_revision(&h.pool, source_id, 3)
        .await
        .unwrap();
    assert_eq!(rev3.status, SourceVersionStatus::Inactive);
    assert!(!rev3.is_active);
}

#[tokio::test]
async fn promote_by_revision_rejects_already_active() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-promote-noop", 5).await;
    seed_n_finalized(&h.pool, source_id, model_id, 2).await;

    let err = source_version::promote_by_revision(&h.pool, source_id, 2)
        .await
        .unwrap_err();
    match err {
        mn_store::StoreError::CheckViolation(msg) => {
            assert!(msg.contains("active"), "msg = {msg}");
        }
        other => panic!("expected CheckViolation, got {other:?}"),
    }
}

#[tokio::test]
async fn promote_by_revision_404_on_unknown_revision() {
    let h = common::boot().await;
    let (source_id, _) = seed_source(&h.pool, "sv-promote-missing", 5).await;

    let err = source_version::promote_by_revision(&h.pool, source_id, 999)
        .await
        .unwrap_err();
    assert!(matches!(err, mn_store::StoreError::NotFound), "{err:?}");
}

#[tokio::test]
async fn sweep_aged_inactive_keeps_retention_window() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-sweep-keep", 3).await;
    let ids = seed_n_finalized(&h.pool, source_id, model_id, 5).await;
    // State: rev 5 active; revs 1..=4 inactive. retention_count = 3 →
    // keep revs 3, 4, 5; sweep revs 1, 2 if aged out.

    // Age ALL versions past 24h.
    for id in &ids {
        age_version(&h.pool, *id, 25 * 60 * 60).await;
    }

    let grace_seconds = 24 * 60 * 60;
    source_version::sweep_aged_inactive(&h.pool, grace_seconds)
        .await
        .unwrap();

    // The sweep return value is racy under shared-Postgres CI (a sibling
    // test's concurrent call to `sweep_aged_inactive` may delete our
    // revs before our own call runs). The reliable post-condition is the
    // remaining set for OUR source_id — that is solely a function of our
    // seed data + the sweep predicate.
    let remaining = source_version::list_for_source(&h.pool, source_id)
        .await
        .unwrap();
    let revs: Vec<i32> = remaining.iter().map(|r| r.revision).collect();
    assert_eq!(revs, vec![5, 4, 3], "retention_count=3 keeps the 3 newest; revs 1+2 swept",);
}

#[tokio::test]
async fn sweep_aged_inactive_keeps_within_grace_even_when_outside_retention() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-sweep-grace", 2).await;
    let ids = seed_n_finalized(&h.pool, source_id, model_id, 5).await;
    // retention_count=2 → only revs 4, 5 are inside the window. But none
    // of these are aged-out, so the sweep MUST NOT delete anything.
    // (No age_version calls.)
    let _ = ids;

    let grace_seconds = 24 * 60 * 60;
    let deleted = source_version::sweep_aged_inactive(&h.pool, grace_seconds)
        .await
        .unwrap();

    let our_count = deleted.iter().filter(|(sid, _)| *sid == source_id).count();
    assert_eq!(our_count, 0, "recently-ingested versions stay in place");
    let remaining = source_version::list_for_source(&h.pool, source_id)
        .await
        .unwrap();
    assert_eq!(remaining.len(), 5);
}

#[tokio::test]
async fn sweep_aged_inactive_never_touches_active_version() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-sweep-active", 1).await;
    let ids = seed_n_finalized(&h.pool, source_id, model_id, 3).await;
    // retention_count=1 → only rev 3 (active) is inside the window. But
    // active is rn=1 regardless of retention_count, so it stays.

    // Age ALL versions; with retention=1, revs 1 + 2 (inactive, aged)
    // become eligible. Rev 3 is active so excluded from being "eligible"
    // (only status IN ('inactive','retired') are deleted).
    for id in &ids {
        age_version(&h.pool, *id, 25 * 60 * 60).await;
    }

    source_version::sweep_aged_inactive(&h.pool, 24 * 60 * 60)
        .await
        .unwrap();

    let active = source_version::get_active(&h.pool, source_id)
        .await
        .unwrap();
    assert_eq!(active.revision, 3, "active version must survive sweep");

    let remaining = source_version::list_for_source(&h.pool, source_id)
        .await
        .unwrap();
    let revs: Vec<i32> = remaining.iter().map(|r| r.revision).collect();
    assert_eq!(revs, vec![3], "with retention=1 only the active version remains");
}

#[tokio::test]
async fn sweep_aged_inactive_leaves_aborted_versions_alone() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-sweep-aborted", 1).await;
    // One finalized active version + one aborted run that never finalized.
    let _active = seed_n_finalized(&h.pool, source_id, model_id, 1).await;
    let (aborted_id, _) =
        source_version::create_building(&h.pool, source_id, model_id, "0.1.0", "h-aborted")
            .await
            .unwrap();
    source_version::abort(&h.pool, aborted_id).await.unwrap();
    age_version(&h.pool, aborted_id, 25 * 60 * 60).await;

    source_version::sweep_aged_inactive(&h.pool, 24 * 60 * 60)
        .await
        .unwrap();

    // Aborted version still present.
    let row = source_version::get_by_id(&h.pool, aborted_id)
        .await
        .unwrap();
    assert_eq!(row.status, SourceVersionStatus::Aborted);
}

#[tokio::test]
async fn sweep_aged_inactive_returns_pairs_sorted() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-sweep-sort", 1).await;
    let ids = seed_n_finalized(&h.pool, source_id, model_id, 4).await;
    for id in &ids {
        age_version(&h.pool, *id, 25 * 60 * 60).await;
    }

    let deleted = source_version::sweep_aged_inactive(&h.pool, 24 * 60 * 60)
        .await
        .unwrap();

    let our: Vec<(Uuid, i32)> = deleted
        .iter()
        .copied()
        .filter(|(sid, _)| *sid == source_id)
        .collect();
    let mut sorted = our.clone();
    sorted.sort();
    assert_eq!(our, sorted);
}

// ===== Phase 15: sweep_aborted =====

/// Create an aborted run for `source_id` and (optionally) backdate it.
async fn seed_aborted_run(
    pool: &PgPool,
    source_id: Uuid,
    model_id: Uuid,
    content_hash: &str,
    aged_seconds_ago: i64,
) -> Uuid {
    let (id, _rev) =
        source_version::create_building(pool, source_id, model_id, "0.1.0", content_hash)
            .await
            .unwrap();
    source_version::abort(pool, id).await.unwrap();
    if aged_seconds_ago > 0 {
        age_version(pool, id, aged_seconds_ago).await;
    }
    id
}

#[tokio::test]
async fn sweep_aborted_hard_deletes_aged_aborted_runs() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-abort-aged", 5).await;
    let aborted_id = seed_aborted_run(&h.pool, source_id, model_id, "h-aged", 2 * 60 * 60).await;

    // grace = 1h; the run is aged 2h → eligible.
    source_version::sweep_aborted(&h.pool, 60 * 60)
        .await
        .unwrap();

    // Race-tolerant post-condition: the specific aborted row is gone.
    let row = source_version::get_by_id(&h.pool, aborted_id).await;
    assert!(matches!(row, Err(mn_store::StoreError::NotFound)), "{row:?}");
}

#[tokio::test]
async fn sweep_aborted_keeps_recent_aborted_runs() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-abort-recent", 5).await;
    // Aborted 30s ago; grace 1h → still inside the window.
    let aborted_id = seed_aborted_run(&h.pool, source_id, model_id, "h-recent", 30).await;

    let deleted = source_version::sweep_aborted(&h.pool, 60 * 60)
        .await
        .unwrap();
    assert!(
        !deleted.iter().any(|(_, rev)| *rev == 1),
        "should not include OUR rev 1: {deleted:?}",
    );

    // Best-effort: row should still exist. (No other test would target
    // this source_id; cross-test pollution is bounded to other
    // source_ids' aborted rows.)
    let row = source_version::get_by_id(&h.pool, aborted_id)
        .await
        .unwrap();
    assert_eq!(row.status, SourceVersionStatus::Aborted);
}

#[tokio::test]
async fn sweep_aborted_does_not_touch_active_or_inactive() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-abort-isolated", 5).await;
    // One active version + one inactive version, both aged past 24h.
    let ids = seed_n_finalized(&h.pool, source_id, model_id, 2).await;
    for id in &ids {
        age_version(&h.pool, *id, 25 * 60 * 60).await;
    }

    // grace=0 → would delete EVERY aborted row; should leave non-aborted
    // versions alone.
    source_version::sweep_aborted(&h.pool, 0).await.unwrap();

    let active = source_version::get_active(&h.pool, source_id)
        .await
        .unwrap();
    assert_eq!(active.revision, 2);
    let rev1 = source_version::get_by_revision(&h.pool, source_id, 1)
        .await
        .unwrap();
    assert_eq!(rev1.status, SourceVersionStatus::Inactive);
}

#[tokio::test]
async fn sweep_aborted_returns_pairs_sorted() {
    let h = common::boot().await;
    let (source_id, model_id) = seed_source(&h.pool, "sv-abort-sort", 5).await;
    seed_aborted_run(&h.pool, source_id, model_id, "h-a", 2 * 60 * 60).await;
    seed_aborted_run(&h.pool, source_id, model_id, "h-b", 2 * 60 * 60).await;

    let deleted = source_version::sweep_aborted(&h.pool, 60 * 60)
        .await
        .unwrap();
    let our: Vec<(Uuid, i32)> = deleted
        .iter()
        .copied()
        .filter(|(sid, _)| *sid == source_id)
        .collect();
    let mut sorted = our.clone();
    sorted.sort();
    assert_eq!(our, sorted);
}

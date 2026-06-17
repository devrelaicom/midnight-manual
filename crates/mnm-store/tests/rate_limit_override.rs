//! Integration tests for [`mnm_store::entities::rate_limit_override`] (Phase 16).
//!
//! CI shares one Postgres across every test binary, so these tests never
//! assert on the *contents* of the global `list_active` result — they scope
//! every row by a unique `created_by` sentinel and filter to it before
//! asserting, mirroring the race-tolerance discipline in
//! `source_retention_sweep.rs`.

#![cfg(feature = "integration")]
#![allow(clippy::doc_markdown)]

mod common;

use mnm_store::entities::rate_limit_override::{self, RateLimitPatch};
use mnm_store::StoreError;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// A unique `created_by` sentinel so sibling test binaries can't see our rows.
fn sentinel() -> String {
    format!("test-{}", Uuid::new_v4())
}

fn future() -> OffsetDateTime {
    OffsetDateTime::now_utc() + Duration::hours(1)
}

#[tokio::test]
async fn insert_and_readback_masks_host_bits() {
    let h = common::boot().await;
    let who = sentinel();
    // `169.155.237.15/25` has host bits set — `cidr` would reject it, but the
    // entity binds via `network($1::inet)`, which masks to the block address.
    let row = rate_limit_override::insert(
        &h.pool,
        "169.155.237.15/25",
        200,
        future(),
        Some("hackathon-london"),
        &who,
    )
    .await
    .expect("insert");
    assert_eq!(row.cidr, "169.155.237.0/25", "host bits must be masked off");
    assert_eq!(row.limit_rps, 200);
    assert_eq!(row.note.as_deref(), Some("hackathon-london"));
    assert_eq!(row.created_by, who);

    let fetched = rate_limit_override::get_by_id(&h.pool, row.id)
        .await
        .expect("get_by_id");
    assert_eq!(fetched, row);
}

#[tokio::test]
async fn list_active_excludes_expired() {
    let h = common::boot().await;
    let who = sentinel();
    let active = rate_limit_override::insert(&h.pool, "203.0.113.0/24", 50, future(), None, &who)
        .await
        .expect("insert active");
    let expired = rate_limit_override::insert(
        &h.pool,
        "198.51.100.0/24",
        50,
        OffsetDateTime::now_utc() - Duration::hours(1),
        None,
        &who,
    )
    .await
    .expect("insert expired");

    let rows = rate_limit_override::list_active(&h.pool)
        .await
        .expect("list");
    let ours: Vec<_> = rows.into_iter().filter(|r| r.created_by == who).collect();
    assert!(ours.iter().any(|r| r.id == active.id), "active row must be listed");
    assert!(!ours.iter().any(|r| r.id == expired.id), "expired row must be excluded");
}

#[tokio::test]
async fn get_by_id_unknown_is_not_found() {
    let h = common::boot().await;
    let err = rate_limit_override::get_by_id(&h.pool, Uuid::new_v4())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn update_extends_and_sparse_patches() {
    let h = common::boot().await;
    let who = sentinel();
    let row = rate_limit_override::insert(&h.pool, "192.0.2.0/24", 10, future(), Some("a"), &who)
        .await
        .expect("insert");

    // Patch only the expiry — limit_rps and note are untouched.
    let later = OffsetDateTime::now_utc() + Duration::hours(48);
    let patched = rate_limit_override::update(
        &h.pool,
        row.id,
        RateLimitPatch {
            expires_at: Some(later),
            ..Default::default()
        },
    )
    .await
    .expect("update expiry");
    assert!(patched.expires_at > row.expires_at, "expiry extended");
    assert_eq!(patched.limit_rps, 10, "limit_rps untouched");
    assert_eq!(patched.note.as_deref(), Some("a"), "note untouched");

    // Patch limit_rps and note together.
    let patched2 = rate_limit_override::update(
        &h.pool,
        row.id,
        RateLimitPatch {
            limit_rps: Some(99),
            note: Some("b".to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect("update fields");
    assert_eq!(patched2.limit_rps, 99);
    assert_eq!(patched2.note.as_deref(), Some("b"));
    assert_eq!(patched2.expires_at, patched.expires_at, "expiry untouched");
}

#[tokio::test]
async fn update_unknown_is_not_found() {
    let h = common::boot().await;
    let err = rate_limit_override::update(
        &h.pool,
        Uuid::new_v4(),
        RateLimitPatch {
            limit_rps: Some(5),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, StoreError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn delete_removes_then_not_found() {
    let h = common::boot().await;
    let who = sentinel();
    let row = rate_limit_override::insert(&h.pool, "203.0.113.0/24", 25, future(), None, &who)
        .await
        .expect("insert");

    let deleted = rate_limit_override::delete(&h.pool, row.id)
        .await
        .expect("delete");
    assert_eq!(deleted.id, row.id);

    let err = rate_limit_override::get_by_id(&h.pool, row.id)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound), "row gone after delete: {err:?}");

    let err2 = rate_limit_override::delete(&h.pool, row.id)
        .await
        .unwrap_err();
    assert!(matches!(err2, StoreError::NotFound), "second delete 404s: {err2:?}");
}

#[tokio::test]
async fn malformed_cidr_insert_surfaces_error() {
    let h = common::boot().await;
    let err = rate_limit_override::insert(&h.pool, "not-an-ip", 10, future(), None, &sentinel())
        .await
        .unwrap_err();
    // The `::inet` cast rejects garbage at the database boundary.
    assert!(
        matches!(err, StoreError::Database(_) | StoreError::CheckViolation(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn insert_rejects_nonpositive_limit() {
    let h = common::boot().await;
    let err =
        rate_limit_override::insert(&h.pool, "203.0.113.0/24", 0, future(), None, &sentinel())
            .await
            .unwrap_err();
    assert!(matches!(err, StoreError::CheckViolation(_)), "got {err:?}");
}

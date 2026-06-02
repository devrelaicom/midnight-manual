//! Integration tests for [`mn_store::entities::token_limit_override`].
//!
//! Covers insert, list, get, update, delete, and CIDR normalisation.

#![cfg(feature = "integration")]
#![allow(missing_docs)]

mod common;

use mn_store::entities::token_limit_override as tlo;
use mn_store::entities::token_limit_override::Patch;
use mn_store::StoreError;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

fn future() -> OffsetDateTime {
    OffsetDateTime::now_utc() + Duration::hours(2)
}

#[tokio::test]
async fn insert_list_update_delete_roundtrip() {
    let h = common::boot().await;
    let exp = future();

    // Insert a user-scoped override.
    let row = tlo::insert(&h.pool, "user", "alice", 4000, 40000, exp, Some("vip"), "admin")
        .await
        .unwrap();
    assert_eq!(row.hourly, 4000);
    assert_eq!(row.daily, 40000);
    assert_eq!(row.subject_kind, "user");
    assert_eq!(row.subject, "alice");
    assert_eq!(row.note.as_deref(), Some("vip"));
    assert_eq!(row.created_by, "admin");

    // The row must appear in list_active.
    let active = tlo::list_active(&h.pool).await.unwrap();
    assert!(active.iter().any(|r| r.id == row.id));

    // get_by_id must round-trip the row.
    let fetched = tlo::get_by_id(&h.pool, row.id).await.unwrap();
    assert_eq!(fetched, row);

    // Insert a CIDR-scoped override — network() must canonicalise host bits.
    let cidr = tlo::insert(&h.pool, "cidr", "203.0.113.0/24", 9, 90, exp, None, "admin")
        .await
        .unwrap();
    // network(203.0.113.0/24::inet)::text is the same address (no host bits set).
    assert_eq!(cidr.subject, "203.0.113.0/24");

    // Update — sparse patch: only hourly changes.
    let patched = tlo::update(
        &h.pool,
        row.id,
        Patch {
            hourly: Some(8000),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(patched.hourly, 8000);
    assert_eq!(patched.daily, row.daily, "daily untouched");
    assert_eq!(patched.note.as_deref(), Some("vip"), "note untouched");

    // Delete — returns the row.
    let deleted = tlo::delete(&h.pool, row.id).await.unwrap();
    assert_eq!(deleted.id, row.id);

    // Second delete must 404.
    let err = tlo::delete(&h.pool, row.id).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound), "got {err:?}");

    // get_by_id on deleted row must 404.
    let err2 = tlo::get_by_id(&h.pool, row.id).await.unwrap_err();
    assert!(matches!(err2, StoreError::NotFound), "got {err2:?}");
}

#[tokio::test]
async fn update_unknown_is_not_found() {
    let h = common::boot().await;
    let err = tlo::update(
        &h.pool,
        Uuid::new_v4(),
        Patch {
            hourly: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, StoreError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn get_by_id_unknown_is_not_found() {
    let h = common::boot().await;
    let err = tlo::get_by_id(&h.pool, Uuid::new_v4()).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn list_active_excludes_expired() {
    let h = common::boot().await;
    let sentinel = format!("test-{}", Uuid::new_v4());

    let active = tlo::insert(&h.pool, "user", &sentinel, 100, 1000, future(), None, &sentinel)
        .await
        .unwrap();

    let expired = tlo::insert(
        &h.pool,
        "user",
        &format!("{sentinel}-exp"),
        100,
        1000,
        OffsetDateTime::now_utc() - Duration::hours(1),
        None,
        &sentinel,
    )
    .await
    .unwrap();

    let rows = tlo::list_active(&h.pool).await.unwrap();
    let ours: Vec<_> = rows.iter().filter(|r| r.created_by == sentinel).collect();
    assert!(ours.iter().any(|r| r.id == active.id), "active row listed");
    assert!(ours.iter().all(|r| r.id != expired.id), "expired row excluded");
}

#[tokio::test]
async fn cidr_with_host_bits_normalised() {
    let h = common::boot().await;
    // 192.0.2.15/24 has host bits set — network() must yield 192.0.2.0/24.
    let row = tlo::insert(&h.pool, "cidr", "192.0.2.15/24", 50, 500, future(), None, "admin")
        .await
        .unwrap();
    assert_eq!(row.subject, "192.0.2.0/24", "host bits must be masked off");
}

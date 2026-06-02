//! Integration test: `TokenUsageLimiter` snapshot/restore round-trip (Task 4.8).
//!
//! Charging a subject, snapshotting to the DB, then loading into a fresh
//! limiter must restore the subject's daily-window usage (per-hour buckets are
//! the durable ones). `AppState`/Postgres is required, so this is gated behind
//! the `integration` feature and runs in CI.

#![cfg(feature = "integration")]
#![allow(missing_docs, clippy::too_many_lines)]

mod common;

use mn_server::tokenlimit::{Limits, TokenSubject, TokenUsageLimiter};

const fn limits() -> Limits {
    Limits { hourly: 2000, daily: 20000 }
}

#[tokio::test]
async fn snapshot_roundtrip_restores_daily_usage() {
    let h = common::boot().await;
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let subject = TokenSubject::User("snapshot-user".to_owned());

    // Charge a subject on one limiter, then persist its buckets.
    let l1 = TokenUsageLimiter::new(limits(), limits(), limits());
    l1.charge(&subject, 500, now);
    l1.snapshot_to_db(&h.pool, now)
        .await
        .expect("snapshot_to_db");

    // A fresh limiter (as if the server restarted) loads the snapshot and must
    // see the same daily-window usage.
    let l2 = TokenUsageLimiter::new(limits(), limits(), limits());
    l2.load_from_db(&h.pool, now).await.expect("load_from_db");
    let info = l2.snapshot_for(&subject, limits(), now);
    assert_eq!(
        info.day.remaining,
        20000 - 500,
        "daily usage must survive a snapshot/restore round-trip"
    );
}

#[tokio::test]
async fn snapshot_evicts_idle_subject_but_keeps_recent() {
    let h = common::boot().await;
    let now = time::OffsetDateTime::now_utc().unix_timestamp();

    let l = TokenUsageLimiter::new(limits(), limits(), limits());
    // An idle subject last seen > 24h ago, and a freshly-charged one.
    let idle = TokenSubject::User("idle-user".to_owned());
    let recent = TokenSubject::User("recent-user".to_owned());
    l.charge(&idle, 100, now - 100_000); // > 86_400s ago → idle
    l.charge(&recent, 100, now);

    l.snapshot_to_db(&h.pool, now)
        .await
        .expect("snapshot_to_db");

    // The recent subject's in-memory usage is retained; the idle one was evicted
    // (its in-memory entry dropped, so it now reports full headroom). The idle
    // subject's stale bucket is also pruned from the snapshot.
    assert_eq!(
        l.snapshot_for(&recent, limits(), now).hour.remaining,
        2000 - 100,
        "recently-active subject must not be evicted"
    );
    assert_eq!(
        l.snapshot_for(&idle, limits(), now).hour.remaining,
        2000,
        "idle subject (>24h) is evicted from memory"
    );
}

//! Integration tests for the telemetry retention sweep (FR-110 / SC-065).
//!
//! Each test scopes its rows by a per-test `version` sentinel so they can
//! run concurrently against the shared CI Postgres without trampling one
//! another. Helpers below filter every query by that sentinel and a small
//! file-scoped mutex serialises the global `sweep_once` calls (the sweep
//! observes ALL expired rows, not just this test's, so we cannot assert
//! global row counts in parallel).

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines)]

mod common;

use std::sync::OnceLock;

use midnight_manual_server::jobs::telemetry_sweep::sweep_once;
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Per-file serialisation guard: `sweep_once` acts on all expired rows on
/// the DB, so two concurrent sweep tests would step on each other even
/// when they otherwise scope by sentinel. The mutex makes one sweep test
/// run at a time — a cheap fix vs. spinning up isolated databases.
fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Unique sentinel inserted into the `version` column so a single test's
/// rows can be picked out of the shared table.
fn sentinel() -> String {
    format!("sweep-test-{}", Uuid::new_v4())
}

async fn insert_raw_row(
    pool: &PgPool,
    sentinel: &str,
    event_type: &str,
    component: &str,
    received_offset_days: i64,
) {
    sqlx::query(
        "INSERT INTO telemetry_event_raw (id, received_at, event_type, component, version, fields, request_id) \
         VALUES ($1, now() - make_interval(days => $2::int), $3, $4, $5, $6, NULL)",
    )
    .bind(Uuid::new_v4())
    .bind(received_offset_days)
    .bind(event_type)
    .bind(component)
    .bind(sentinel)
    .bind(json!({"event_type": event_type, "startup_ms": 1, "model_state": "ready"}))
    .execute(pool)
    .await
    .expect("insert telemetry row");
}

async fn raw_count(pool: &PgPool, sentinel: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*)::bigint FROM telemetry_event_raw WHERE version = $1")
        .bind(sentinel)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// `day_offset_days` is the integer N for `day = CURRENT_DATE - N days`.
async fn aggregate_total_at_offset(
    pool: &PgPool,
    event_type: &str,
    component: &str,
    day_offset_days: i64,
) -> i64 {
    // Aggregates don't carry a sentinel — we scope by exact (event_type,
    // component, day) which is what each test sets.
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(count)::bigint, 0) FROM telemetry_aggregate_daily \
         WHERE event_type = $1 AND component = $2 \
           AND day = (CURRENT_DATE - make_interval(days => $3::int))::date",
    )
    .bind(event_type)
    .bind(component)
    .bind(day_offset_days)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn sweep_deletes_expired_rows_and_increments_aggregates() {
    let _g = lock().lock().await;
    let h = common::boot().await;
    let s = sentinel();

    // 3 rows older than 7d (the default retention window) + 2 fresh rows.
    insert_raw_row(&h.pool, &s, "mcp_startup", "mcp", 10).await;
    insert_raw_row(&h.pool, &s, "mcp_startup", "mcp", 9).await;
    insert_raw_row(&h.pool, &s, "mcp_startup", "mcp", 8).await;
    insert_raw_row(&h.pool, &s, "mcp_startup", "mcp", 1).await;
    insert_raw_row(&h.pool, &s, "mcp_startup", "mcp", 0).await;

    let before_remaining = raw_count(&h.pool, &s).await;
    assert_eq!(before_remaining, 5);

    sweep_once(&h.pool, 7).await.expect("sweep");
    let remaining = raw_count(&h.pool, &s).await;
    assert_eq!(remaining, 2, "only fresh rows remain in raw");

    // SC-065: aggregates reflect the swept rows. We scope by day-offset
    // (8..=10) to avoid colliding with other tests that may have rolled
    // up the same (event_type, component).
    let mut total: i64 = 0;
    for offset in [8, 9, 10] {
        total += aggregate_total_at_offset(&h.pool, "mcp_startup", "mcp", offset).await;
    }
    assert!(total >= 3, "aggregate_daily must have ≥3 swept-row counts; got {total}");
}

#[tokio::test]
async fn sweep_is_idempotent_on_repeat() {
    let _g = lock().lock().await;
    let h = common::boot().await;
    let s = sentinel();

    insert_raw_row(&h.pool, &s, "cli_command", "cli", 30).await;
    insert_raw_row(&h.pool, &s, "cli_command", "cli", 30).await;

    sweep_once(&h.pool, 7).await.unwrap();
    let after_first = raw_count(&h.pool, &s).await;
    assert_eq!(after_first, 0, "first sweep removes all 2 sentinel rows");

    sweep_once(&h.pool, 7).await.unwrap();
    // Idempotent: the sentinel row count stays at zero.
    let after_second = raw_count(&h.pool, &s).await;
    assert_eq!(after_second, 0);
}

#[tokio::test]
async fn sweep_buckets_by_day_event_type_and_component() {
    let _g = lock().lock().await;
    let h = common::boot().await;
    let s = sentinel();

    insert_raw_row(&h.pool, &s, "mcp_tool_call", "mcp", 10).await;
    insert_raw_row(&h.pool, &s, "mcp_tool_call", "mcp", 10).await;
    insert_raw_row(&h.pool, &s, "cli_command", "cli", 10).await;

    sweep_once(&h.pool, 7).await.unwrap();
    assert_eq!(raw_count(&h.pool, &s).await, 0, "all 3 sentinel rows must be swept");

    // The aggregate row for our (event_type, component, day) tuple must
    // have grown by ≥ the number we inserted.
    let tool_calls = aggregate_total_at_offset(&h.pool, "mcp_tool_call", "mcp", 10).await;
    assert!(tool_calls >= 2, "got {tool_calls}");
    let cli_cmds = aggregate_total_at_offset(&h.pool, "cli_command", "cli", 10).await;
    assert!(cli_cmds >= 1, "got {cli_cmds}");
}

#[tokio::test]
async fn sweep_preserves_existing_aggregate_rows() {
    let _g = lock().lock().await;
    let h = common::boot().await;
    let s = sentinel();

    // Pre-existing aggregate. Use a deliberately old day to avoid collision
    // with parallel test inserts: 100 days ago and an event_type/component
    // combo (ingest_complete/cli) that no other phase-8c test touches.
    sqlx::query(
        "INSERT INTO telemetry_aggregate_daily (day, event_type, component, count) \
         VALUES (CURRENT_DATE - INTERVAL '100 days', 'ingest_complete', 'cli', 100) \
         ON CONFLICT (day, event_type, component) DO UPDATE SET count = telemetry_aggregate_daily.count + 100",
    )
    .execute(&h.pool)
    .await
    .unwrap();

    let before_total = aggregate_total_at_offset(&h.pool, "ingest_complete", "cli", 100).await;

    // Insert one raw row at the SAME day so the rollup hits the same bucket.
    insert_raw_row(&h.pool, &s, "ingest_complete", "cli", 100).await;
    sweep_once(&h.pool, 7).await.unwrap();

    let after_total = aggregate_total_at_offset(&h.pool, "ingest_complete", "cli", 100).await;
    // The pre-existing 100 (or 200 if a prior run seeded) is preserved AND
    // our single roll-up has been added.
    assert!(
        after_total > before_total,
        "pre-existing aggregate must be preserved; before={before_total} after={after_total}",
    );
}

#[tokio::test]
async fn sweep_does_not_touch_fresh_rows() {
    let _g = lock().lock().await;
    let h = common::boot().await;
    let s = sentinel();

    insert_raw_row(&h.pool, &s, "mcp_startup", "mcp", 0).await;
    insert_raw_row(&h.pool, &s, "mcp_startup", "mcp", 6).await;

    sweep_once(&h.pool, 7).await.unwrap();
    assert_eq!(raw_count(&h.pool, &s).await, 2, "fresh sentinel rows must remain");
}

#[tokio::test]
async fn sweep_rolls_up_search_dimensions_into_telemetry_search_daily() {
    let _g = lock().lock().await;
    let h = common::boot().await;

    // Insert an expired mcp_tool_call/search event with known dimensions.
    sqlx::query(
        "INSERT INTO telemetry_event_raw (id, received_at, event_type, component, version, fields, request_id) \
         VALUES (gen_random_uuid(), now() - interval '1 day', 'mcp_tool_call', 'mcp', $1, $2, NULL)",
    )
    .bind(sentinel())
    .bind(json!({
        "event_type": "mcp_tool_call",
        "tool_name": "search",
        "corpus_model": "voyage-code-3@1",
        "top_attribution": "foundation",
        "reranker_used": "rerank-2.5",
        "top_source": "Compact Docs",
        "top_confidence": "high"
    }))
    .execute(&h.pool)
    .await
    .expect("insert search telemetry row");

    sweep_once(&h.pool, 0).await.expect("sweep");

    let count: i64 = sqlx::query_scalar(
        "SELECT count FROM telemetry_search_daily \
         WHERE corpus_model = 'voyage-code-3@1' AND attribution = 'foundation' \
           AND reranker = 'rerank-2.5' AND top_source = 'Compact Docs' \
           AND confidence_bucket = 'high'",
    )
    .fetch_one(&h.pool)
    .await
    .expect("fetch telemetry_search_daily row");

    assert_eq!(count, 1, "search dimensions must be rolled up with count=1");
}

#[tokio::test]
async fn sweep_rollup_includes_advanced_search_and_merges_shared_dimensions() {
    let _g = lock().lock().await;
    let h = common::boot().await;

    // One `search` and one `advanced_search` row with IDENTICAL dimensions:
    // the rollup predicate must include both wire names (advanced_search
    // events would otherwise silently drop out of telemetry_search_daily)
    // and merge them into a single dimensional bucket.
    for tool_name in ["search", "advanced_search"] {
        sqlx::query(
            "INSERT INTO telemetry_event_raw (id, received_at, event_type, component, version, fields, request_id) \
             VALUES (gen_random_uuid(), now() - interval '1 day', 'mcp_tool_call', 'mcp', $1, $2, NULL)",
        )
        .bind(sentinel())
        .bind(json!({
            "event_type": "mcp_tool_call",
            "tool_name": tool_name,
            "corpus_model": "voyage-code-3@1",
            "top_attribution": "community",
            "reranker_used": "rerank-2.5",
            "top_source": "Midnight JS",
            "top_confidence": "medium"
        }))
        .execute(&h.pool)
        .await
        .expect("insert telemetry row");
    }

    sweep_once(&h.pool, 0).await.expect("sweep");

    let count: i64 = sqlx::query_scalar(
        "SELECT count FROM telemetry_search_daily \
         WHERE corpus_model = 'voyage-code-3@1' AND attribution = 'community' \
           AND reranker = 'rerank-2.5' AND top_source = 'Midnight JS' \
           AND confidence_bucket = 'medium'",
    )
    .fetch_one(&h.pool)
    .await
    .expect("fetch telemetry_search_daily row");

    assert_eq!(
        count, 2,
        "search + advanced_search with shared dimensions must merge to count=2"
    );
}

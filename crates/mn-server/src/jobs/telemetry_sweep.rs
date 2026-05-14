//! Telemetry retention sweep (FR-110 / SC-065).
//!
//! Rolls expired `telemetry_event_raw` rows into `telemetry_aggregate_daily`
//! and then deletes them. The two operations run inside a single transaction
//! so the aggregate counters are only ever incremented for rows that are
//! actually removed — if the DELETE fails, the rollup rolls back too.
//!
//! Retention is configurable via `MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS`
//! (default 7). Aggregate rows are retained indefinitely.

use std::time::Duration;

use sqlx::PgPool;

/// Default retention window for raw telemetry rows, matching the spec
/// (FR-110). Configurable per-run via `ServerConfig::telemetry_raw_retention_days`.
pub const DEFAULT_RETENTION_DAYS: i64 = 7;

/// How often the sweep job ticks at runtime. One hour is fine for v1 —
/// retention is per-day, not per-minute, and a missed sweep window cannot
/// produce a privacy violation (rows simply linger an hour past their
/// notional expiry).
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// One pass through the sweep pipeline.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepStats {
    /// Number of raw rows rolled into `telemetry_aggregate_daily` and then
    /// deleted in this pass.
    pub swept_rows: u64,
    /// Number of distinct `(day, event_type, component)` aggregate rows
    /// touched (INSERTed or UPDATEd) by this pass.
    pub aggregated_rows: u64,
}

/// Run one sweep cycle synchronously. Exposed for direct testing and for
/// `mnm`-side tooling that may eventually trigger a manual sweep.
///
/// `retention_days` is the rolling window; rows with `received_at < now() -
/// interval 'retention_days days'` are rolled up and deleted.
///
/// # Errors
///
/// Returns the underlying `sqlx::Error` if any of the three queries fails.
/// The whole pass is wrapped in a transaction; a failure leaves the raw
/// rows in place.
pub async fn sweep_once(pool: &PgPool, retention_days: i64) -> Result<SweepStats, sqlx::Error> {
    let retention_days = retention_days.max(0);
    let mut tx = pool.begin().await?;

    // Aggregate: bucket the expired rows by (day, event_type, component) and
    // increment the matching `telemetry_aggregate_daily` row, creating it
    // on first hit. The CTE narrows to the same date predicate the DELETE
    // below uses so we never roll up a row we don't then delete.
    let aggregated_rows = sqlx::query(
        "WITH expired AS (
             SELECT received_at::date AS day, event_type, component, COUNT(*)::bigint AS c
             FROM telemetry_event_raw
             WHERE received_at < now() - make_interval(days => $1::int)
             GROUP BY 1, 2, 3
         )
         INSERT INTO telemetry_aggregate_daily (day, event_type, component, count)
         SELECT day, event_type, component, c FROM expired
         ON CONFLICT (day, event_type, component)
         DO UPDATE SET count = telemetry_aggregate_daily.count + EXCLUDED.count",
    )
    .bind(retention_days)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    // Delete: ensure the WHERE clause is bit-for-bit identical to the CTE
    // predicate above so a row can't slip between the two queries.
    let deleted: u64 = sqlx::query(
        "DELETE FROM telemetry_event_raw \
         WHERE received_at < now() - make_interval(days => $1::int)",
    )
    .bind(retention_days)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    tx.commit().await?;

    Ok(SweepStats {
        swept_rows: deleted,
        aggregated_rows,
    })
}

/// Read the total row count from `telemetry_aggregate_daily` for the given
/// `(event_type, component)` pair across all days. Test-only helper.
#[cfg(any(test, feature = "integration"))]
pub async fn aggregate_total(
    pool: &PgPool,
    event_type: &str,
    component: &str,
) -> Result<i64, sqlx::Error> {
    use sqlx::Row as _;
    let row = sqlx::query(
        "SELECT COALESCE(SUM(count)::bigint, 0) AS total \
         FROM telemetry_aggregate_daily \
         WHERE event_type = $1 AND component = $2",
    )
    .bind(event_type)
    .bind(component)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<i64, _>("total"))
}

/// Spawn the periodic sweep task. The returned `JoinHandle` is kept alive
/// by the server's main loop; on drop, the task is cancelled.
///
/// The task ticks every [`SWEEP_INTERVAL`]; an initial tick fires
/// immediately so a freshly-started server does not need to wait an hour
/// to honour retention.
pub fn spawn(pool: PgPool, retention_days: i64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SWEEP_INTERVAL);
        // The first `interval.tick()` fires immediately. We want it to —
        // that's the boot-time sweep.
        loop {
            interval.tick().await;
            match sweep_once(&pool, retention_days).await {
                Ok(stats) => {
                    if stats.swept_rows > 0 {
                        tracing::info!(
                            swept_rows = stats.swept_rows,
                            aggregated_rows = stats.aggregated_rows,
                            "telemetry sweep complete",
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "telemetry sweep failed; will retry");
                }
            }
        }
    })
}

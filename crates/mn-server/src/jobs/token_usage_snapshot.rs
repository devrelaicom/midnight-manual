//! Periodic token-usage snapshot job (restart durability + idle eviction).
//!
//! On each tick the job persists every subject's in-window per-hour buckets to
//! `token_usage_snapshot`, evicts subjects idle for >24h from memory, and
//! prunes stale snapshot rows. Snapshotting is best-effort: a failed tick logs
//! and retries on the next interval rather than tearing down the loop.

use std::sync::Arc;
use std::time::Duration;

use crate::tokenlimit::TokenUsageLimiter;

/// Spawn the periodic snapshot loop. Every `secs`, persists in-window usage to
/// the DB, evicts idle subjects, and prunes stale snapshot rows. `secs` is
/// floored to 1 second so a misconfigured `0` cannot busy-loop.
#[must_use]
pub fn spawn(
    pool: sqlx::PgPool,
    limiter: Arc<TokenUsageLimiter>,
    secs: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs.max(1)));
        tick.tick().await; // consume the immediate first tick
        loop {
            tick.tick().await;
            let now = time::OffsetDateTime::now_utc().unix_timestamp();
            if let Err(e) = limiter.snapshot_to_db(&pool, now).await {
                tracing::warn!(error = %e, "token usage snapshot failed");
            }
        }
    })
}

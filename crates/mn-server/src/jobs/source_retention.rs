//! Source retention sweep (Phase 13).
//!
//! Hard-deletes sources whose `retired_at` is older than a grace window so
//! the soft-delete left by `mnm sources retire` (Phase 12) eventually frees
//! the slug and the underlying storage. The DB schema does the heavy
//! lifting: deleting a `source` row cascades through
//! `source_version` → `node` / `package` / `document` / `chunk` via the
//! `ON DELETE CASCADE` foreign keys declared in migration 0002.
//!
//! Retention is configurable via
//! `MIDNIGHT_MANUAL_SOURCE_RETIREMENT_GRACE_HOURS` (default 24) and the
//! sweep interval via `MIDNIGHT_MANUAL_SOURCE_RETIREMENT_INTERVAL_MINUTES`
//! (default 60). Both are clamped at parse time in [`crate::config`].

use std::time::Duration;

use mn_store::entities::source;
use sqlx::PgPool;

/// One pass through the source-retention sweep.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepStats {
    /// Slugs of sources hard-deleted in this pass, sorted ascending.
    pub deleted_slugs: Vec<String>,
}

impl SweepStats {
    /// Convenience accessor for the deleted-row count.
    #[must_use]
    pub const fn deleted_count(&self) -> usize {
        self.deleted_slugs.len()
    }
}

/// Run one sweep cycle synchronously. Exposed for direct testing and for
/// any future admin endpoint that triggers a manual sweep.
///
/// `grace_hours` is the rolling window — sources whose `retired_at` is
/// older than `now() - grace_hours` are hard-deleted in a single SQL
/// statement, cascading their children.
///
/// # Errors
///
/// Returns the underlying [`mn_store::StoreError`] if the DELETE fails.
pub async fn sweep_once(
    pool: &PgPool,
    grace_hours: i64,
) -> Result<SweepStats, mn_store::StoreError> {
    let grace_seconds = grace_hours.saturating_mul(60 * 60);
    let deleted_slugs = source::sweep_retired(pool, grace_seconds).await?;
    Ok(SweepStats { deleted_slugs })
}

/// Spawn the periodic source-retention sweep. The returned `JoinHandle` is
/// kept alive by the server's main loop; on drop, the task is cancelled.
///
/// The task ticks every `interval_minutes`; an initial tick fires
/// immediately so a freshly-started server does not need to wait an hour
/// to honour retention.
#[must_use]
pub fn spawn(pool: PgPool, grace_hours: i64, interval_minutes: u64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval_duration = Duration::from_secs(interval_minutes.saturating_mul(60).max(1));
        let mut interval = tokio::time::interval(interval_duration);
        loop {
            interval.tick().await;
            match sweep_once(&pool, grace_hours).await {
                Ok(stats) if stats.deleted_slugs.is_empty() => {
                    tracing::debug!("source retention sweep tick — nothing to delete");
                }
                Ok(stats) => {
                    tracing::info!(
                        deleted = stats.deleted_count(),
                        slugs = ?stats.deleted_slugs,
                        "source retention sweep complete",
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "source retention sweep failed; will retry");
                }
            }
        }
    })
}

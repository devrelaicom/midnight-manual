//! Source + source-version retention sweep (Phases 13 + 14 + 15).
//!
//! The daemon makes three passes on every tick, in order:
//!
//! 1. **Source sweep** (Phase 13) — hard-deletes sources whose
//!    `retired_at` is older than `source_grace_hours` so the soft-delete
//!    left by `mnm sources retire` eventually frees the slug.
//! 2. **Source-version sweep** (Phase 14 / FR-063) — for each source,
//!    keeps the `retention_count` most recent versions and hard-deletes
//!    any older `inactive` or `retired` rows whose `ingested_at` is past
//!    `version_grace_hours`. The active version is always preserved;
//!    `building` and `aborted` versions are left alone.
//! 3. **Aborted-run sweep** (Phase 15 / FR-063 / EC-22) — hard-deletes
//!    `aborted` source_version rows whose `ingested_at` is older than
//!    `abort_grace_hours`. Cleans up runs that were started but never
//!    finalized.
//!
//! All passes lean on `ON DELETE CASCADE` so a single DELETE removes the
//! `source_version → node / package / document / chunk` subtree.
//!
//! Retention is configurable via
//! `MIDNIGHT_MANUAL_SOURCE_RETIREMENT_GRACE_HOURS` (default 24),
//! `MIDNIGHT_MANUAL_SOURCE_VERSION_SWEEP_GRACE_HOURS` (default 24),
//! `MIDNIGHT_MANUAL_ABORT_GRACE_HOURS` (default 1), and the sweep
//! interval via `MIDNIGHT_MANUAL_SOURCE_RETIREMENT_INTERVAL_MINUTES`
//! (default 60). All are clamped at parse time in [`crate::config`].

use std::time::Duration;

use mn_store::entities::{source, source_version};
use sqlx::PgPool;
use uuid::Uuid;

/// One pass through the full retention sweep (sources + versions + aborts).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepStats {
    /// Slugs of sources hard-deleted in this pass, sorted ascending.
    /// Cascades take care of their source_version, node, document, and
    /// chunk children.
    pub deleted_slugs: Vec<String>,
    /// `(source_id, revision)` of source_versions hard-deleted by the
    /// retention sweep (Phase 14). Excludes versions deleted via the
    /// source sweep above — those are accounted for under `deleted_slugs`.
    pub deleted_versions: Vec<(Uuid, i32)>,
    /// `(source_id, revision)` of `aborted` source_versions hard-deleted
    /// by the aborted-run sweep (Phase 15).
    pub deleted_aborted_runs: Vec<(Uuid, i32)>,
}

impl SweepStats {
    /// Convenience accessor for the source-row deletion count.
    #[must_use]
    pub const fn deleted_source_count(&self) -> usize {
        self.deleted_slugs.len()
    }

    /// Convenience accessor for the (retention-driven) source_version
    /// deletion count.
    #[must_use]
    pub const fn deleted_version_count(&self) -> usize {
        self.deleted_versions.len()
    }

    /// Convenience accessor for the aborted-run deletion count.
    #[must_use]
    pub const fn deleted_aborted_count(&self) -> usize {
        self.deleted_aborted_runs.len()
    }
}

/// Run one full sweep cycle synchronously: source pass (frees slugs),
/// then source-version pass (retention), then aborted-run pass. All
/// three pass results are surfaced in the returned [`SweepStats`].
///
/// # Errors
///
/// Returns the underlying [`mn_store::StoreError`] if any DELETE fails.
/// The three passes are independent — a failure in one short-circuits
/// the call and the remaining passes do not run on this tick.
pub async fn sweep_once(
    pool: &PgPool,
    source_grace_hours: i64,
    version_grace_hours: i64,
    abort_grace_hours: i64,
) -> Result<SweepStats, mn_store::StoreError> {
    let source_grace_seconds = source_grace_hours.saturating_mul(60 * 60);
    let deleted_slugs = source::sweep_retired(pool, source_grace_seconds).await?;

    let version_grace_seconds = version_grace_hours.saturating_mul(60 * 60);
    let deleted_versions = source_version::sweep_aged_inactive(pool, version_grace_seconds).await?;

    let abort_grace_seconds = abort_grace_hours.saturating_mul(60 * 60);
    let deleted_aborted_runs = source_version::sweep_aborted(pool, abort_grace_seconds).await?;

    Ok(SweepStats {
        deleted_slugs,
        deleted_versions,
        deleted_aborted_runs,
    })
}

/// Spawn the periodic retention sweep. The returned `JoinHandle` is kept
/// alive by the server's main loop; on drop, the task is cancelled.
///
/// The task ticks every `interval_minutes`; an initial tick fires
/// immediately so a freshly-started server does not need to wait an hour
/// to honour retention.
#[must_use]
pub fn spawn(
    pool: PgPool,
    source_grace_hours: i64,
    version_grace_hours: i64,
    abort_grace_hours: i64,
    interval_minutes: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval_duration = Duration::from_secs(interval_minutes.saturating_mul(60).max(1));
        let mut interval = tokio::time::interval(interval_duration);
        loop {
            interval.tick().await;
            match sweep_once(&pool, source_grace_hours, version_grace_hours, abort_grace_hours)
                .await
            {
                Ok(stats)
                    if stats.deleted_slugs.is_empty()
                        && stats.deleted_versions.is_empty()
                        && stats.deleted_aborted_runs.is_empty() =>
                {
                    tracing::debug!("retention sweep tick — nothing to delete");
                }
                Ok(stats) => {
                    tracing::info!(
                        deleted_sources = stats.deleted_source_count(),
                        slugs = ?stats.deleted_slugs,
                        deleted_versions = stats.deleted_version_count(),
                        versions = ?stats.deleted_versions,
                        deleted_aborted_runs = stats.deleted_aborted_count(),
                        aborted_runs = ?stats.deleted_aborted_runs,
                        "retention sweep complete",
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "retention sweep failed; will retry");
                }
            }
        }
    })
}

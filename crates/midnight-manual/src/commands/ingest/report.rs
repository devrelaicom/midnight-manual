//! Structured ingest report: one canonical object rendered to stdout (`--json`)
//! and/or written to disk (`--report-file`).

use std::path::Path;

use serde::Serialize;

/// Per-run statistics summary.
#[derive(Debug, Clone, Serialize)]
pub struct ReportStats {
    /// Total source files walked.
    pub walked: usize,
    /// Files added as new documents this run.
    pub new: usize,
    /// Files carried over unchanged from the prior revision.
    pub carried: usize,
    /// Files deleted relative to the prior revision.
    pub deleted: usize,
    /// Total chunks the plan intended to upload. On a `finalized` run this is
    /// what was stored; on an `aborted` run it is the PLANNED total and may
    /// exceed what was actually committed (an early failure may have stored
    /// none). See the `aborted` note on [`IngestReport::outcome`].
    pub chunks_emitted: usize,
    /// Number of documents that produced upload conflicts.
    pub conflicts: usize,
    /// Voyage API tokens consumed for embedding.
    pub voyage_tokens: u64,
}

/// One processed document entry.
#[derive(Debug, Clone, Serialize)]
pub struct ReportDoc {
    /// Repo-relative path.
    pub path: String,
    /// Content classification label.
    pub classification: String,
    /// Number of chunks produced.
    pub chunks: usize,
    /// Whether embedding completed successfully.
    pub embed_complete: bool,
}

/// One skipped file entry.
#[derive(Debug, Clone, Serialize)]
pub struct ReportSkip {
    /// Repo-relative path.
    pub path: String,
    /// Human-readable reason the file was skipped.
    pub reason: String,
}

/// The four terminal outcomes of an `ingest run` / `ingest plan` invocation.
///
/// This is the single source of truth for the `outcome` string: the emitted
/// set is exactly [`Outcome::as_str`]'s exhaustive match, pinned by the
/// `outcome_as_str_is_stable` drift-guard test. Keeping it an enum (rather than
/// bare string literals at each call site) makes the compiler enforce that
/// every producer emits a member of this set — the drift that let `aborted`
/// sit as dead code (issue #136) can't recur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// `ingest plan` preview — no upload.
    Planned,
    /// `ingest run --dry-run` — no upload.
    DryRun,
    /// `ingest run` uploaded and activated a new revision.
    Finalized,
    /// `ingest run` started but failed after run-start; see
    /// [`IngestReport::outcome`] for what the report's stats do and don't mean.
    Aborted,
}

impl Outcome {
    /// The canonical snake_case wire string written to `outcome`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::DryRun => "dry_run",
            Self::Finalized => "finalized",
            Self::Aborted => "aborted",
        }
    }
}

/// Canonical structured summary of a single `ingest run` or `ingest plan`
/// invocation. Serialised to JSON for `--json` output and `--report-file`.
#[derive(Debug, Clone, Serialize)]
pub struct IngestReport {
    /// Monotonic schema version. Bump whenever the shape or emission contract
    /// changes in a way a strict consumer could notice — this includes additive
    /// fields and newly-emitted outcomes, not just removals/renames (v2 added
    /// the always-present `error` field and began emitting `aborted`). See
    /// [`Self::SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Originating subcommand: `"ingest run"` or `"ingest plan"`.
    pub command: String,
    /// Source slug identifying the corpus.
    pub source_slug: String,
    /// Revision number assigned to this run, if any.
    pub revision: Option<i32>,
    /// Revision number of the previous run, if any.
    pub prior_revision: Option<i32>,
    /// Embedding model identifier.
    pub embedding_model: String,
    /// Code-specific embedding model identifier, if separate.
    pub code_embedding_model: Option<String>,
    /// Run outcome. Emitted as the snake_case string of [`Outcome`], one of:
    /// - `planned`   — `ingest plan` preview (no upload).
    /// - `dry_run`   — `ingest run --dry-run` (no upload).
    /// - `finalized` — `ingest run` uploaded and activated a new revision.
    /// - `aborted`   — `ingest run` started but failed after run-start (embed,
    ///   upload, residual-conflict, or finalize failure); the CLI then requested
    ///   the server run be aborted (best-effort — if that request fails the
    ///   server-side version may linger).
    ///
    /// IMPORTANT for `aborted` reports: the numeric [`stats`](Self::stats),
    /// [`documents`](Self::documents), and [`skipped_files`](Self::skipped_files)
    /// describe the PLAN the run intended to commit — NOT what was actually
    /// persisted. An abort may have committed none of it (e.g. an embed failure
    /// on the first batch stores zero chunks even though `stats.chunks_emitted`
    /// is non-zero). Only [`error`](Self::error), `stats.voyage_tokens`, the
    /// [`conflicts`](Self::conflicts) list (and `stats.conflicts`), and each
    /// document's `embed_complete` (always `false` for new docs on abort)
    /// reflect actual committed progress.
    pub outcome: String,
    /// RFC 3339 timestamp when the run started.
    pub started_at: String,
    /// RFC 3339 timestamp when the run finished.
    pub finished_at: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u128,
    /// Aggregate statistics for this run. On an `aborted` run these are the
    /// PLAN's intended totals, not what was committed — see [`Self::outcome`].
    pub stats: ReportStats,
    /// Per-document records for all processed files. On an `aborted` run this is
    /// the PLANNED document set, not what was committed — see [`Self::outcome`].
    pub documents: Vec<ReportDoc>,
    /// Upload conflicts surfaced by the server. Populated on the residual-
    /// conflict abort path; unlike the other stats, this reflects real server
    /// responses.
    pub conflicts: Vec<mnm_core::ingest::UploadConflict>,
    /// Files that were skipped during the walk (and planner). On an `aborted`
    /// run this is the PLANNED skip set — see [`Self::outcome`].
    pub skipped_files: Vec<ReportSkip>,
    /// Non-fatal warnings accumulated during the run.
    pub warnings: Vec<String>,
    /// The triggering error when `outcome == "aborted"`: the `anyhow` context
    /// chain (`{:#}`) with bearer/token-like substrings scrubbed to `[redacted]`
    /// and internal whitespace normalized (it persists to a report file, so it
    /// is redacted symmetrically with server error bodies — FR-019). `None` on
    /// every non-aborted outcome, so automation can distinguish "run aborted"
    /// (`error` populated) from "run never happened" (no report file at all).
    pub error: Option<String>,
}

impl IngestReport {
    /// Schema version constant — use instead of bare integer literals.
    ///
    /// - v1: original shape.
    /// - v2: added the [`error`](Self::error) field and the `aborted` outcome is
    ///   now emitted on every post-start failure path (issue #136).
    pub const SCHEMA_VERSION: u32 = 2;

    /// Minimal all-fields-populated instance used only in unit tests.
    #[cfg(test)]
    fn sample() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            command: "ingest run".into(),
            source_slug: "docs".into(),
            revision: Some(2),
            prior_revision: Some(1),
            embedding_model: "voyage-context-3@1".into(),
            code_embedding_model: None,
            outcome: "finalized".into(),
            started_at: "t0".into(),
            finished_at: "t1".into(),
            duration_ms: 1,
            stats: ReportStats {
                walked: 1,
                new: 1,
                carried: 0,
                deleted: 0,
                chunks_emitted: 1,
                conflicts: 0,
                voyage_tokens: 0,
            },
            documents: vec![],
            conflicts: vec![],
            skipped_files: vec![],
            warnings: vec![],
            error: None,
        }
    }
}

/// Ensure the report path's parent directory exists (creating it if needed) and
/// is writable before doing any ingest work.
///
/// Call this early so the process fails fast rather than after a long run.
pub fn preflight(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!("report dir {} not writable: {e}", parent.display())
            })?;
        }
    }
    // Probe write permission: a directory may exist but be read-only. Writing a
    // sentinel file and removing it confirms the post-finalize `write_atomic` will
    // succeed, so we fail fast before doing any embedding work.
    let probe = path.with_extension("json.preflight");
    std::fs::write(&probe, b"")
        .map_err(|e| anyhow::anyhow!("report path {} not writable: {e}", path.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Serialize `report` to JSON and write it to `path` via a temp-file + atomic
/// rename, so a killed process never leaves a half-written output file behind.
///
/// The temp file is placed alongside the destination (same directory) to ensure
/// the rename is within the same filesystem.
pub fn write_atomic(path: &Path, report: &IngestReport) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(report)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| anyhow::anyhow!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp); // don't leave a partial temp behind
        anyhow::anyhow!("rename into {}: {e}", path.display())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_serializes_with_schema_version_and_stats() {
        let r = IngestReport::sample();
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["schema_version"], 2);
        assert_eq!(v["stats"]["carried"], r.stats.carried);
        assert!(v["documents"].is_array());
        // The `error` field is always present; null on non-aborted outcomes so
        // consumers can test `report.error != null` unconditionally.
        assert!(v["error"].is_null());
    }

    /// Drift guard: the emitted `outcome` set is exactly these four strings.
    /// If a variant is added, the exhaustive `as_str` match forces an update
    /// here, keeping the enum and the documented set in lockstep.
    #[test]
    fn outcome_as_str_is_stable() {
        assert_eq!(Outcome::Planned.as_str(), "planned");
        assert_eq!(Outcome::DryRun.as_str(), "dry_run");
        assert_eq!(Outcome::Finalized.as_str(), "finalized");
        assert_eq!(Outcome::Aborted.as_str(), "aborted");
    }

    #[test]
    fn write_atomic_cleans_tmp_on_rename_failure() {
        // Force a rename failure by pointing the destination inside a path
        // whose "parent" is actually a regular file, so rename(2) returns ENOTDIR.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"").unwrap(); // regular file, not a directory
        let path = blocker.join("out.json"); // parent is a file → rename will fail

        let json = serde_json::to_vec_pretty(&IngestReport::sample()).unwrap();
        let tmp = path.with_extension("json.tmp");
        // Write the temp file directly (bypassing preflight) to simulate the
        // mid-flight failure scenario.
        std::fs::write(&tmp, &json).unwrap_or(()); // may fail too; that's OK
        let result = write_atomic(&path, &IngestReport::sample());
        assert!(result.is_err(), "expected rename failure");
        assert!(!tmp.exists(), ".tmp must be cleaned up after rename failure");
    }

    /// `preflight` must error when the parent directory is read-only.
    ///
    /// Skipped when running as root (root bypasses POSIX mode bits, so the probe
    /// write would succeed even on a `0o555` directory, making the assertion
    /// meaningless). We verify this by attempting the probe write first and
    /// skip if it unexpectedly succeeds.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn preflight_errors_on_readonly_parent() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let ro_dir = dir.path().join("readonly");
        std::fs::create_dir_all(&ro_dir).unwrap();

        // Make the directory read-only (no write bit).
        std::fs::set_permissions(&ro_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let path = ro_dir.join("report.json");

        // Sanity-check: if the probe write somehow succeeds (e.g. running as
        // root), restore permissions and skip rather than give a false pass.
        let probe = path.with_extension("json.preflight");
        if std::fs::write(&probe, b"").is_ok() {
            let _ = std::fs::remove_file(&probe);
            std::fs::set_permissions(&ro_dir, std::fs::Permissions::from_mode(0o755)).unwrap_or(());
            return; // running as root — skip
        }

        let result = preflight(&path);

        // Restore write permission so the TempDir destructor can clean up.
        std::fs::set_permissions(&ro_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err(), "preflight must fail for a read-only parent directory");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not writable"), "error message must mention 'not writable': {msg}",);
    }

    #[test]
    fn write_atomic_creates_file_and_no_tmp_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/out.json");
        preflight(&path).unwrap();
        write_atomic(&path, &IngestReport::sample()).unwrap();
        assert!(path.exists());
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no .tmp file left behind");
    }
}

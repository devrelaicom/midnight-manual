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
    /// Total chunks emitted to the store.
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

/// Canonical structured summary of a single `ingest run` or `ingest plan`
/// invocation. Serialised to JSON for `--json` output and `--report-file`.
#[derive(Debug, Clone, Serialize)]
pub struct IngestReport {
    /// Monotonic schema version; bump only on breaking field changes.
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
    /// Run outcome: `planned` | `dry_run` | `finalized` | `aborted`.
    pub outcome: String,
    /// RFC 3339 timestamp when the run started.
    pub started_at: String,
    /// RFC 3339 timestamp when the run finished.
    pub finished_at: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u128,
    /// Aggregate statistics for this run.
    pub stats: ReportStats,
    /// Per-document records for all processed files.
    pub documents: Vec<ReportDoc>,
    /// Upload conflicts surfaced by the server.
    pub conflicts: Vec<mnm_core::ingest::UploadConflict>,
    /// Files that were skipped during the walk.
    pub skipped_files: Vec<ReportSkip>,
    /// Non-fatal warnings accumulated during the run.
    pub warnings: Vec<String>,
}

impl IngestReport {
    /// Schema version constant — use instead of bare `1` literals.
    pub const SCHEMA_VERSION: u32 = 1;

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
    std::fs::rename(&tmp, path)
        .map_err(|e| anyhow::anyhow!("rename into {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_serializes_with_schema_version_and_stats() {
        let r = IngestReport::sample();
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["stats"]["carried"], r.stats.carried);
        assert!(v["documents"].is_array());
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

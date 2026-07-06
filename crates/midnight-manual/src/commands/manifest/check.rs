//! `mnm manifest check` — purely-local manifest validation.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mnm_content::manifest::{Manifest, ManifestError};
use serde::Serialize;

/// Arguments for `mnm manifest check`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the `hierarchy.yaml` manifest to validate.
    pub manifest: PathBuf,
    /// Override the base directory for file-existence checks.
    #[arg(long)]
    pub base: Option<PathBuf>,
    /// Sitemap URL or file path to check coverage against (repeatable).
    #[arg(long = "sitemap")]
    pub sitemap: Vec<String>,
    /// Fail on any missing-file or unmatched-URL issue.
    #[arg(long)]
    pub strict: bool,
}

/// Closed set of issue categories emitted by `--json`.
///
/// Each variant maps 1:1 to one of the command's existing validation
/// categories. The set is closed: `--json` never emits a `kind` outside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum IssueKind {
    /// The manifest could not be read or parsed (single-issue envelope).
    ParseError,
    /// A `file:` / `path:` value is absolute or contains `..` (escapes the base).
    UnsafePath,
    /// A file is referenced as a `file:` leaf under more than one parent.
    DuplicateFile,
    /// A `file:` reference does not exist on disk under the base.
    MissingFile,
    /// A resolved leaf has no matching sitemap URL. Advisory by default; a
    /// blocking issue under `--strict`. Needs `--sitemap` to be evaluated.
    UnmatchedUrl,
}

/// One structured issue in the `--json` payload.
#[derive(Debug, Serialize)]
struct Issue {
    /// Which closed category this issue belongs to.
    kind: IssueKind,
    /// `true` when this issue gates `ok` / the exit code. Always blocking for
    /// `unsafe_path` / `duplicate_file` / `missing_file` / `parse_error`;
    /// `unmatched_url` is advisory (`false`) by default and blocking (`true`)
    /// under `--strict`. This makes the contract self-describing:
    /// `ok == !issues.any(|i| i.blocking)` and the fix-set is
    /// `issues.filter(|i| i.blocking)`.
    blocking: bool,
    /// The offending path (repo-relative file, or the manifest for `parse_error`).
    path: String,
    /// Human-readable explanation of the issue.
    detail: String,
}

impl Issue {
    /// A blocking issue (gates `ok` / the exit code).
    const fn blocking(kind: IssueKind, path: String, detail: String) -> Self {
        Self {
            kind,
            blocking: true,
            path,
            detail,
        }
    }

    /// An advisory issue (reported but never flips `ok`).
    const fn advisory(kind: IssueKind, path: String, detail: String) -> Self {
        Self {
            kind,
            blocking: false,
            path,
            detail,
        }
    }
}

/// Sitemap coverage summary for the `--json` payload.
#[derive(Debug, Serialize)]
struct SitemapCoverage {
    /// Leaves whose `published_url` matched a sitemap entry.
    matched: usize,
    /// Total resolved leaves considered.
    total: usize,
    /// `matched / total` as a percentage, rounded to one decimal place.
    pct: f64,
}

/// The single JSON document printed under `--json`.
#[derive(Debug, Serialize)]
struct CheckReport {
    /// `true` when there are no blocking issues (mirrors the exit code):
    /// equivalently, `!issues.any(|i| i.blocking)`.
    ok: bool,
    /// The manifest path as given on the command line (identifies the file even
    /// when many manifests share the basename `hierarchy.yaml`).
    manifest: String,
    /// Every issue found. Blocking categories (`unsafe_path` / `duplicate_file`
    /// / `missing_file`) plus `unmatched_url` (advisory by default, blocking
    /// under `--strict`); each carries a `blocking` discriminator. Note:
    /// `unsafe_path` / `duplicate_file` are reported first-match (`validate()`
    /// short-circuits, at most one per run); `missing_file` / `unmatched_url`
    /// are exhaustive.
    issues: Vec<Issue>,
    /// Sitemap coverage, or `null` when `--sitemap` was not supplied (or its
    /// fetch failed under `--json`, which degrades to `null` rather than error).
    sitemap_coverage: Option<SitemapCoverage>,
}

/// Run `mnm manifest check`.
///
/// When `json` is `true`, exactly one `CheckReport` JSON document is written to
/// stdout and human diagnostics are suppressed; the process exit code is
/// identical to the non-JSON path (blocking issues → error, otherwise success).
pub async fn run(args: Args, json: bool) -> Result<()> {
    // Load: read + parse. Under `--json`, surface load failures as a
    // single-issue `parse_error` envelope so callers always receive JSON when
    // they asked for it; either way the original error still propagates so the
    // exit code and (non-JSON) stderr message are unchanged.
    let body = match std::fs::read_to_string(&args.manifest)
        .with_context(|| format!("read {}", args.manifest.display()))
    {
        Ok(b) => b,
        Err(e) => return load_failure(json, &args.manifest, e),
    };
    let manifest = match Manifest::parse(&body).context("parse manifest") {
        Ok(m) => m,
        Err(e) => return load_failure(json, &args.manifest, e),
    };

    // Blocking validations (unchanged categories). `issues` drives the legacy
    // human output byte-for-byte; `structured` mirrors it for `--json` and also
    // carries the advisory `unmatched_url` entries.
    let mut issues: Vec<String> = Vec::new();
    let mut structured: Vec<Issue> = Vec::new();

    if let Err(e) = manifest.validate() {
        issues.push(format!("schema/paths: {e}"));
        structured.push(validate_issue(&e, &args.manifest));
    }

    let base = args.base.clone().unwrap_or_else(|| {
        args.manifest
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });

    let missing = manifest.validate_files_exist(&base);
    for m in &missing {
        issues.push(format!("missing file: {}", m.display()));
        structured.push(Issue::blocking(
            IssueKind::MissingFile,
            m.display().to_string(),
            "referenced by a `file:` node but not found under the manifest base".to_owned(),
        ));
    }

    // Sitemap coverage: prints the human line (non-JSON) or returns a summary.
    // In both paths it appends `unmatched_url` issues to `structured` when they
    // matter — always under `--json`, and under `--strict` where each is marked
    // blocking. No `--sitemap` → nothing to report.
    let coverage =
        sitemap_coverage(&manifest, &base, &args.sitemap, json, args.strict, &mut structured)
            .await?;

    // The blocking count gates `ok` / the exit code. It is derived from
    // `structured` (the single source of truth) so `--strict`-escalated
    // `unmatched_url` findings are counted: previously `--strict` was declared
    // but never read, so an unmatched-URL coverage gate silently passed. Without
    // `--strict`, `unmatched_url` stays advisory and the count is unchanged.
    let blocking = structured.iter().filter(|i| i.blocking).count();

    if json {
        return emit_json(&args.manifest, blocking, structured, coverage);
    }

    // Non-JSON: mirror any `--strict`-escalated `unmatched_url` findings into the
    // human issue list so the coverage gate produces a non-zero exit AND names
    // each offending leaf (parity with the `--json` path's blocking set).
    if args.strict {
        for i in &structured {
            if i.kind == IssueKind::UnmatchedUrl {
                issues.push(format!("unmatched url: {} ({})", i.path, i.detail));
            }
        }
    }

    if issues.is_empty() {
        eprintln!("ok");
        return Ok(());
    }
    for i in &issues {
        eprintln!("- {i}");
    }
    Err(anyhow!("{} issue(s)", issues.len()))
}

/// Resolve the manifest, compute sitemap coverage, and either print the legacy
/// human coverage line (non-JSON) or return a [`SitemapCoverage`] (JSON).
///
/// Appends an `unmatched_url` [`Issue`] to `structured` for every leaf with no
/// sitemap match whenever `json || strict`. The issue is **blocking** when
/// `strict` is set (so `--strict` escalates the coverage gate to a non-zero
/// exit) and **advisory** otherwise (preserving the default JSON contract).
///
/// Returns `None` when `--sitemap` was not supplied.
async fn sitemap_coverage(
    manifest: &Manifest,
    base: &Path,
    specs: &[String],
    json: bool,
    strict: bool,
    structured: &mut Vec<Issue>,
) -> Result<Option<SitemapCoverage>> {
    if specs.is_empty() {
        return Ok(None);
    }
    let sitemap_urls = match super::generate::load_sitemaps(specs).await {
        Ok(urls) => urls,
        // Coverage is advisory: under `--json` a fetch failure must not sink the
        // whole report (the fix-loop still needs the blocking issues), so degrade
        // to `sitemap_coverage: null`. The non-JSON path propagates as before.
        Err(e) if json => {
            tracing::debug!(
                target: "midnight-manual::manifest::check",
                error = %e,
                "sitemap load failed under --json; degrading coverage to null"
            );
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let leaves = mnm_content::manifest::resolve::resolve(
        manifest,
        base,
        mnm_content::manifest::resolve::FilterRunOptions::default(),
    );
    let total = leaves.len();
    let mut matched = 0usize;
    for leaf in &leaves {
        let hit = leaf
            .published_url
            .as_ref()
            .is_some_and(|u| sitemap_urls.iter().any(|s| s.as_str() == u));
        if hit {
            matched += 1;
        } else if json || strict {
            // Record the finding when it will be consumed: always under `--json`
            // (for the report), and under `--strict` (to gate the exit, in both
            // output modes). `--strict` makes it blocking; otherwise advisory.
            let path = leaf.rel_path.display().to_string();
            let detail = leaf.published_url.as_ref().map_or_else(
                || "leaf has no published_url".to_owned(),
                |u| format!("no sitemap match for {u}"),
            );
            structured.push(if strict {
                Issue::blocking(IssueKind::UnmatchedUrl, path, detail)
            } else {
                Issue::advisory(IssueKind::UnmatchedUrl, path, detail)
            });
        }
    }

    if json {
        return Ok(Some(SitemapCoverage {
            matched,
            total,
            pct: coverage_pct(matched, total),
        }));
    }
    // Byte-for-byte identical to the pre-`--json` output.
    eprintln!(
        "sitemap coverage: {matched}/{total} ({}%)",
        if total == 0 {
            100
        } else {
            matched * 100 / total
        }
    );
    Ok(None)
}

/// Build and print the single `--json` [`CheckReport`], then map blocking issues
/// to the same exit status (`Err`) the non-JSON path produces.
fn emit_json(
    manifest_path: &Path,
    blocking: usize,
    issues: Vec<Issue>,
    coverage: Option<SitemapCoverage>,
) -> Result<()> {
    let report = CheckReport {
        ok: blocking == 0,
        manifest: manifest_path.display().to_string(),
        issues,
        sitemap_coverage: coverage,
    };
    println!("{}", serde_json::to_string(&report).context("serialize check report")?);
    if blocking == 0 {
        Ok(())
    } else {
        Err(anyhow!("{blocking} issue(s)"))
    }
}

/// Emit a `parse_error` envelope (under `--json`) and always return the
/// original error so the exit code and non-JSON stderr output are unchanged.
fn load_failure(json: bool, manifest_path: &Path, err: anyhow::Error) -> Result<()> {
    if json {
        let report = CheckReport {
            ok: false,
            manifest: manifest_path.display().to_string(),
            // `detail` is "the most specific human message available": the full
            // anyhow chain (`{:#}`) here, since a single `parse_error` kind
            // collapses read / YAML-parse / schema-version failures and the chain
            // is what disambiguates them.
            issues: vec![Issue::blocking(
                IssueKind::ParseError,
                manifest_path.display().to_string(),
                format!("{err:#}"),
            )],
            sitemap_coverage: None,
        };
        // Best-effort: never let a serialization hiccup mask the real error.
        if let Ok(s) = serde_json::to_string(&report) {
            println!("{s}");
        }
    }
    Err(err)
}

/// Map a [`ManifestError`] from `validate()` to its structured [`Issue`].
///
/// `validate()` only surfaces [`ManifestError::UnsafePath`] and
/// [`ManifestError::DuplicateFile`]; the load-time variants are mapped to
/// `parse_error` defensively so the match stays exhaustive. All are blocking.
fn validate_issue(err: &ManifestError, manifest_path: &Path) -> Issue {
    let (kind, path) = match err {
        ManifestError::UnsafePath(p) => (IssueKind::UnsafePath, p.display().to_string()),
        ManifestError::DuplicateFile(p) => (IssueKind::DuplicateFile, p.display().to_string()),
        ManifestError::Parse(_) | ManifestError::SchemaVersionMismatch { .. } => {
            (IssueKind::ParseError, manifest_path.display().to_string())
        }
    };
    // `detail` is the specific single-line error message (see `load_failure`).
    Issue::blocking(kind, path, err.to_string())
}

/// `matched / total` as a percentage rounded to one decimal place; `100.0`
/// when there is nothing to cover.
#[allow(clippy::cast_precision_loss)] // coverage percentages: sub-f64 precision is irrelevant
fn coverage_pct(matched: usize, total: usize) -> f64 {
    if total == 0 {
        return 100.0;
    }
    let raw = matched as f64 * 100.0 / total as f64;
    (raw * 10.0).round() / 10.0
}

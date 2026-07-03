//! `mnm manifest <subcommand>` dispatcher. See §1 of the ingest-UX
//! design spec.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

pub mod check;
pub mod generate;
pub mod init;

/// Top-level arguments for `mnm manifest`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The manifest subcommand to execute.
    #[command(subcommand)]
    pub cmd: ManifestCmd,
}

/// `mnm manifest` subcommands.
#[derive(Debug, Subcommand)]
pub enum ManifestCmd {
    /// Write an empty starter manifest with comments.
    Init(init::Args),
    /// Populate a hierarchy.yaml from globs + optional sitemap.
    Generate(generate::Args),
    /// Validate a manifest locally: schema, paths, file existence.
    ///
    /// With the global `--json` flag, one JSON document is written to stdout:
    /// `{ ok, manifest, issues: [{ kind, blocking, path, detail }], sitemap_coverage }`.
    /// `kind` is a closed set:
    ///
    /// * `parse_error`    — the manifest could not be read or parsed
    ///   (single-issue envelope; `blocking`).
    /// * `unsafe_path`    — a `file:` / `path:` is absolute or contains `..`
    ///   (escapes the manifest base; `blocking`).
    /// * `duplicate_file` — a file is referenced as a `file:` leaf under more
    ///   than one parent (`blocking`).
    /// * `missing_file`   — a `file:` reference does not exist under the base
    ///   (`blocking`).
    /// * `unmatched_url`  — a resolved leaf has no matching sitemap URL
    ///   (advisory, `blocking:false`; requires `--sitemap`).
    ///
    /// Each issue carries a `blocking` boolean, so the contract is
    /// self-describing: `ok == !issues.any(|i| i.blocking)` and the fix-set is
    /// `issues.filter(|i| i.blocking)`. `ok` and the exit code mirror the
    /// non-`--json` behaviour (blocking issues → error; `unmatched_url` never
    /// flips them).
    ///
    /// Exhaustiveness: `unsafe_path` / `duplicate_file` are reported first-match
    /// — `validate()` short-circuits, so at most one path problem surfaces per
    /// run; `missing_file` and `unmatched_url` are reported exhaustively.
    ///
    /// `sitemap_coverage` is `{ matched, total, pct }` (`pct` a float rounded to
    /// one decimal), or `null` when `--sitemap` is absent (or its fetch failed,
    /// which degrades to `null` rather than erroring the report).
    Check(check::Args),
}

/// Dispatch `mnm manifest <subcommand>`.
///
/// `json` is the global `--json` flag; only `check` currently honours it.
pub async fn run(args: Args, json: bool) -> Result<()> {
    match args.cmd {
        ManifestCmd::Init(a) => init::run(&a),
        ManifestCmd::Generate(a) => generate::run(a).await,
        ManifestCmd::Check(a) => check::run(a, json).await,
    }
}

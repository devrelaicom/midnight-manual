//! `mnm ingest plan` — compute the full ingest plan without starting a
//! server-side ingest run.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use mnm_content::ingest::{PlanBuilder, PriorState, WalkContext, Walker};
use mnm_content::manifest::Manifest;
use mnm_core::types::{DocumentKind, SourceKind};
use time::OffsetDateTime;

/// Args for `mnm ingest plan`.
#[derive(Debug, ClapArgs)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Path to the `hierarchy.yaml` manifest.
    pub manifest: PathBuf,

    /// Slug of the source to plan against.
    #[arg(long)]
    pub source_slug: String,

    /// Free-form revision label (often a git SHA). Defaults to `git rev-parse
    /// --short HEAD` in the source root; falls back to "unknown".
    #[arg(long)]
    pub revision: Option<String>,

    /// Embedding-model wire id (`name@revision`). Defaults to `auto`, which
    /// resolves the corpus's active model (matching `mnm ingest run` / `search`).
    #[arg(long, default_value = "auto")]
    pub embedding_model: String,

    /// Override the source root directory (default: the manifest's parent dir).
    #[arg(long)]
    pub base: Option<PathBuf>,

    /// Honour the repo's own .gitignore / .git/info/exclude during discovery
    /// (off by default — ingest is hermetic). Never reads the machine-global
    /// (core.excludesFile) or parent-directory ignore files.
    #[arg(long)]
    pub respect_gitignore: bool,

    /// Disable the built-in default skip-list (node_modules, target, vendor, dist,
    /// build, out, coverage, managed, __snapshots__, lockfiles, minified/generated,
    /// boilerplate .md) so those files are walked during discovery.
    #[arg(long)]
    pub disable_default_ignore_list: bool,

    /// Fail the whole plan if a chunker panics while planning a new or changed
    /// file, instead of degrading that file to the line-window fallback with a
    /// warning (issue #121).
    #[arg(long)]
    pub strict: bool,

    /// Emit a single-line JSON object instead of the human-readable summary.
    #[arg(long)]
    pub json: bool,

    /// Write the structured IngestReport (JSON) to this path, in addition to
    /// the stdout summary. Orthogonal to --json.
    #[arg(long, value_name = "PATH")]
    pub report_file: Option<PathBuf>,
}

/// Dispatch `mnm ingest plan`.
///
/// # Errors
///
/// Returns `anyhow::Error` if the manifest cannot be read, the source tree
/// walk fails, or the plan builder encounters a duplicate path.
#[allow(clippy::too_many_lines)]
pub async fn run(
    args: Args,
    server: Option<&str>,
    config: Option<&Path>,
    _json: bool,
) -> Result<()> {
    let started_at = OffsetDateTime::now_utc();

    // Fail fast before any work if the report path is not writable.
    if let Some(rp) = &args.report_file {
        super::report::preflight(rp).context("report-file preflight")?;
    }

    let server_url = crate::shared::resolve_server_url(server, config);
    let body = std::fs::read_to_string(&args.manifest)
        .with_context(|| format!("read manifest at {}", args.manifest.display()))?;
    let manifest = Manifest::parse(&body).context("parse manifest")?;
    let base = args.base.clone().unwrap_or_else(|| {
        args.manifest
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });

    // `ingest plan` has no --max-file-size or --max-line-bytes flag, so the
    // walker uses its default ceilings (DEFAULT_MAX_FILE_BYTES for size,
    // DEFAULT_MAX_LINE_BYTES for the longest line — the latter skips
    // machine-generated data like chain-specs). Skipped files are warned,
    // mirroring the resilient behavior of `ingest run`; the loop below prints
    // each `skip.reason` generically, so LongLine skips surface automatically.
    let w = Walker::new(manifest.clone(), base.clone()).with_filter_options(
        mnm_content::manifest::resolve::FilterRunOptions {
            respect_gitignore: args.respect_gitignore,
            default_ignore_list: !args.disable_default_ignore_list,
        },
    );
    let outcome = w.walk().context("walk source tree")?;
    for skip in &outcome.skipped {
        tracing::warn!(
            path = %skip.rel_path.display(),
            reason = %skip.reason,
            "skipping file during ingest plan walk",
        );
    }
    let walk_skipped = outcome.skipped;
    let walked = outcome.documents;

    // Resolve the run's embedding model wire id the same way `ingest run` does:
    // when "auto" (the default), fetch the corpus's active model from the server
    // so the model gate compares apples-to-apples. An explicit override is used
    // verbatim, matching the run.rs behaviour.
    let run_model = if args.embedding_model == super::run::DEFAULT_EMBEDDING_MODEL {
        match crate::commands::models::fetch_active(&server_url).await {
            Ok(active) => format!("{}@{}", active.name, active.revision),
            Err(e) => {
                tracing::warn!(error = %e, "could not resolve active model for plan; treating all as new");
                // Fall back to an empty string that will never match any stored
                // model wire id, so the model gate correctly returns PriorState::default().
                String::new()
            }
        }
    } else {
        args.embedding_model.clone()
    };

    // Load the admin bearer token if available (same path as `ingest run`).
    // `ingest plan` must work without a token — a 401 from the inventory
    // endpoint returns PriorState::default() (all-new), which is the safe
    // pre-existing behaviour.
    let token = load_optional_admin_token();

    // Without an admin token the prior-version inventory fetch 401s and falls
    // back to an empty PriorState, so EVERY document is classified "new" — a
    // silently worst-case cost preview. Surface that on stderr (for interactive
    // users) here AND in the report's `warnings[]` (for `--json` /
    // `--report-file` consumers) via `build_plan_report` below, so the
    // degradation is never invisible (#140). Both derive from the same
    // `plan_warnings(token)` source so they can never diverge.
    for w in &plan_warnings(token.as_deref()) {
        eprintln!("{w}");
    }

    // Pass `None` for the code-embedding model: `ingest plan` intentionally
    // ignores code-embedding carry, so it conservatively over-reports "new" for
    // code sources (never under-reports), keeping the preview safe to act on.
    let prior = fetch_prior_state(
        &server_url,
        &args.source_slug,
        &run_model,
        None, // plan has no code-embedding flag; treated as None (no code model)
        token.as_deref().unwrap_or(""),
    )
    .await
    .unwrap_or_default();

    let revision = args
        .revision
        .clone()
        .unwrap_or_else(|| super::infer_revision(&base));

    let mut b = PlanBuilder::new(&args.source_slug, SourceKind::DocsSite, &revision, prior)
        .with_strict(args.strict);
    for doc in walked {
        let ctx = WalkContext {
            path: doc.rel_path.clone(),
            kind: DocumentKind::Markdown,
            content: &doc.content,
            split: &doc.split,
            resolved: &doc.resolved,
            // Task 10 fills machine-extracted provenance; empty for now.
            extracted: mnm_core::provenance::Provenance::default(),
            source_modified_at: doc.source_modified_at,
            package: None,
        };
        b.add_walked_document(&ctx)
            .with_context(|| format!("plan add {}", doc.rel_path.display()))?;
    }
    let plan = b.finalize();
    let finished_at = OffsetDateTime::now_utc();

    // Resolve the embedding-model wire id for the report: same logic as above
    // but we already have `run_model` at this point.
    let embedding_model_for_report = if run_model.is_empty() {
        "auto"
    } else {
        &run_model
    };

    let sel = super::run::ReportSelection::new(args.json, args.report_file.as_deref());
    let report = build_plan_report(
        token.as_deref(),
        &args.source_slug,
        embedding_model_for_report,
        started_at,
        finished_at,
        &plan,
        &walk_skipped,
    );

    super::run::emit_report(&report, &sel, args.report_file.as_deref(), || {
        use std::fmt::Write as _;
        let mut out = String::new();
        // Build the human-readable plan summary as a return value so the
        // renderer is a pure FnOnce() -> String (no direct stdout writes here).
        let _ = write!(
            out,
            "plan for source `{}` (rev {}):\n  walked       {} files\n  chunked      {} chunks\n    new          {} documents\n    carried      {} documents\n    deleted      {} documents",
            plan.source_slug,
            plan.target_revision,
            plan.new_documents.len() + plan.carried_documents.len(),
            plan.stats.chunks_emitted,
            plan.stats.documents_added,
            plan.stats.documents_carried,
            plan.stats.documents_deleted,
        );
        out
    });
    Ok(())
}

/// The stderr line + report `warnings[]` entry emitted when `ingest plan` runs
/// without an admin token: the prior-version inventory endpoint 401s, so every
/// document is classified as new and the cost preview is worst-case (#140).
const TOKENLESS_PLAN_WARNING: &str =
    "no admin token — prior-version inventory unavailable; every document is \
     classified as new, so the cost preview is worst-case (run `mnm login \
     --user-id <id>` for carry-forward-aware plans)";

/// Warnings to surface (stderr + report) for a plan run. Currently exactly one:
/// the tokenless-degradation notice when `token` is absent. Pure so the exact
/// wording and the token→warning mapping are unit-testable without a network
/// round-trip (#140).
fn plan_warnings(token: Option<&str>) -> Vec<String> {
    if token.is_none() {
        vec![TOKENLESS_PLAN_WARNING.to_owned()]
    } else {
        Vec::new()
    }
}

/// Assemble the `ingest plan` report, threading `plan_warnings(token)` into the
/// report's `warnings[]`. `run()` delegates here so the token→`warnings[]`
/// wiring is a pure, network-free seam: dropping the warning (e.g. reverting
/// this call's warnings arg to `Vec::new()`) fails `build_plan_report_*` below.
/// The serialization-funnel test on `assemble_report` cannot catch that
/// regression because it passes a hardcoded vec — this seam pins the CALL SITE.
#[allow(clippy::too_many_arguments)]
fn build_plan_report(
    token: Option<&str>,
    source_slug: &str,
    embedding_model: &str,
    started_at: OffsetDateTime,
    finished_at: OffsetDateTime,
    plan: &mnm_content::ingest::IngestPlan,
    walk_skipped: &[mnm_content::ingest::SkippedFile],
) -> super::report::IngestReport {
    super::run::assemble_report(
        "ingest plan",
        source_slug,
        super::report::Outcome::Planned,
        None,
        None,
        embedding_model,
        None, // plan has no code-embedding model
        started_at,
        finished_at,
        plan,
        walk_skipped,
        Vec::new(), // conflicts: plan does not upload
        plan_warnings(token),
        0,
    )
}

/// Load the admin bearer token from `auth.toml` if present and not expired.
/// Returns `None` when the file is missing, the `[admin]` section is absent,
/// or the token is expired. Never errors — `ingest plan` degrades gracefully.
fn load_optional_admin_token() -> Option<String> {
    let env = mnm_core::config::StdEnv;
    let auth_path = mnm_core::paths::auth_file_path(&env)?;
    let auth_file = mnm_core::auth_file::AuthFile::read_optional(&auth_path).ok()??;
    auth_file
        .active_admin_token(time::OffsetDateTime::now_utc())
        .map(ToOwned::to_owned)
}

/// Carry-forward is only valid when the prior version's models exactly match
/// this run's. Otherwise we must re-embed everything.
fn prior_state_applies(
    prior_model: &str,
    prior_code: Option<&str>,
    run_model: &str,
    run_code: Option<&str>,
) -> bool {
    prior_model == run_model && prior_code == run_code
}

/// Wire-format response from `GET /v1/admin/sources/:slug/active-version/documents`.
#[derive(serde::Deserialize)]
struct InventoryResponse {
    embedding_model: String,
    code_embedding_model: Option<String>,
    documents: Vec<InventoryDocWire>,
}

/// One document entry in the inventory response.
#[derive(serde::Deserialize)]
struct InventoryDocWire {
    source_path: String,
    content_hash: String,
    document_id: uuid::Uuid,
    embed_complete: bool,
}

/// Fetch the prior active source version's document inventory from the server.
///
/// On any network error, unreachable host, 404 (no prior version), or non-OK
/// response the function returns `Ok(PriorState::default())`, which causes the
/// plan to treat every document as new (safe, conservative). If the prior
/// version's embedding models differ from the run's the same fallback applies —
/// we must re-embed everything.
///
/// Shared with `ingest run` (`super::run`): `pub(super)` keeps it reachable from
/// the sibling command without duplicating the inventory-fetch + model-gate
/// logic, so plan and run can never diverge in how they classify carry-forward.
pub(super) async fn fetch_prior_state(
    server_url: &str,
    slug: &str,
    run_model: &str,
    run_code_model: Option<&str>,
    token: &str,
) -> Result<PriorState> {
    let url = format!("{server_url}/v1/admin/sources/{slug}/active-version/documents");
    let resp = reqwest::Client::new()
        .get(&url)
        .bearer_auth(token)
        .send()
        .await;
    let inv: InventoryResponse = match resp {
        Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => return Ok(PriorState::default()),
        Ok(r) if r.status().is_success() => r.json().await.context("decode inventory")?,
        Ok(r) => {
            tracing::warn!(status = %r.status(), "prior inventory non-OK; treating all as new");
            return Ok(PriorState::default());
        }
        Err(e) => {
            tracing::warn!(error = %e, "prior inventory fetch failed; treating all as new");
            return Ok(PriorState::default());
        }
    };
    if !prior_state_applies(
        &inv.embedding_model,
        inv.code_embedding_model.as_deref(),
        run_model,
        run_code_model,
    ) {
        tracing::info!("embedding model changed since prior version; re-embedding all documents");
        return Ok(PriorState::default());
    }
    Ok(PriorState {
        documents: inv
            .documents
            .into_iter()
            .filter(|d| d.embed_complete)
            .map(|d| mnm_content::ingest::PriorDocument {
                path: d.source_path.into(),
                content_hash: d.content_hash,
                document_id: d.document_id,
            })
            .collect(),
    })
}

#[cfg(test)]
mod prior_state_gate_tests {
    use super::prior_state_applies;

    #[test]
    fn prior_state_dropped_on_model_change() {
        // same models -> applies
        assert!(prior_state_applies("voyage-context-3@1", None, "voyage-context-3@1", None));
        // general model changed -> does not apply
        assert!(!prior_state_applies("voyage-context-3@1", None, "voyage-context-3@2", None));
        // code model toggled on -> does not apply (prior had none)
        assert!(!prior_state_applies(
            "voyage-context-3@1",
            None,
            "voyage-context-3@1",
            Some("voyage-code-3@1")
        ));
    }
}

#[cfg(test)]
mod plan_warning_tests {
    use super::{build_plan_report, plan_warnings};
    use mnm_content::ingest::{IngestPlan, PlanBuilder, PriorState};
    use mnm_core::types::SourceKind;
    use time::OffsetDateTime;

    /// A minimal finalized plan (no walked docs) — enough to assemble a report;
    /// the seam tests care about `warnings[]`, not the document lists.
    fn empty_plan() -> IngestPlan {
        PlanBuilder::new("docs", SourceKind::DocsSite, "rev-1", PriorState::default()).finalize()
    }

    /// Tokenless plan surfaces exactly one warning — the #140 degradation
    /// notice — which the caller writes to stderr AND threads into the report's
    /// `warnings[]`. The wording names the real recovery command (`mnm login
    /// --user-id <id>`).
    #[test]
    fn tokenless_plan_emits_degradation_warning() {
        let warnings = plan_warnings(None);
        assert_eq!(warnings.len(), 1, "exactly one warning when tokenless");
        let w = &warnings[0];
        assert!(w.contains("no admin token"), "names the cause: {w}");
        assert!(
            w.contains("every document is classified as new"),
            "explains the classification: {w}"
        );
        assert!(w.contains("worst-case"), "flags the cost inflation explicitly: {w}");
        assert!(w.contains("mnm login --user-id <id>"), "names the fix: {w}");
    }

    /// A tokened plan is unchanged: no warning, so `warnings[]` stays empty.
    #[test]
    fn tokened_plan_emits_no_warning() {
        assert!(plan_warnings(Some("admin-bearer")).is_empty());
    }

    /// The CALL-SITE guard: `build_plan_report(None, …)` must thread the
    /// tokenless warning into the report's serialized `warnings[]`. This fails
    /// if the seam's `plan_warnings(token)` arg is reverted to `Vec::new()` —
    /// the regression the `assemble_report` funnel test (hardcoded vec) misses.
    #[test]
    fn build_plan_report_threads_tokenless_warning_into_warnings_array() {
        let plan = empty_plan();
        let t = OffsetDateTime::now_utc();
        let report = build_plan_report(None, "docs", "voyage-context-3@1", t, t, &plan, &[]);
        assert_eq!(report.warnings.len(), 1, "tokenless report must carry the notice");
        assert!(report.warnings[0].contains("no admin token"), "{:?}", report.warnings);
        // And it survives serialization into the `warnings[]` array `--json` /
        // `--report-file` consumers read.
        let v = serde_json::to_value(&report).unwrap();
        assert!(
            v["warnings"][0]
                .as_str()
                .is_some_and(|w| w.contains("no admin token")),
            "warnings must serialize into `warnings[]`: {v}"
        );
    }

    /// Tokened plans are unchanged through the seam: empty `warnings[]`.
    #[test]
    fn build_plan_report_tokened_has_no_warnings() {
        let plan = empty_plan();
        let t = OffsetDateTime::now_utc();
        let report = build_plan_report(Some("admin-bearer"), "docs", "m@1", t, t, &plan, &[]);
        assert!(
            report.warnings.is_empty(),
            "tokened report must not warn: {:?}",
            report.warnings
        );
    }
}

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
pub async fn run(args: Args, server: Option<&str>, _json: bool) -> Result<()> {
    let started_at = OffsetDateTime::now_utc();

    // Fail fast before any work if the report path is not writable.
    if let Some(rp) = &args.report_file {
        super::report::preflight(rp).context("report-file preflight")?;
    }

    let server_url = crate::shared::resolve_server_url(server);
    let body = std::fs::read_to_string(&args.manifest)
        .with_context(|| format!("read manifest at {}", args.manifest.display()))?;
    let manifest = Manifest::parse(&body).context("parse manifest")?;
    let base = args.base.clone().unwrap_or_else(|| {
        args.manifest
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });

    // `ingest plan` has no --max-file-size flag, so the walker uses its default
    // ceiling (DEFAULT_MAX_FILE_BYTES); skipped files are warned, mirroring the
    // resilient behavior of `ingest run`.
    let w = Walker::new(manifest.clone(), base.clone());
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

    let mut b = PlanBuilder::new(&args.source_slug, SourceKind::DocsSite, &revision, prior);
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
    let report = super::run::assemble_report(
        "ingest plan",
        &args.source_slug,
        "planned",
        None,
        None,
        embedding_model_for_report,
        None, // plan has no code-embedding model
        started_at,
        finished_at,
        &plan,
        &walk_skipped,
        Vec::new(),
        Vec::new(),
        0,
    );

    if sel.json_stdout {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        print_plan(&plan, false);
    }
    if sel.write_file {
        if let Some(path) = &args.report_file {
            if let Err(e) = super::report::write_atomic(path, &report) {
                eprintln!("warning: could not write report file {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    }
    Ok(())
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

/// Print the plan summary in human-readable or JSON form.
fn print_plan(plan: &mnm_content::ingest::IngestPlan, json: bool) {
    if json {
        let v = serde_json::to_value(plan).unwrap_or(serde_json::Value::Null);
        println!("{v}");
        return;
    }
    println!("plan for source `{}` (rev {}):", plan.source_slug, plan.target_revision);
    println!(
        "  walked       {} files",
        plan.new_documents.len() + plan.carried_documents.len()
    );
    println!("  chunked      {} chunks", plan.stats.chunks_emitted);
    println!("    new          {} documents", plan.stats.documents_added);
    println!("    carried      {} documents", plan.stats.documents_carried);
    println!("    deleted      {} documents", plan.stats.documents_deleted);
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

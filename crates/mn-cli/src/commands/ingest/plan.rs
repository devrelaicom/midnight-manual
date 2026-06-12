//! `mnm ingest plan` — compute the full ingest plan without starting a
//! server-side ingest run.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use mn_content::ingest::{PlanBuilder, PriorState, WalkContext, Walker};
use mn_content::manifest::Manifest;
use mn_core::types::{DocumentKind, SourceKind};

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
}

/// Dispatch `mnm ingest plan`.
///
/// # Errors
///
/// Returns `anyhow::Error` if the manifest cannot be read, the source tree
/// walk fails, or the plan builder encounters a duplicate path.
pub async fn run(args: Args, server: Option<&str>, _json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let body = std::fs::read_to_string(&args.manifest)
        .with_context(|| format!("read manifest at {}", args.manifest.display()))?;
    let manifest = Manifest::parse(&body).context("parse manifest")?;
    let base = args.base.clone().unwrap_or_else(|| {
        args.manifest
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });

    let w = Walker::new(manifest.clone(), base.clone());
    let walked = w.walk().context("walk source tree")?;

    // TODO(Task 26): replace the stub with GET /v1/sources/:slug/active-version/documents
    // once that endpoint exists. For now we always start from an empty prior state.
    let prior = fetch_prior_state(&server_url, &args.source_slug)
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
            extracted: mn_core::provenance::Provenance::default(),
            source_modified_at: doc.source_modified_at,
            package: None,
        };
        b.add_walked_document(&ctx)
            .with_context(|| format!("plan add {}", doc.rel_path.display()))?;
    }
    let plan = b.finalize();
    print_plan(&plan, args.json);
    Ok(())
}

/// Fetch the prior active source version's document inventory from the server.
///
/// On any network error or unreachable host the function returns `Ok(Default)`,
/// which causes the plan to treat every document as new (safe, conservative).
async fn fetch_prior_state(server_url: &str, slug: &str) -> Result<PriorState> {
    // TODO(Task 26): implement GET /v1/sources/:slug/active-version/documents
    // with the admin bearer token if present; deserialize body into
    // Vec<PriorDocument> and build a PriorState from it.
    let _ = (server_url, slug);
    Ok(PriorState::default())
}

/// Print the plan summary in human-readable or JSON form.
fn print_plan(plan: &mn_content::ingest::IngestPlan, json: bool) {
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

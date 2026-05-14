//! `mnm ingest <manifest>` — admin command that runs an end-to-end ingest
//! against the cloud server (Story 10).
//!
//! Flow:
//!
//! 1. Read the manifest (`hierarchy.yaml`) and validate it.
//!
//! 2. Walk the source root, parse frontmatter, run the Markdown chunker
//!    (via the orchestrator in [`mn_content::ingest`]).
//!
//! 3. Load the admin bearer from `auth.toml`.
//!
//! 4. `POST /v1/admin/sources/:slug/ingest-runs` — allocate a building
//!    source_version.
//!
//! 5. `PUT  /v1/admin/sources/:slug/ingest-runs/:id/documents` — upload
//!    every walked document with its chunks.
//!
//! 6. `POST /v1/admin/sources/:slug/ingest-runs/:id/finalize` — promote
//!    the run to `active`.
//!
//! 7. Emit a single `IngestComplete` telemetry event with the per-run stats.
//!
//! On any failure between steps 4 and 7 the CLI calls `.../abort` so the
//! building source_version doesn't block the next attempt (FR-022).

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mn_content::ingest::{PlanBuilder, PriorState, Walker};
use mn_content::manifest::Manifest;
use mn_core::auth_file::AuthFile;
use mn_core::provenance::Provenance;
use mn_core::types::{DocumentKind, SourceKind};
use mn_telemetry::events::{Component, EventPayload, Outcome};
use mn_telemetry::{Event, TelemetryClient};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Args for `mnm ingest`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the `hierarchy.yaml` manifest.
    pub manifest: PathBuf,

    /// Slug of the target source (must already exist in the corpus).
    #[arg(long)]
    pub source_slug: String,

    /// Free-form revision label (often a git SHA). Recorded on the
    /// `source_version` row for reproducibility (FR-019).
    #[arg(long, default_value = "unknown")]
    pub revision: String,

    /// Embedding-model wire id (`name@revision`). Defaults to
    /// `bge-base-en-v1.5@1` to match the corpus's current model.
    #[arg(long, default_value = "bge-base-en-v1.5@1")]
    pub embedding_model: String,

    /// Optional ingest note, recorded on the source_version row.
    #[arg(long)]
    pub note: Option<String>,

    /// Override the source root directory (default: the manifest's parent dir).
    #[arg(long)]
    pub source_root: Option<PathBuf>,

    /// Dry-run: walk + build the plan, print stats, do NOT post anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// Dispatch.
///
/// # Errors
///
/// Returns `anyhow::Error` if the manifest cannot be read, the source tree
/// walk fails, the auth.toml cannot be loaded, or any of the HTTP round-trips
/// fail.
pub async fn run(
    args: Args,
    server_flag: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server_flag);
    let env = mn_core::config::StdEnv;
    let auth_path = mn_core::paths::auth_file_path(&env)
        .ok_or_else(|| anyhow!("could not resolve auth.toml path (set XDG_CONFIG_HOME or HOME)"))?;
    run_with_paths(args, &server_url, &auth_path, telemetry, cli_version, json).await
}

/// Path-explicit driver, exposed for integration tests.
///
/// # Errors
///
/// Returns the same errors as [`run`].
pub async fn run_with_paths(
    args: Args,
    server_url: &str,
    auth_path: &Path,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    let started = Instant::now();
    let outcome = run_inner(&args, server_url, auth_path, json).await;

    let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    let (added, carried, deleted, telemetry_outcome) = match &outcome {
        Ok(stats) => (stats.added, stats.carried, stats.deleted, Outcome::Ok),
        Err(_) => (0, 0, 0, Outcome::Error),
    };
    telemetry
        .emit(Event::new(
            Component::Cli,
            cli_version,
            EventPayload::IngestComplete {
                documents_added: u32::try_from(added).unwrap_or(u32::MAX),
                documents_updated: u32::try_from(carried).unwrap_or(u32::MAX),
                documents_skipped: u32::try_from(deleted).unwrap_or(u32::MAX),
                duration_ms,
                outcome: telemetry_outcome,
            },
        ))
        .await;

    outcome.map(|_| ())
}

struct RunStats {
    added: usize,
    carried: usize,
    deleted: usize,
}

#[allow(clippy::too_many_lines)]
async fn run_inner(
    args: &Args,
    server_url: &str,
    auth_path: &Path,
    json: bool,
) -> Result<RunStats> {
    // 1. Read + validate manifest.
    let manifest_path = &args.manifest;
    let body = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read manifest at {}", manifest_path.display()))?;
    let manifest = Manifest::parse(&body).context("parse manifest")?;
    manifest.validate().context("validate manifest")?;

    let source_root = args.source_root.clone().unwrap_or_else(|| {
        manifest_path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });
    let missing = manifest.validate_files_exist(&source_root);
    if !missing.is_empty() {
        return Err(anyhow!(
            "manifest references {} missing file(s): first missing = {}",
            missing.len(),
            missing[0].display()
        ));
    }

    // 2. Walk + build plan.
    let walker = Walker::new(manifest, source_root);
    let walked_docs = walker.walk().context("walk source tree")?;

    let mut builder = PlanBuilder::new(
        &args.source_slug,
        SourceKind::DocsSite,
        &args.revision,
        PriorState::default(),
    );
    for doc in walked_docs {
        builder
            .add_walked_document(
                doc.rel_path.clone(),
                DocumentKind::Markdown,
                &doc.content,
                &doc.split,
            )
            .with_context(|| format!("plan add {}", doc.rel_path.display()))?;
    }
    let plan = builder.finalize();

    if args.dry_run {
        println!(
            "{}",
            format_dry_run(
                &args.source_slug,
                plan.stats.documents_added,
                plan.stats.chunks_emitted,
                json
            ),
        );
        return Ok(RunStats {
            added: plan.stats.documents_added,
            carried: 0,
            deleted: 0,
        });
    }

    // 3. Load admin bearer.
    let auth_file = AuthFile::read_optional(auth_path)
        .with_context(|| format!("read auth.toml at {}", auth_path.display()))?
        .ok_or_else(|| anyhow!("no admin token — run `mnm login --user-id <id>` first"))?;
    let admin = auth_file
        .admin
        .ok_or_else(|| anyhow!("auth.toml has no [admin] section — run `mnm login` first"))?;
    if admin.expires_at <= OffsetDateTime::now_utc() {
        return Err(anyhow!(
            "admin token expired at {}; run `mnm login` to refresh",
            admin.expires_at,
        ));
    }
    let token = admin.token;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build HTTP client")?;

    // 4. Start the run.
    let start: StartIngestRunResponse = post_json(
        &client,
        &format!(
            "{server_url}/v1/admin/sources/{slug}/ingest-runs",
            slug = url_encode(&args.source_slug),
        ),
        &token,
        &StartIngestRunRequest {
            ingest_cli_version: env!("CARGO_PKG_VERSION").to_owned(),
            embedding_model: args.embedding_model.clone(),
            note: args.note.clone(),
        },
    )
    .await
    .context("start ingest run")?;

    // 5. Upload documents (single batch for v1).
    let docs: Vec<DocumentUpload> = plan
        .new_documents
        .iter()
        .map(|d| DocumentUpload {
            path: d.path.display().to_string(),
            kind: d.kind,
            content_hash: d.content_hash.clone(),
            source_url: None,
            published_url: None,
            language: None,
            source_modified_at: None,
            frontmatter: d.frontmatter.clone(),
            provenance: d.provenance.clone(),
            char_count: i32::try_from(d.char_count).unwrap_or(i32::MAX),
            token_count: 0,
            chunks: d
                .chunks
                .iter()
                .map(|c| ChunkUpload {
                    chunk_index: i32::try_from(c.chunk_index).unwrap_or(i32::MAX),
                    total_chunks: i32::try_from(c.total_chunks).unwrap_or(i32::MAX),
                    content: c.content.clone(),
                    content_hash: c.content_hash.clone(),
                    heading_path: c.heading_path.clone(),
                    symbol_path: Vec::new(),
                    start_byte: i32::try_from(c.start_byte).unwrap_or(i32::MAX),
                    end_byte: i32::try_from(c.end_byte).unwrap_or(i32::MAX),
                    token_count: 0,
                })
                .collect(),
        })
        .collect();

    let upload_url = format!(
        "{server_url}/v1/admin/sources/{slug}/ingest-runs/{id}/documents",
        slug = url_encode(&args.source_slug),
        id = start.ingest_run_id,
    );
    let upload_result: anyhow::Result<UploadDocumentsResponse> =
        put_json(&client, &upload_url, &token, &UploadDocumentsRequest { documents: docs }).await;

    let upload = match upload_result {
        Ok(u) => u,
        Err(e) => {
            abort_run(&client, server_url, &args.source_slug, start.ingest_run_id, &token).await;
            return Err(e.context("upload documents"));
        }
    };

    // 6. Finalize.
    let finalize_url = format!(
        "{server_url}/v1/admin/sources/{slug}/ingest-runs/{id}/finalize",
        slug = url_encode(&args.source_slug),
        id = start.ingest_run_id,
    );
    let finalize: FinalizeResult = match post_empty(&client, &finalize_url, &token).await {
        Ok(r) => r,
        Err(e) => {
            abort_run(&client, server_url, &args.source_slug, start.ingest_run_id, &token).await;
            return Err(e.context("finalize ingest run"));
        }
    };

    let stats = RunStats {
        added: upload.accepted.saturating_sub(upload.carried),
        carried: upload.carried,
        deleted: 0,
    };
    println!(
        "{}",
        format_success(
            &args.source_slug,
            finalize.revision,
            finalize.demoted_revision,
            stats.added,
            stats.carried,
            json,
        )
    );
    Ok(stats)
}

async fn abort_run(
    client: &reqwest::Client,
    server_url: &str,
    slug: &str,
    run_id: Uuid,
    token: &str,
) {
    let url = format!(
        "{server_url}/v1/admin/sources/{slug}/ingest-runs/{run_id}/abort",
        slug = url_encode(slug),
    );
    let _ = client.post(&url).bearer_auth(token).send().await;
}

fn url_encode(s: &str) -> String {
    // Slugs are lowercase alnum + hyphen by convention; nothing to encode in
    // practice. Replace any forward slash defensively.
    s.replace('/', "%2F")
}

async fn post_json<I: Serialize + Sync, O: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &I,
) -> Result<O> {
    let resp = client
        .post(url)
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    decode_response(resp, url).await
}

async fn post_empty<O: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<O> {
    let resp = client
        .post(url)
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    decode_response(resp, url).await
}

async fn put_json<I: Serialize + Sync, O: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &I,
) -> Result<O> {
    let resp = client
        .put(url)
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .with_context(|| format!("PUT {url}"))?;
    decode_response(resp, url).await
}

async fn decode_response<O: for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
    url: &str,
) -> Result<O> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("{status} from {url}: {}", redact_token_like(&body)));
    }
    resp.json::<O>()
        .await
        .with_context(|| format!("parse response from {url}"))
}

/// Strip long base64-y fragments from error bodies so a bearer that ends up in
/// an error envelope is not echoed to logs (FR-019).
fn redact_token_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if word.len() > 40
            && word
                .chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '=')
        {
            out.push_str("[redacted]");
        } else {
            out.push_str(word);
        }
        out.push(' ');
    }
    out.trim_end().to_owned()
}

#[derive(Debug, Serialize)]
struct StartIngestRunRequest {
    ingest_cli_version: String,
    embedding_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StartIngestRunResponse {
    ingest_run_id: Uuid,
    #[allow(dead_code)]
    source_version_id: Uuid,
    #[allow(dead_code)]
    source_version_revision: i32,
}

#[derive(Debug, Serialize)]
struct UploadDocumentsRequest {
    documents: Vec<DocumentUpload>,
}

#[derive(Debug, Serialize)]
struct DocumentUpload {
    path: String,
    kind: DocumentKind,
    content_hash: String,
    source_url: Option<String>,
    published_url: Option<String>,
    language: Option<String>,
    source_modified_at: Option<OffsetDateTime>,
    frontmatter: Option<serde_json::Value>,
    provenance: Provenance,
    char_count: i32,
    token_count: i32,
    chunks: Vec<ChunkUpload>,
}

#[derive(Debug, Serialize)]
struct ChunkUpload {
    chunk_index: i32,
    total_chunks: i32,
    content: String,
    content_hash: String,
    heading_path: Vec<String>,
    symbol_path: Vec<String>,
    start_byte: i32,
    end_byte: i32,
    token_count: i32,
}

#[derive(Debug, Deserialize)]
struct UploadDocumentsResponse {
    accepted: usize,
    carried: usize,
    #[allow(dead_code)]
    #[serde(default)]
    conflicts: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct FinalizeResult {
    #[allow(dead_code)]
    source_version_id: Uuid,
    revision: i32,
    #[allow(dead_code)]
    is_active: bool,
    #[serde(default)]
    demoted_revision: Option<i32>,
}

#[derive(Debug, Serialize)]
struct DryRunOutput<'a> {
    action: &'a str,
    source_slug: &'a str,
    documents: usize,
    chunks: usize,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct SuccessOutput<'a> {
    action: &'a str,
    source_slug: &'a str,
    revision: i32,
    demoted_revision: Option<i32>,
    documents_added: usize,
    documents_carried: usize,
}

fn format_dry_run(source_slug: &str, documents: usize, chunks: usize, json: bool) -> String {
    if json {
        let body = DryRunOutput {
            action: "ingest",
            source_slug,
            documents,
            chunks,
            dry_run: true,
        };
        serde_json::to_string(&body).unwrap_or_default()
    } else {
        format!(
            "ingest dry-run for `{source_slug}`: would post {documents} documents / {chunks} chunks",
        )
    }
}

fn format_success(
    source_slug: &str,
    revision: i32,
    demoted_revision: Option<i32>,
    added: usize,
    carried: usize,
    json: bool,
) -> String {
    if json {
        let body = SuccessOutput {
            action: "ingest",
            source_slug,
            revision,
            demoted_revision,
            documents_added: added,
            documents_carried: carried,
        };
        serde_json::to_string(&body).unwrap_or_default()
    } else if let Some(prev) = demoted_revision {
        format!(
            "ingested `{source_slug}` rev {revision} (was {prev}); +{added} new, {carried} carried"
        )
    } else {
        format!("ingested `{source_slug}` rev {revision} (first version); +{added} new")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_human_output() {
        let s = format_dry_run("docs", 3, 7, false);
        assert!(s.contains("docs"));
        assert!(s.contains("3 documents"));
        assert!(s.contains("7 chunks"));
    }

    #[test]
    fn dry_run_json_output() {
        let s = format_dry_run("docs", 3, 7, true);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["action"], "ingest");
        assert_eq!(v["documents"], 3);
        assert_eq!(v["chunks"], 7);
        assert_eq!(v["dry_run"], true);
    }

    #[test]
    fn success_human_output_first_version() {
        let s = format_success("docs", 1, None, 5, 0, false);
        assert!(s.contains("first version"));
        assert!(s.contains("+5 new"));
    }

    #[test]
    fn success_human_output_with_demote() {
        let s = format_success("docs", 2, Some(1), 3, 4, false);
        assert!(s.contains("rev 2"));
        assert!(s.contains("was 1"));
        assert!(s.contains("+3 new"));
        assert!(s.contains("4 carried"));
    }

    #[test]
    fn redacts_long_alnum_blobs() {
        let body = "verify failed token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let redacted = redact_token_like(body);
        assert!(!redacted.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(redacted.contains("[redacted]"));
    }
}

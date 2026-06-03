//! `mnm doctor` — diagnostic report.
//!
//! Reports local state (CLI version, config file, telemetry resolver) and,
//! when an admin bearer is present in `auth.toml`, fetches the corpus's
//! ingest status from `GET /v1/admin/ingest/status` and renders a
//! per-source summary so operators can spot incomplete embed work without
//! ad-hoc SQL.

use anyhow::Result;
use clap::Args as ClapArgs;
use mn_core::auth_file::AuthFile;
use mn_telemetry::optout;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::commands::version::VersionInfo;

/// Arguments for `mnm doctor`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Emit a single JSON object instead of human-formatted output.
    #[arg(long)]
    pub json: bool,
}

/// Diagnostic report shape.
#[derive(Debug, Serialize)]
struct Report {
    cli: VersionInfo,
    config_file: Option<String>,
    admin_visibility: bool,
    telemetry: TelemetryReport,
    /// Corpus ingest summary. `None` when no admin bearer is available or
    /// the server round-trip failed (the human renderer prints a hint
    /// pointing the user at `mnm login`).
    #[serde(skip_serializing_if = "Option::is_none")]
    corpus: Option<CorpusReport>,
}

/// Telemetry section of the doctor report (FR-073 + Phase 8b).
#[derive(Debug, Serialize)]
struct TelemetryReport {
    /// `true` when the three-mechanism resolver currently allows emission.
    enabled: bool,
    /// Resolved sink URL, derived from `[server].url`.
    sink_url: String,
    /// Resolved persistent-marker path (mechanism #3). `None` when no
    /// `HOME` / `XDG_CONFIG_HOME` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    marker_path: Option<String>,
    /// Whether the marker file exists at `marker_path`.
    marker_present: bool,
    /// Per-mechanism disable status (env / config / runtime).
    disabled_by: DisabledBy,
}

/// Which of the three opt-out mechanisms is currently active.
#[derive(Debug, Serialize)]
struct DisabledBy {
    env: bool,
    config: bool,
    runtime: bool,
}

/// Corpus-side ingest summary fetched from `/v1/admin/ingest/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusReport {
    /// Active embedding model wire id (`name@revision`).
    pub active_embedding_model: String,
    /// One row per registered source.
    pub sources: Vec<CorpusSourceStatus>,
}

/// One row in [`CorpusReport::sources`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusSourceStatus {
    /// Source slug.
    pub slug: String,
    /// Active source_version revision (or `None` when nothing's active).
    pub active_revision: Option<i32>,
    /// Total chunks under the active version.
    pub total_chunks: i64,
    /// Chunks ready for search.
    pub ready_chunks: i64,
    /// Chunks missing an embedding (no vector — not ready for search).
    pub embed_failed_chunks: i64,
}

/// Run the `doctor` subcommand.
///
/// # Errors
///
/// Returns an error if config discovery fails. Corpus-fetch failures are
/// surfaced in the rendered report (and do not fail the command).
pub async fn run(args: Args, json: bool) -> Result<()> {
    let emit_json = json || args.json;

    let env = mn_core::config::StdEnv;
    let (cfg, path) = mn_core::config::Config::discover(None, &env)?;
    let marker = mn_core::paths::telemetry_marker_path(&env);
    optout::load_persistent_marker(marker.as_deref());
    let env_disabled = std::env::var(optout::DISABLE_ENV_VAR)
        .ok()
        .is_some_and(|v| {
            matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
        });
    let marker_present = marker.as_deref().is_some_and(std::path::Path::exists);
    let telemetry = TelemetryReport {
        enabled: optout::is_enabled(&optout::StdEnv, cfg.telemetry.enabled),
        sink_url: format!("{}/v1/telemetry/events", cfg.server.url.trim_end_matches('/')),
        marker_path: marker.as_ref().map(|p| p.display().to_string()),
        marker_present,
        disabled_by: DisabledBy {
            env: env_disabled,
            config: !cfg.telemetry.enabled,
            runtime: optout::runtime_disabled(),
        },
    };

    let admin_token = resolve_admin_token(env);
    let server_url = cfg.server.url.trim_end_matches('/').to_owned();
    let corpus = if let Some(token) = admin_token.as_deref() {
        match fetch_corpus_report(&server_url, token).await {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(error = %e, "corpus status fetch failed");
                None
            }
        }
    } else {
        None
    };

    let report = Report {
        cli: VersionInfo::current(),
        config_file: path.map(|p| p.display().to_string()),
        admin_visibility: std::env::var("MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS")
            .ok()
            .is_some_and(|v| !matches!(v.as_str(), "0" | "false" | "no")),
        telemetry,
        corpus,
    };

    if emit_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report, admin_token.is_some());
    }
    Ok(())
}

fn print_human(report: &Report, had_admin_token: bool) {
    println!("mnm doctor");
    println!("  version:           {} ({})", report.cli.version, report.cli.host);
    println!(
        "  config file:       {}",
        report
            .config_file
            .as_deref()
            .unwrap_or("(none — using defaults)")
    );
    println!("  admin visibility:  {}", if report.admin_visibility { "on" } else { "off" },);
    println!(
        "  telemetry:         {} (sink: {})",
        if report.telemetry.enabled {
            "on"
        } else {
            "off"
        },
        report.telemetry.sink_url,
    );
    if let Some(p) = report.telemetry.marker_path.as_deref() {
        println!(
            "    marker file:     {p} ({})",
            if report.telemetry.marker_present {
                "present"
            } else {
                "absent"
            },
        );
    }
    if report.telemetry.disabled_by.env {
        println!("    - disabled by env ({})", optout::DISABLE_ENV_VAR);
    }
    if report.telemetry.disabled_by.config {
        println!("    - disabled by config.toml (telemetry.enabled = false)");
    }
    if report.telemetry.disabled_by.runtime {
        println!("    - disabled by runtime toggle (`mnm telemetry disable`)");
    }

    if let Some(c) = &report.corpus {
        print_corpus(c);
    } else {
        println!();
        println!("  corpus:            (not queried)");
        if had_admin_token {
            println!("    server unavailable or returned an error — see logs");
        } else {
            println!("    run `mnm login --user-id <id>` to enable the corpus ingest section");
        }
    }
}

fn print_corpus(c: &CorpusReport) {
    println!();
    println!("  corpus ingest:");
    println!("    active model:    {}", c.active_embedding_model);
    if c.sources.is_empty() {
        println!("    sources:         (none registered)");
        return;
    }
    let total: i64 = c.sources.iter().map(|s| s.total_chunks).sum();
    let ready: i64 = c.sources.iter().map(|s| s.ready_chunks).sum();
    let pending: i64 = c.sources.iter().map(|s| s.embed_failed_chunks).sum();
    println!(
        "    sources:         {} ({total} chunks; {ready} ready; {pending} pending embed)",
        c.sources.len(),
    );
    for s in &c.sources {
        let rev = s
            .active_revision
            .map_or_else(|| "(no active version)".to_owned(), |r| format!("rev {r}"));
        println!(
            "      {slug:30} {rev:18}  {ready:>5}/{total:<5} ready  {pending:>5} pending",
            slug = truncate(&s.slug, 30),
            rev = rev,
            ready = s.ready_chunks,
            total = s.total_chunks,
            pending = s.embed_failed_chunks,
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

fn resolve_admin_token(env: mn_core::config::StdEnv) -> Option<String> {
    let path = mn_core::paths::auth_file_path(&env)?;
    let file = AuthFile::read_optional(&path).ok().flatten()?;
    file.active_admin_token(OffsetDateTime::now_utc())
        .map(str::to_owned)
}

/// Fetch the corpus ingest summary. Exposed for integration tests.
///
/// # Errors
///
/// Returns `anyhow::Error` on transport, non-2xx, or decode failure.
pub async fn fetch_corpus_report(server_url: &str, token: &str) -> Result<CorpusReport> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client
        .get(format!("{server_url}/v1/admin/ingest/status"))
        .bearer_auth(token)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("{status} from /v1/admin/ingest/status: {body}"));
    }
    Ok(resp.json::<CorpusReport>().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_no_op_under_max() {
        assert_eq!(truncate("docs", 10), "docs");
    }

    #[test]
    fn truncate_appends_ellipsis_over_max() {
        let s = truncate("a-very-long-source-slug", 10);
        assert_eq!(s.chars().count(), 10);
        assert!(s.ends_with('…'));
    }
}

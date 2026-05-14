//! `mnm versions` — source-version inspection and admin promote / retire.
//!
//! Public sub-subcommands (anonymous reads):
//!
//! - `list <slug>` — `GET /v1/sources/:slug/versions`.
//! - `show <slug> <revision>` — `GET /v1/sources/:slug/versions/:revision`.
//!
//! Admin sub-subcommands (hidden from `--help` by default per D23; still
//! execute when called by name). All three resolve an admin bearer from
//! `auth.toml`; if no token is available the command exits cleanly with a
//! "run `mnm login` first" error.
//!
//! - `promote <slug> --revision N` — `POST /v1/admin/sources/:slug/versions/:N/promote`.
//! - `rollback <slug>` — convenience: looks up the most recent prior
//!   active version via `list`, then promotes it (FR-072).
//! - `retire <slug> --revision N` — `POST /v1/admin/sources/:slug/versions/:N/retire`.

use anyhow::{anyhow, Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mn_core::auth_file::AuthFile;
use time::OffsetDateTime;

/// `mnm versions <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: VersionsCmd,
}

/// `versions` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum VersionsCmd {
    /// List all source_versions for a slug (anonymous read).
    List {
        /// Source slug.
        slug: String,
    },
    /// Show one source_version by revision (anonymous read).
    Show {
        /// Source slug.
        slug: String,
        /// Source-version revision (integer).
        revision: i32,
    },
    /// Promote a historical version back to active (admin).
    #[command(hide = true)]
    Promote(PromoteArgs),
    /// Roll back to the most recent prior active version (admin
    /// convenience that calls `list` then `promote`).
    #[command(hide = true)]
    Rollback(RollbackArgs),
    /// Retire a single historical version (admin).
    #[command(hide = true)]
    Retire(RetireArgs),
}

/// `mnm versions promote <slug> --revision N` arguments.
#[derive(Debug, ClapArgs)]
pub struct PromoteArgs {
    /// Source slug.
    pub slug: String,
    /// Revision to promote. Must currently be in `inactive` state.
    #[arg(long)]
    pub revision: i32,
}

/// `mnm versions rollback <slug>` arguments.
#[derive(Debug, ClapArgs)]
pub struct RollbackArgs {
    /// Source slug.
    pub slug: String,
}

/// `mnm versions retire <slug> --revision N` arguments.
#[derive(Debug, ClapArgs)]
pub struct RetireArgs {
    /// Source slug.
    pub slug: String,
    /// Revision to retire. The active revision is rejected — promote
    /// another version first.
    #[arg(long)]
    pub revision: i32,
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error on network failure, non-2xx responses, or — for admin
/// subcommands — when no admin bearer can be resolved from `auth.toml`.
pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;

    match args.cmd {
        VersionsCmd::List { slug } => {
            let value = list_request(&client, &server_url, &slug).await?;
            emit_list(&value, json);
        }
        VersionsCmd::Show { slug, revision } => {
            let value = show_request(&client, &server_url, &slug, revision).await?;
            emit_show(&value, json);
        }
        VersionsCmd::Promote(p) => {
            let token = require_admin_token()?;
            let value = promote_request(&client, &server_url, &p.slug, p.revision, &token).await?;
            emit_promote(&value, json);
        }
        VersionsCmd::Rollback(r) => {
            let token = require_admin_token()?;
            let revision = resolve_rollback_target(&client, &server_url, &r.slug).await?;
            let value = promote_request(&client, &server_url, &r.slug, revision, &token).await?;
            emit_promote(&value, json);
        }
        VersionsCmd::Retire(r) => {
            let token = require_admin_token()?;
            let value = retire_request(&client, &server_url, &r.slug, r.revision, &token).await?;
            emit_show(&value, json);
        }
    }
    Ok(())
}

// ---- HTTP helpers (pub for integration tests) ----

/// `GET /v1/sources/:slug/versions`.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn list_request(
    client: &reqwest::Client,
    server_url: &str,
    slug: &str,
) -> Result<serde_json::Value> {
    let resp = client
        .get(format!("{server_url}/v1/sources/{slug}/versions"))
        .send()
        .await
        .with_context(|| format!("GET /v1/sources/{slug}/versions"))?;
    decode_response(resp, "list versions").await
}

/// `GET /v1/sources/:slug/versions/:revision`.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn show_request(
    client: &reqwest::Client,
    server_url: &str,
    slug: &str,
    revision: i32,
) -> Result<serde_json::Value> {
    let resp = client
        .get(format!("{server_url}/v1/sources/{slug}/versions/{revision}"))
        .send()
        .await
        .with_context(|| format!("GET /v1/sources/{slug}/versions/{revision}"))?;
    decode_response(resp, "show version").await
}

/// `POST /v1/admin/sources/:slug/versions/:revision/promote`.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn promote_request(
    client: &reqwest::Client,
    server_url: &str,
    slug: &str,
    revision: i32,
    bearer: &str,
) -> Result<serde_json::Value> {
    let resp = client
        .post(format!("{server_url}/v1/admin/sources/{slug}/versions/{revision}/promote"))
        .bearer_auth(bearer)
        .send()
        .await
        .with_context(|| format!("POST .../{slug}/versions/{revision}/promote"))?;
    decode_response(resp, "promote version").await
}

/// `POST /v1/admin/sources/:slug/versions/:revision/retire`.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn retire_request(
    client: &reqwest::Client,
    server_url: &str,
    slug: &str,
    revision: i32,
    bearer: &str,
) -> Result<serde_json::Value> {
    let resp = client
        .post(format!("{server_url}/v1/admin/sources/{slug}/versions/{revision}/retire"))
        .bearer_auth(bearer)
        .send()
        .await
        .with_context(|| format!("POST .../{slug}/versions/{revision}/retire"))?;
    decode_response(resp, "retire version").await
}

/// Resolve the rollback target — the most recent revision that is currently
/// in `inactive` state. Exposed for tests so the lookup can be driven
/// against a wiremock server.
///
/// # Errors
///
/// Returns an error on transport failure or when no prior inactive version
/// exists for `slug` (EC-71).
pub async fn resolve_rollback_target(
    client: &reqwest::Client,
    server_url: &str,
    slug: &str,
) -> Result<i32> {
    let value = list_request(client, server_url, slug).await?;
    let rows = value
        .as_array()
        .ok_or_else(|| anyhow!("unexpected /versions response shape: {value}"))?;
    let mut inactive: Vec<i32> = rows
        .iter()
        .filter(|r| r["status"] == "inactive")
        .filter_map(|r| r["revision"].as_i64().and_then(|v| i32::try_from(v).ok()))
        .collect();
    inactive.sort_unstable();
    inactive.last().copied().ok_or_else(|| {
        anyhow!(
            "no prior version exists to roll back to; `{slug}` has only one active source_version"
        )
    })
}

// ---- auth ----

fn require_admin_token() -> Result<String> {
    require_admin_token_from(&mn_core::config::StdEnv)
}

/// `require_admin_token` over an arbitrary [`mn_core::config::ConfigEnv`].
///
/// # Errors
///
/// Returns an error when no admin token is present at the resolved
/// `auth.toml` path, or when the path itself is undetermined.
pub fn require_admin_token_from(env: &impl mn_core::config::ConfigEnv) -> Result<String> {
    let path = mn_core::paths::auth_file_path(env).ok_or_else(|| {
        anyhow!("auth.toml location is undetermined — set $HOME or $XDG_CONFIG_HOME")
    })?;
    let file = AuthFile::read_optional(&path)
        .with_context(|| format!("read {}", path.display()))?
        .ok_or_else(|| {
            anyhow!("no admin bearer in {}. Run `mnm login --user-id <id>` first.", path.display())
        })?;
    file.active_admin_token(OffsetDateTime::now_utc())
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!(
                "no current admin bearer in {}. Run `mnm login --user-id <id>` first.",
                path.display()
            )
        })
}

async fn decode_response(resp: reqwest::Response, op: &str) -> Result<serde_json::Value> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("{status} from {op}: {body}"));
    }
    serde_json::from_str(&body).with_context(|| format!("parse {op} response body"))
}

// ---- emit ----

fn emit_list(v: &serde_json::Value, json: bool) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(v) {
            println!("{s}");
        }
        return;
    }
    let Some(rows) = v.as_array() else {
        println!("(unexpected response shape)");
        return;
    };
    for row in rows {
        let rev = row["revision"].as_i64().unwrap_or(-1);
        let status = row["status"].as_str().unwrap_or("?");
        let active = if row["is_active"].as_bool().unwrap_or(false) {
            " (active)"
        } else {
            ""
        };
        println!("  rev {rev:<5} {status:<10}{active}");
    }
}

fn emit_show(v: &serde_json::Value, json: bool) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(v) {
            println!("{s}");
        }
        return;
    }
    let rev = v["revision"].as_i64().unwrap_or(-1);
    let status = v["status"].as_str().unwrap_or("?");
    let active = v["is_active"].as_bool().unwrap_or(false);
    let retired = v["retired_at"].is_null();
    println!("revision:       {rev}");
    println!("status:         {status}");
    println!("is_active:      {active}");
    println!("retired:        {}", if retired { "no" } else { "yes" });
}

fn emit_promote(v: &serde_json::Value, json: bool) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(v) {
            println!("{s}");
        }
        return;
    }
    let promoted = v["promoted_revision"].as_i64().unwrap_or(-1);
    if v["demoted_revision"].is_null() {
        println!("promoted revision {promoted}");
    } else {
        let demoted = v["demoted_revision"].as_i64().unwrap_or(-1);
        println!("promoted revision {promoted}; demoted revision {demoted}");
    }
}

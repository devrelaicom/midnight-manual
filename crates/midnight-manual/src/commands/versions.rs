//! `mnm versions` — source-version inspection and admin promote / retire.
//!
//! Public sub-subcommands (anonymous reads):
//!
//! - `list <slug>` — `GET /v1/sources/:slug/versions`.
//! - `show <slug> <revision>` — `GET /v1/sources/:slug/versions/:revision`.
//!
//! Admin sub-subcommands are **always** hidden from `--help` — each is
//! statically `#[command(hide = true)]`. `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1`
//! does NOT reveal them: that toggle only un-hides the top-level admin commands
//! listed in [`crate::cli`]'s `ADMIN_SUBCOMMANDS` (and `versions` is not one of
//! them — it stays visible for its public `list` / `show` reads), and the hide
//! gate never recurses into nested variants. They still execute when called by
//! name. All three resolve an admin bearer from `auth.toml`; if no token is
//! available the command exits cleanly with a "run `mnm login` first" error.
//!
//! - `promote <slug> --revision N` — `POST /v1/admin/sources/:slug/versions/:N/promote`.
//! - `rollback <slug>` — convenience: looks up the most recent prior
//!   active version via `list`, then promotes it (FR-072).
//! - `retire <slug> --revision N` — `POST /v1/admin/sources/:slug/versions/:N/retire`.

use anyhow::{anyhow, Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mnm_core::auth_file::AuthFile;
use mnm_core::types::SourceVersionStatus;
use serde::Deserialize;
use time::OffsetDateTime;

/// Deserialize-only mirror of the server's `source_version` row
/// (`mnm_core::types::SourceVersion`, serialized verbatim by the `Json(row)`
/// paths in [`crate::routes::versions`] and [`crate::routes::admin_versions`]'s
/// retire readback).
///
/// The `list` / `show` human paths deserialize into this instead of reading
/// hard-coded `serde_json::Value` string keys with `-1`/`"?"` fallbacks, so a
/// server-side rename of `revision`/`status`/`is_active` — or a rename of a
/// [`SourceVersionStatus`] snake_case variant, since `status` reuses the
/// canonical enum — fails the deserialize test at build time. The `--json`
/// paths still emit the verbatim server envelope and never touch this struct.
///
/// Only the fields the human rows print are modelled; `serde` ignores the
/// remaining `SourceVersion` columns the rows do not show.
#[derive(Debug, Deserialize)]
pub(crate) struct SourceVersionRow {
    /// Monotonic per-source revision number.
    pub revision: i32,
    /// Lifecycle state. Reuses the canonical enum so a variant rename is caught.
    pub status: SourceVersionStatus,
    /// True only for the single active row per source.
    pub is_active: bool,
    /// Retirement timestamp as an RFC 3339 string; `None` when not retired.
    /// The server serializes the column as JSON `null` when unset (mapped to
    /// `None`), so [`SourceVersionRow::is_retired`] equals the `retired_at`
    /// null-check the string-key path used.
    #[serde(default)]
    pub retired_at: Option<String>,
}

impl SourceVersionRow {
    /// The canonical snake_case wire string for this row's status — the same
    /// token the string-key path printed. Delegates to
    /// [`SourceVersionStatus::as_str`] so the mapping lives in `mnm-core` as the
    /// single source of truth (test-pinned to the serde `rename_all` output) and
    /// can never drift here.
    const fn status_wire(&self) -> &'static str {
        self.status.as_str()
    }

    /// Whether this version has been marked retired.
    const fn is_retired(&self) -> bool {
        self.retired_at.is_some()
    }
}

/// Deserialize-only mirror of the server's promote/rollback response
/// (`crate::routes::admin_versions::PromoteResult`). `demoted_revision` is
/// genuinely optional (no version was demoted), so it is modelled as
/// `Option<i32>` rather than the `-1` sentinel the string-key path invented.
#[derive(Debug, Deserialize)]
pub(crate) struct PromoteResultRow {
    /// Revision that was just promoted to `active`.
    pub promoted_revision: i32,
    /// Revision demoted from `active` to `inactive`, if any.
    #[serde(default)]
    pub demoted_revision: Option<i32>,
}

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
    require_admin_token_from(&mnm_core::config::StdEnv)
}

/// `require_admin_token` over an arbitrary [`mnm_core::config::ConfigEnv`].
///
/// # Errors
///
/// Returns an error when no admin token is present at the resolved
/// `auth.toml` path, or when the path itself is undetermined.
pub fn require_admin_token_from(env: &impl mnm_core::config::ConfigEnv) -> Result<String> {
    let path = mnm_core::paths::auth_file_path(env).ok_or_else(|| {
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
        let Ok(version) = serde_json::from_value::<SourceVersionRow>(row.clone()) else {
            println!("  (unexpected version row shape)");
            continue;
        };
        let active = if version.is_active { " (active)" } else { "" };
        println!("  rev {:<5} {:<10}{active}", version.revision, version.status_wire());
    }
}

fn emit_show(v: &serde_json::Value, json: bool) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(v) {
            println!("{s}");
        }
        return;
    }
    let Ok(version) = serde_json::from_value::<SourceVersionRow>(v.clone()) else {
        println!("(unexpected version row shape)");
        return;
    };
    println!("revision:       {}", version.revision);
    println!("status:         {}", version.status_wire());
    println!("is_active:      {}", version.is_active);
    println!("retired:        {}", if version.is_retired() { "yes" } else { "no" });
}

fn emit_promote(v: &serde_json::Value, json: bool) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(v) {
            println!("{s}");
        }
        return;
    }
    let Ok(result) = serde_json::from_value::<PromoteResultRow>(v.clone()) else {
        println!("(unexpected promote response shape)");
        return;
    };
    match result.demoted_revision {
        Some(demoted) => {
            println!("promoted revision {}; demoted revision {demoted}", result.promoted_revision);
        }
        None => println!("promoted revision {}", result.promoted_revision),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative server `Json(SourceVersion)` row decodes into the
    /// list/show mirror with the expected typed fields. Renaming
    /// `revision`/`status`/`is_active` — or a `SourceVersionStatus` variant's
    /// snake_case token — fails this test instead of printing `-1`/`?`.
    #[test]
    fn source_version_row_deserializes_from_server_shape() {
        let row: SourceVersionRow = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "source_id": "00000000-0000-0000-0000-000000000002",
            "revision": 3,
            "status": "active",
            "is_active": true,
            "ingested_at": "2026-01-01T00:00:00Z",
            "ingest_cli_version": "0.1.0",
            "embedding_model_id": "00000000-0000-0000-0000-000000000003",
            "content_hash": "deadbeef",
            "notes": null,
            "retired_at": null
        }))
        .expect("server source_version row must decode into SourceVersionRow");
        assert_eq!(row.revision, 3);
        assert_eq!(row.status, SourceVersionStatus::Active);
        assert_eq!(row.status_wire(), "active");
        assert!(row.is_active);
        // Explicit JSON `null` retired_at → None → not retired.
        assert!(!row.is_retired());
    }

    /// An inactive, retired row decodes and `is_retired()` flips to `true`.
    #[test]
    fn source_version_row_decodes_retired() {
        let row: SourceVersionRow = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "source_id": "00000000-0000-0000-0000-000000000002",
            "revision": 1,
            "status": "retired",
            "is_active": false,
            "ingested_at": "2026-01-01T00:00:00Z",
            "ingest_cli_version": "0.1.0",
            "embedding_model_id": "00000000-0000-0000-0000-000000000003",
            "content_hash": "deadbeef",
            "notes": null,
            "retired_at": "2026-06-01T00:00:00Z"
        }))
        .expect("retired source_version row must decode");
        assert_eq!(row.status, SourceVersionStatus::Retired);
        assert!(!row.is_active);
        assert!(row.is_retired());
    }

    /// The promote/rollback `PromoteResult` envelope decodes, with
    /// `demoted_revision` modelled as a genuine `Option` (no `-1` sentinel).
    #[test]
    fn promote_result_row_deserializes_with_and_without_demotion() {
        let with_demotion: PromoteResultRow = serde_json::from_value(serde_json::json!({
            "promoted_revision": 4,
            "demoted_revision": 3
        }))
        .expect("promote result with demotion must decode");
        assert_eq!(with_demotion.promoted_revision, 4);
        assert_eq!(with_demotion.demoted_revision, Some(3));

        let no_demotion: PromoteResultRow = serde_json::from_value(serde_json::json!({
            "promoted_revision": 4,
            "demoted_revision": null
        }))
        .expect("promote result without demotion must decode");
        assert_eq!(no_demotion.promoted_revision, 4);
        assert!(no_demotion.demoted_revision.is_none());
    }
}

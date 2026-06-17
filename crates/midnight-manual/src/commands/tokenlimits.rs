//! `mnm tokenlimits` — admin CRUD for per-CIDR / per-user embedding token-limit
//! overrides.
//!
//! Mirrors `mnm ratelimits` (same admin-only gating, bearer resolution, and
//! confirm-on-remove flow) but manages `token_limit_override` rows, which key on
//! a subject (a CIDR block for anonymous IPs, or a user id) and carry hourly +
//! daily token ceilings instead of a requests-per-second rate.
//!
//! - `add (--cidr <CIDR> | --user <ID>) --hourly <N> --daily <N> --ttl <DUR> [--note <S>]` — `POST`.
//! - `list` — `GET` active overrides.
//! - `extend <id> --ttl <DUR>` — `PATCH` a new expiry.
//! - `remove <id> [--yes]` — `DELETE` (confirms unless `--yes`).

use anyhow::{anyhow, Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use serde_json::json;

use crate::commands::ratelimits::{decode_response, expiry_from_ttl, require_admin_token_from};

/// `mnm tokenlimits <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: TokenlimitsCmd,
}

/// `tokenlimits` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum TokenlimitsCmd {
    /// Create a new per-CIDR or per-user override.
    Add(AddArgs),
    /// List overrides still in effect.
    List,
    /// Extend an existing override's TTL.
    Extend(ExtendArgs),
    /// Remove an override (confirms unless `--yes`).
    Remove(RemoveArgs),
}

/// `mnm tokenlimits add` arguments. Exactly one of `--cidr` / `--user`.
#[derive(Debug, ClapArgs)]
pub struct AddArgs {
    /// CIDR block (`addr/prefix`) for an anonymous-IP override. Mutually
    /// exclusive with `--user`.
    #[arg(long, conflicts_with = "user")]
    pub cidr: Option<String>,
    /// User id for an authenticated-subject override. Mutually exclusive with
    /// `--cidr`.
    #[arg(long, conflicts_with = "cidr")]
    pub user: Option<String>,
    /// Rolling-hour token ceiling (>= 0).
    #[arg(long)]
    pub hourly: i64,
    /// Rolling-day token ceiling (>= 0).
    #[arg(long)]
    pub daily: i64,
    /// Time-to-live from now, e.g. `48h`, `30m`, `7d`, `90s`.
    #[arg(long)]
    pub ttl: String,
    /// Optional operator note.
    #[arg(long)]
    pub note: Option<String>,
}

/// `mnm tokenlimits extend <id> --ttl <DUR>` arguments.
#[derive(Debug, ClapArgs)]
pub struct ExtendArgs {
    /// Override id (UUID).
    pub id: String,
    /// New time-to-live from now, e.g. `24h`.
    #[arg(long)]
    pub ttl: String,
}

/// `mnm tokenlimits remove <id>` arguments.
#[derive(Debug, ClapArgs)]
pub struct RemoveArgs {
    /// Override id (UUID) to remove.
    pub id: String,
    /// Skip the interactive confirmation.
    #[arg(long)]
    pub yes: bool,
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error on network failure, non-2xx responses, argument-parse
/// failures, or when no admin bearer can be resolved from `auth.toml`.
pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;

    match args.cmd {
        TokenlimitsCmd::Add(a) => {
            let (subject_kind, subject) = resolve_subject(&a)?;
            let expires_at = expiry_from_ttl(&a.ttl)?;
            let token = require_admin_token_from(&mnm_core::config::StdEnv)?;
            let value = add_request(
                &client,
                &server_url,
                subject_kind,
                &subject,
                a.hourly,
                a.daily,
                &expires_at,
                a.note.as_deref(),
                &token,
            )
            .await?;
            emit_one(&value, json);
        }
        TokenlimitsCmd::List => {
            let token = require_admin_token_from(&mnm_core::config::StdEnv)?;
            let value = list_request(&client, &server_url, &token).await?;
            emit_list(&value, json);
        }
        TokenlimitsCmd::Extend(e) => {
            let expires_at = expiry_from_ttl(&e.ttl)?;
            let token = require_admin_token_from(&mnm_core::config::StdEnv)?;
            let value = extend_request(&client, &server_url, &e.id, &expires_at, &token).await?;
            emit_one(&value, json);
        }
        TokenlimitsCmd::Remove(r) => {
            confirm_remove(&r)?;
            let token = require_admin_token_from(&mnm_core::config::StdEnv)?;
            let value = remove_request(&client, &server_url, &r.id, &token).await?;
            emit_one(&value, json);
        }
    }
    Ok(())
}

/// Resolve the `(subject_kind, subject)` pair from the mutually-exclusive
/// `--cidr` / `--user` flags. Clap's `conflicts_with` rejects "both"; this
/// rejects "neither".
fn resolve_subject(a: &AddArgs) -> Result<(&'static str, String)> {
    match (a.cidr.as_deref(), a.user.as_deref()) {
        (Some(c), None) => Ok(("cidr", c.to_owned())),
        (None, Some(u)) => Ok(("user", u.to_owned())),
        (None, None) => Err(anyhow!("specify exactly one of --cidr or --user")),
        (Some(_), Some(_)) => Err(anyhow!("--cidr and --user are mutually exclusive")),
    }
}

// ---- HTTP helpers (pub for integration tests) ----

/// `POST /v1/admin/tokenlimits`.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
#[allow(clippy::too_many_arguments)]
pub async fn add_request(
    client: &reqwest::Client,
    server_url: &str,
    subject_kind: &str,
    subject: &str,
    hourly: i64,
    daily: i64,
    expires_at: &str,
    note: Option<&str>,
    bearer: &str,
) -> Result<serde_json::Value> {
    let mut body = serde_json::Map::new();
    body.insert("subject_kind".to_owned(), json!(subject_kind));
    body.insert("subject".to_owned(), json!(subject));
    body.insert("hourly".to_owned(), json!(hourly));
    body.insert("daily".to_owned(), json!(daily));
    body.insert("expires_at".to_owned(), json!(expires_at));
    if let Some(n) = note {
        body.insert("note".to_owned(), json!(n));
    }
    let resp = client
        .post(format!("{server_url}/v1/admin/tokenlimits"))
        .bearer_auth(bearer)
        .json(&body)
        .send()
        .await
        .context("POST /v1/admin/tokenlimits")?;
    decode_response(resp, "add token limit").await
}

/// `GET /v1/admin/tokenlimits`.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn list_request(
    client: &reqwest::Client,
    server_url: &str,
    bearer: &str,
) -> Result<serde_json::Value> {
    let resp = client
        .get(format!("{server_url}/v1/admin/tokenlimits"))
        .bearer_auth(bearer)
        .send()
        .await
        .context("GET /v1/admin/tokenlimits")?;
    decode_response(resp, "list token limits").await
}

/// `PATCH /v1/admin/tokenlimits/:id` with a new expiry.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn extend_request(
    client: &reqwest::Client,
    server_url: &str,
    id: &str,
    expires_at: &str,
    bearer: &str,
) -> Result<serde_json::Value> {
    let resp = client
        .patch(format!("{server_url}/v1/admin/tokenlimits/{id}"))
        .bearer_auth(bearer)
        .json(&json!({ "expires_at": expires_at }))
        .send()
        .await
        .with_context(|| format!("PATCH /v1/admin/tokenlimits/{id}"))?;
    decode_response(resp, "extend token limit").await
}

/// `DELETE /v1/admin/tokenlimits/:id`.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn remove_request(
    client: &reqwest::Client,
    server_url: &str,
    id: &str,
    bearer: &str,
) -> Result<serde_json::Value> {
    let resp = client
        .delete(format!("{server_url}/v1/admin/tokenlimits/{id}"))
        .bearer_auth(bearer)
        .send()
        .await
        .with_context(|| format!("DELETE /v1/admin/tokenlimits/{id}"))?;
    decode_response(resp, "remove token limit").await
}

// ---- confirmation ----

/// Interactive confirmation gate for `remove`. Tests may call this directly.
///
/// # Errors
///
/// Returns an error when run non-interactively without `--yes`, or when the
/// user types anything other than `y`/`yes`.
pub fn confirm_remove(args: &RemoveArgs) -> Result<()> {
    use std::io::{BufRead as _, Write as _};

    if args.yes {
        return Ok(());
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(anyhow!(
            "refusing to remove token-limit override `{}` non-interactively without --yes",
            args.id
        ));
    }
    eprint!("Remove token-limit override `{}`? [y/N] ", args.id);
    std::io::stderr().flush().ok();
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let answer = line.trim().to_ascii_lowercase();
    if answer == "y" || answer == "yes" {
        Ok(())
    } else {
        Err(anyhow!("aborted"))
    }
}

// ---- emit ----

fn emit_one(v: &serde_json::Value, json: bool) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(v) {
            println!("{s}");
        }
        return;
    }
    print_row(v);
}

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
    if rows.is_empty() {
        println!("(no active overrides)");
        return;
    }
    for row in rows {
        print_row(row);
    }
}

fn print_row(v: &serde_json::Value) {
    let id = v["id"].as_str().unwrap_or("?");
    let subject_kind = v["subject_kind"].as_str().unwrap_or("?");
    let subject = v["subject"].as_str().unwrap_or("?");
    let hourly = v["hourly"].as_i64().unwrap_or(0);
    let daily = v["daily"].as_i64().unwrap_or(0);
    let note = v["note"].as_str().unwrap_or("");
    println!("  {id}  {subject_kind}:{subject:<24} {hourly:>9}/h {daily:>11}/d  {note}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_args(cidr: Option<&str>, user: Option<&str>) -> AddArgs {
        AddArgs {
            cidr: cidr.map(str::to_owned),
            user: user.map(str::to_owned),
            hourly: 1,
            daily: 1,
            ttl: "1h".to_owned(),
            note: None,
        }
    }

    #[test]
    fn resolve_subject_picks_cidr_or_user() {
        assert_eq!(
            resolve_subject(&add_args(Some("203.0.113.0/24"), None)).unwrap(),
            ("cidr", "203.0.113.0/24".to_owned())
        );
        assert_eq!(
            resolve_subject(&add_args(None, Some("alice"))).unwrap(),
            ("user", "alice".to_owned())
        );
    }

    #[test]
    fn resolve_subject_rejects_neither() {
        assert!(resolve_subject(&add_args(None, None)).is_err());
    }
}

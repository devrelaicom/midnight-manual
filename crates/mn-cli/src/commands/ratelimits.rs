//! `mnm ratelimits` — admin CRUD for per-CIDR rate-limit overrides.
//!
//! The entire subtree is admin-only (hidden from `--help` by default per D23
//! unless `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1`; it still executes when called
//! by name). Every subcommand resolves an admin bearer from `auth.toml`; if
//! none is available the command exits with a clear "run `mnm login` first"
//! error.
//!
//! - `add --cidr <CIDR> --limit <N>/s --ttl <DUR> [--note <S>]` — `POST`.
//! - `list` — `GET` active overrides.
//! - `extend <id> --ttl <DUR>` — `PATCH` a new expiry.
//! - `remove <id> [--yes]` — `DELETE` (confirms unless `--yes`).

use anyhow::{anyhow, Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mn_core::auth_file::AuthFile;
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

/// `mnm ratelimits <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: RatelimitsCmd,
}

/// `ratelimits` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum RatelimitsCmd {
    /// Create a new per-CIDR override.
    Add(AddArgs),
    /// List overrides still in effect.
    List,
    /// Extend an existing override's TTL.
    Extend(ExtendArgs),
    /// Remove an override (confirms unless `--yes`).
    Remove(RemoveArgs),
}

/// `mnm ratelimits add` arguments.
#[derive(Debug, ClapArgs)]
pub struct AddArgs {
    /// Network block in `addr/prefix` form, e.g. `203.0.113.0/24`.
    #[arg(long)]
    pub cidr: String,
    /// Requests-per-second ceiling. Accepts `200` or `200/s`.
    #[arg(long)]
    pub limit: String,
    /// Time-to-live from now, e.g. `48h`, `30m`, `7d`, `90s`.
    #[arg(long)]
    pub ttl: String,
    /// Optional operator note.
    #[arg(long)]
    pub note: Option<String>,
}

/// `mnm ratelimits extend <id> --ttl <DUR>` arguments.
#[derive(Debug, ClapArgs)]
pub struct ExtendArgs {
    /// Override id (UUID).
    pub id: String,
    /// New time-to-live from now, e.g. `24h`.
    #[arg(long)]
    pub ttl: String,
}

/// `mnm ratelimits remove <id>` arguments.
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
        RatelimitsCmd::Add(a) => {
            let limit_rps = parse_limit(&a.limit)?;
            let expires_at = expiry_from_ttl(&a.ttl)?;
            let token = require_admin_token()?;
            let value = add_request(
                &client,
                &server_url,
                &a.cidr,
                limit_rps,
                &expires_at,
                a.note.as_deref(),
                &token,
            )
            .await?;
            emit_one(&value, json);
        }
        RatelimitsCmd::List => {
            let token = require_admin_token()?;
            let value = list_request(&client, &server_url, &token).await?;
            emit_list(&value, json);
        }
        RatelimitsCmd::Extend(e) => {
            let expires_at = expiry_from_ttl(&e.ttl)?;
            let token = require_admin_token()?;
            let value = extend_request(&client, &server_url, &e.id, &expires_at, &token).await?;
            emit_one(&value, json);
        }
        RatelimitsCmd::Remove(r) => {
            confirm_remove(&r)?;
            let token = require_admin_token()?;
            let value = remove_request(&client, &server_url, &r.id, &token).await?;
            emit_one(&value, json);
        }
    }
    Ok(())
}

// ---- parsing helpers ----

/// Parse a `--limit` argument, accepting either `200` or `200/s`.
///
/// # Errors
///
/// Returns an error when the numeric part is missing or not a positive
/// integer.
pub fn parse_limit(raw: &str) -> Result<i32> {
    let trimmed = raw.trim();
    let digits = trimmed.strip_suffix("/s").unwrap_or(trimmed).trim();
    let n: i32 = digits.parse().with_context(|| {
        format!("invalid --limit `{raw}`: expected an integer like `200` or `200/s`")
    })?;
    if n <= 0 {
        return Err(anyhow!("invalid --limit `{raw}`: must be a positive integer"));
    }
    Ok(n)
}

/// Parse a single-unit duration string (`90s`, `30m`, `48h`, `7d`).
///
/// # Errors
///
/// Returns an error on an empty string, an unknown unit suffix, a missing or
/// non-numeric magnitude, or arithmetic overflow.
pub fn parse_ttl(raw: &str) -> Result<Duration> {
    let s = raw.trim();
    let unit = s
        .chars()
        .last()
        .ok_or_else(|| anyhow!("invalid --ttl: empty"))?;
    let per_unit: i64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        other => {
            return Err(anyhow!(
                "invalid --ttl `{raw}`: unknown unit `{other}` (use s, m, h, or d)"
            ))
        }
    };
    let magnitude: i64 = s[..s.len() - 1]
        .parse()
        .with_context(|| format!("invalid --ttl `{raw}`: expected a number before the unit"))?;
    if magnitude <= 0 {
        return Err(anyhow!("invalid --ttl `{raw}`: magnitude must be positive"));
    }
    let secs = magnitude
        .checked_mul(per_unit)
        .ok_or_else(|| anyhow!("invalid --ttl `{raw}`: duration overflow"))?;
    Ok(Duration::seconds(secs))
}

/// Resolve a TTL string to an RFC 3339 `expires_at` timestamp (`now + ttl`).
///
/// # Errors
///
/// Returns an error when `ttl` is not a valid single-unit duration.
pub(crate) fn expiry_from_ttl(ttl: &str) -> Result<String> {
    let dur = parse_ttl(ttl)?;
    let when = OffsetDateTime::now_utc() + dur;
    when.format(&Rfc3339).context("format expires_at")
}

// ---- HTTP helpers (pub for integration tests) ----

/// `POST /v1/admin/ratelimits`.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn add_request(
    client: &reqwest::Client,
    server_url: &str,
    cidr: &str,
    limit_rps: i32,
    expires_at: &str,
    note: Option<&str>,
    bearer: &str,
) -> Result<serde_json::Value> {
    let mut body = serde_json::Map::new();
    body.insert("cidr".to_owned(), json!(cidr));
    body.insert("limit_rps".to_owned(), json!(limit_rps));
    body.insert("expires_at".to_owned(), json!(expires_at));
    if let Some(n) = note {
        body.insert("note".to_owned(), json!(n));
    }
    let resp = client
        .post(format!("{server_url}/v1/admin/ratelimits"))
        .bearer_auth(bearer)
        .json(&body)
        .send()
        .await
        .context("POST /v1/admin/ratelimits")?;
    decode_response(resp, "add override").await
}

/// `GET /v1/admin/ratelimits`.
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
        .get(format!("{server_url}/v1/admin/ratelimits"))
        .bearer_auth(bearer)
        .send()
        .await
        .context("GET /v1/admin/ratelimits")?;
    decode_response(resp, "list overrides").await
}

/// `PATCH /v1/admin/ratelimits/:id` with a new expiry.
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
        .patch(format!("{server_url}/v1/admin/ratelimits/{id}"))
        .bearer_auth(bearer)
        .json(&json!({ "expires_at": expires_at }))
        .send()
        .await
        .with_context(|| format!("PATCH /v1/admin/ratelimits/{id}"))?;
    decode_response(resp, "extend override").await
}

/// `DELETE /v1/admin/ratelimits/:id`.
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
        .delete(format!("{server_url}/v1/admin/ratelimits/{id}"))
        .bearer_auth(bearer)
        .send()
        .await
        .with_context(|| format!("DELETE /v1/admin/ratelimits/{id}"))?;
    decode_response(resp, "remove override").await
}

// ---- confirmation ----

/// Interactive confirmation gate for `remove`. Tests may call this directly
/// to exercise the `--yes` and non-interactive-refusal paths.
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
            "refusing to remove override `{}` non-interactively without --yes",
            args.id
        ));
    }
    eprint!("Remove rate-limit override `{}`? [y/N] ", args.id);
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

// ---- auth ----

fn require_admin_token() -> Result<String> {
    require_admin_token_from(&mn_core::config::StdEnv)
}

/// `require_admin_token` over an arbitrary [`mn_core::config::ConfigEnv`] —
/// exposed so integration tests can drive the lookup with a synthetic
/// `HOME` / `XDG_CONFIG_HOME`.
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

// ---- emit ----

pub(crate) async fn decode_response(
    resp: reqwest::Response,
    op: &str,
) -> Result<serde_json::Value> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("{status} from {op}: {body}"));
    }
    serde_json::from_str(&body).with_context(|| format!("parse {op} response body"))
}

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
    let cidr = v["cidr"].as_str().unwrap_or("?");
    let limit = v["limit_rps"].as_i64().unwrap_or(0);
    let note = v["note"].as_str().unwrap_or("");
    println!("  {id}  {cidr:<20} {limit:>6}/s  {note}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_limit_accepts_plain_and_suffixed() {
        assert_eq!(parse_limit("200").unwrap(), 200);
        assert_eq!(parse_limit("200/s").unwrap(), 200);
        assert_eq!(parse_limit("  50/s ").unwrap(), 50);
    }

    #[test]
    fn parse_limit_rejects_garbage() {
        assert!(parse_limit("").is_err());
        assert!(parse_limit("0").is_err());
        assert!(parse_limit("-5").is_err());
        assert!(parse_limit("fast").is_err());
    }

    #[test]
    fn parse_ttl_handles_each_unit() {
        assert_eq!(parse_ttl("90s").unwrap(), Duration::seconds(90));
        assert_eq!(parse_ttl("30m").unwrap(), Duration::seconds(1800));
        assert_eq!(parse_ttl("48h").unwrap(), Duration::seconds(172_800));
        assert_eq!(parse_ttl("7d").unwrap(), Duration::seconds(604_800));
    }

    #[test]
    fn parse_ttl_rejects_garbage() {
        assert!(parse_ttl("").is_err());
        assert!(parse_ttl("10").is_err());
        assert!(parse_ttl("10y").is_err());
        assert!(parse_ttl("abch").is_err());
        assert!(parse_ttl("0h").is_err());
    }
}

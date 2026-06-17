//! `mnm sources` — source registry inspection and admin CRUD.
//!
//! Public sub-subcommands:
//!
//! - `list`  — list active sources (anonymous read of `GET /v1/sources`).
//! - `show <slug>` — fetch one source (anonymous read of `GET /v1/sources/:slug`).
//!
//! Admin sub-subcommands (hidden from `--help` by default per D23 unless
//! `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1`; they still execute when called by
//! name). All four require an admin bearer in `auth.toml`; if none is
//! available the command exits with a clear "run `mnm login` first" error.
//!
//! - `create`   — `POST   /v1/admin/sources`.
//! - `update`   — `PATCH  /v1/admin/sources/:slug`.
//! - `retire`   — `DELETE /v1/admin/sources/:slug` (confirms unless `--yes`).
//! - `list-all` — `GET    /v1/admin/sources` (includes retired rows).

use anyhow::{anyhow, Context as _, Result};
use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use mnm_core::auth_file::AuthFile;
use serde::Serialize;
use serde_json::json;
use time::OffsetDateTime;

/// `mnm sources <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: SourcesCmd,
}

/// `sources` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum SourcesCmd {
    /// List active sources from the cloud (anonymous read).
    List,
    /// Show one source's metadata by slug (anonymous read).
    Show {
        /// Source slug.
        slug: String,
    },
    /// Register a new source (admin).
    #[command(hide = true)]
    Create(CreateArgs),
    /// Update an existing source (admin).
    #[command(hide = true)]
    Update(UpdateArgs),
    /// Retire a source (admin). Soft-delete; not reversible via the CLI.
    #[command(hide = true)]
    Retire(RetireArgs),
    /// List every source including retired ones (admin).
    #[command(name = "list-all", hide = true)]
    ListAll,
}

/// `mnm sources create` arguments.
#[derive(Debug, ClapArgs)]
pub struct CreateArgs {
    /// URL-safe slug; must match `^[a-z0-9][a-z0-9-]*[a-z0-9]$`, up to 63 chars.
    #[arg(long)]
    pub slug: String,
    /// Human-readable label. Defaults to `slug` when omitted.
    #[arg(long)]
    pub display_name: Option<String>,
    /// Source kind discriminator.
    #[arg(long, value_enum)]
    pub kind: CreateKind,
    /// Canonical origin URL (git URL, docs site URL, etc.).
    #[arg(long)]
    pub origin_url: Option<String>,
    /// Historical-version retention count. Range `[1, 50]`; defaults to 5.
    #[arg(long)]
    pub retention_count: Option<i32>,
}

/// `mnm sources update` arguments.
#[derive(Debug, ClapArgs)]
pub struct UpdateArgs {
    /// Source slug.
    pub slug: String,
    /// New display label.
    #[arg(long)]
    pub display_name: Option<String>,
    /// New origin URL.
    #[arg(long)]
    pub origin_url: Option<String>,
    /// New retention count.
    #[arg(long)]
    pub retention_count: Option<i32>,
}

/// `mnm sources retire` arguments.
#[derive(Debug, ClapArgs)]
pub struct RetireArgs {
    /// Source slug to retire.
    pub slug: String,
    /// Skip the interactive confirmation.
    #[arg(long)]
    pub yes: bool,
}

/// Wire-string source kinds — kept separate from the type the server
/// deserializes so that `clap` can value-enum a slug-friendly form.
#[derive(Debug, Clone, Copy, ValueEnum)]
#[clap(rename_all = "snake_case")]
pub enum CreateKind {
    /// Documentation website.
    DocsSite,
    /// Source-code repository.
    CodeRepo,
    /// One-off standalone files.
    Standalone,
    /// Mixed docs + code.
    Mixed,
}

impl CreateKind {
    const fn wire(self) -> &'static str {
        match self {
            Self::DocsSite => "docs_site",
            Self::CodeRepo => "code_repo",
            Self::Standalone => "standalone",
            Self::Mixed => "mixed",
        }
    }
}

/// Body shape `mnm sources create` POSTs.
#[derive(Debug, Serialize)]
struct CreateBody<'a> {
    slug: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<&'a str>,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retention_count: Option<i32>,
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
        SourcesCmd::List => {
            let value = get_json(&client, &format!("{server_url}/v1/sources"), None).await?;
            emit_list(&value, json);
        }
        SourcesCmd::Show { slug } => {
            let value = get_json(&client, &format!("{server_url}/v1/sources/{slug}"), None).await?;
            emit_value(&value, json);
        }
        SourcesCmd::Create(c) => {
            let token = require_admin_token()?;
            let value = create_request(&client, &server_url, &c, &token).await?;
            emit_value(&value, json);
        }
        SourcesCmd::Update(u) => {
            let token = require_admin_token()?;
            let value = update_request(&client, &server_url, &u, &token).await?;
            emit_value(&value, json);
        }
        SourcesCmd::Retire(r) => {
            confirm_retire(&r)?;
            let token = require_admin_token()?;
            let value = retire_request(&client, &server_url, &r.slug, &token).await?;
            emit_value(&value, json);
        }
        SourcesCmd::ListAll => {
            let token = require_admin_token()?;
            let value = list_all_request(&client, &server_url, &token).await?;
            emit_list_all(&value, json);
        }
    }
    Ok(())
}

/// `POST /v1/admin/sources` against `server_url` with `bearer`. Exposed for
/// integration tests so they can drive the HTTP transport against a
/// wiremock server without going through bearer resolution.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn create_request(
    client: &reqwest::Client,
    server_url: &str,
    args: &CreateArgs,
    bearer: &str,
) -> Result<serde_json::Value> {
    let body = CreateBody {
        slug: &args.slug,
        display_name: args.display_name.as_deref(),
        kind: args.kind.wire(),
        origin_url: args.origin_url.as_deref(),
        retention_count: args.retention_count,
    };
    let resp = client
        .post(format!("{server_url}/v1/admin/sources"))
        .bearer_auth(bearer)
        .json(&body)
        .send()
        .await
        .context("POST /v1/admin/sources")?;
    decode_response(resp, "create source").await
}

/// `PATCH /v1/admin/sources/:slug`. Exposed for tests.
///
/// # Errors
///
/// Returns an error if no patch field is set, on transport failure, or on
/// non-2xx HTTP responses.
pub async fn update_request(
    client: &reqwest::Client,
    server_url: &str,
    args: &UpdateArgs,
    bearer: &str,
) -> Result<serde_json::Value> {
    if args.display_name.is_none() && args.origin_url.is_none() && args.retention_count.is_none() {
        return Err(anyhow!(
            "supply at least one of --display-name, --origin-url, --retention-count"
        ));
    }
    let mut body = serde_json::Map::new();
    if let Some(d) = &args.display_name {
        body.insert("display_name".to_owned(), json!(d));
    }
    if let Some(u) = &args.origin_url {
        body.insert("origin_url".to_owned(), json!(u));
    }
    if let Some(r) = args.retention_count {
        body.insert("retention_count".to_owned(), json!(r));
    }
    let resp = client
        .patch(format!("{server_url}/v1/admin/sources/{}", args.slug))
        .bearer_auth(bearer)
        .json(&body)
        .send()
        .await
        .context("PATCH /v1/admin/sources/:slug")?;
    decode_response(resp, "update source").await
}

/// `DELETE /v1/admin/sources/:slug`. Exposed for tests.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn retire_request(
    client: &reqwest::Client,
    server_url: &str,
    slug: &str,
    bearer: &str,
) -> Result<serde_json::Value> {
    let resp = client
        .delete(format!("{server_url}/v1/admin/sources/{slug}"))
        .bearer_auth(bearer)
        .send()
        .await
        .context("DELETE /v1/admin/sources/:slug")?;
    decode_response(resp, "retire source").await
}

/// `GET /v1/admin/sources`. Exposed for tests.
///
/// # Errors
///
/// Returns an error on transport failure or non-2xx HTTP responses.
pub async fn list_all_request(
    client: &reqwest::Client,
    server_url: &str,
    bearer: &str,
) -> Result<serde_json::Value> {
    get_json(client, &format!("{server_url}/v1/admin/sources"), Some(bearer)).await
}

/// Interactive confirmation gate for `retire`. Tests may call this directly
/// to exercise the `--yes` and non-interactive-refusal paths.
///
/// # Errors
///
/// Returns an error when run non-interactively without `--yes` or when the
/// user types anything other than `y`/`yes`.
pub fn confirm_retire(args: &RetireArgs) -> Result<()> {
    use std::io::{BufRead as _, Write as _};

    if args.yes {
        return Ok(());
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(anyhow!("refusing to retire `{}` non-interactively without --yes", args.slug));
    }
    eprint!(
        "Retire source `{}`? Retired sources are restored only by direct SQL. [y/N] ",
        args.slug
    );
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

async fn get_json(
    client: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
) -> Result<serde_json::Value> {
    let mut req = client.get(url);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    decode_response(resp, "fetch").await
}

async fn decode_response(resp: reqwest::Response, op: &str) -> Result<serde_json::Value> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("{status} from {op}: {body}"));
    }
    serde_json::from_str(&body).with_context(|| format!("parse {op} response body"))
}

/// Return the active admin bearer from `auth.toml`, or a contextful error
/// instructing the operator to authenticate.
fn require_admin_token() -> Result<String> {
    require_admin_token_from(&mnm_core::config::StdEnv)
}

/// `require_admin_token` over an arbitrary [`mnm_core::config::ConfigEnv`] —
/// exposed so integration tests can drive the lookup with a synthetic
/// `HOME` / `XDG_CONFIG_HOME` instead of mutating the live process env.
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

fn emit_value(v: &serde_json::Value, json: bool) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(v) {
            println!("{s}");
        }
        return;
    }
    if v.is_array() {
        for row in v.as_array().unwrap() {
            print_row(row);
        }
    } else {
        print_show(v);
    }
}

/// Render the paginated `GET /v1/sources` object (`{sources, total,
/// next_cursor}`). In `--json` mode the whole envelope is printed verbatim so
/// scripts can drive `next_cursor` themselves.
fn emit_list(v: &serde_json::Value, json: bool) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(v) {
            println!("{s}");
        }
        return;
    }
    let Some(rows) = v["sources"].as_array() else {
        println!("(unexpected response shape)");
        return;
    };
    for row in rows {
        print_row(row);
    }
    if !v["next_cursor"].is_null() {
        let total = v["total"].as_i64().unwrap_or(0);
        println!("  … showing {} of {total} (more pages available)", rows.len());
    }
}

fn emit_list_all(v: &serde_json::Value, json: bool) {
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
    let (mut active, mut retired): (Vec<&serde_json::Value>, Vec<&serde_json::Value>) =
        (Vec::new(), Vec::new());
    for row in rows {
        if row["retired_at"].is_null() {
            active.push(row);
        } else {
            retired.push(row);
        }
    }
    println!("ACTIVE ({}):", active.len());
    for row in active {
        print_row(row);
    }
    if !retired.is_empty() {
        println!();
        println!("RETIRED ({}):", retired.len());
        for row in retired {
            print_row(row);
        }
    }
}

fn print_row(row: &serde_json::Value) {
    let slug = row["slug"].as_str().unwrap_or("?");
    let display = row["display_name"].as_str().unwrap_or("?");
    let kind = row["kind"].as_str().unwrap_or("?");
    println!("  {slug:<32} {kind:<12} {display}");
}

fn print_show(v: &serde_json::Value) {
    let slug = v["slug"].as_str().unwrap_or("?");
    let display = v["display_name"].as_str().unwrap_or("?");
    let kind = v["kind"].as_str().unwrap_or("?");
    let origin = v["origin_url"].as_str().unwrap_or("(none)");
    let retention = v["retention_count"].as_i64().unwrap_or(0);
    let retired = if v["retired_at"].is_null() {
        "active"
    } else {
        "retired"
    };
    println!("slug:           {slug}");
    println!("display name:   {display}");
    impl_show_field("kind", kind);
    impl_show_field("origin url", origin);
    println!("retention:      {retention}");
    impl_show_field("state", retired);
}

fn impl_show_field(key: &str, val: &str) {
    println!("{key:<16}{val}");
}

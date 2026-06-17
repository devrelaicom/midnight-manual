//! `mnm auth {github,status,logout}` — GitHub-OAuth read-uplift flow and
//! local auth-file inspection (FR-115).
//!
//! ## `mnm auth github`
//!
//! 1. Bind a local listener on `127.0.0.1:0` to discover a free port.
//!
//! 2. Open the browser to
//!    `<server>/v1/auth/github/start?cli_port=<port>`. The server mints a
//!    state token and redirects the browser to GitHub's authorize URL.
//!
//! 3. After GitHub redirects to the server's callback, the server
//!    verifies org membership, mints a 30-day read-uplift JWT, and
//!    redirects the browser back to
//!    `http://127.0.0.1:<port>/oauth?token=…&github_login=…&expires_at=…`.
//!
//! 4. The CLI's local listener captures those query params, persists
//!    the token to `auth.toml[read_uplift]`, prints a status line, and
//!    exits.
//!
//! ## `mnm auth status`
//!
//! Renders both token states (admin + read-uplift) from `auth.toml`,
//! redacting the token bytes (FR-019).
//!
//! ## `mnm auth logout`
//!
//! Clears the `[read_uplift]` section only — admin tokens are
//! managed by `mnm login`.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use serde::Serialize;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener as TokioTcpListener;

/// `mnm auth <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: AuthCmd,
}

/// `auth` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthCmd {
    /// Run the GitHub OAuth read-uplift flow.
    Github(GithubArgs),
    /// Show the state of both tokens (admin + read-uplift).
    Status,
    /// Remove the read-uplift token from `auth.toml`. Admin tokens are
    /// untouched.
    Logout,
}

/// Args for `mnm auth github`.
#[derive(Debug, ClapArgs)]
pub struct GithubArgs {
    /// Don't try to open the system browser; print the authorize URL
    /// and let the user open it manually. The local-listener capture
    /// still runs — paste the URL into any browser on this machine.
    #[arg(long)]
    pub no_browser: bool,

    /// Don't actually persist the token.
    #[arg(long)]
    pub dry_run: bool,

    /// Override the listener bind timeout, in seconds. Defaults to 300.
    #[arg(long, default_value_t = 300)]
    pub timeout_s: u64,
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error when the OAuth flow fails, when the local listener
/// cannot bind, when the server cannot be reached, or when `auth.toml`
/// cannot be written.
pub async fn run(args: Args, server_flag: Option<&str>, json: bool) -> Result<()> {
    match args.cmd {
        AuthCmd::Github(a) => github(a, server_flag, json).await,
        AuthCmd::Status => status(json),
        AuthCmd::Logout => logout(json),
    }
}

#[derive(Debug, Serialize)]
struct StatusEntry {
    present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in_s: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expired: Option<bool>,
}

#[derive(Debug, Serialize)]
struct StatusOutput<'a> {
    action: &'a str,
    auth_file: String,
    admin: StatusEntry,
    read_uplift: StatusEntry,
}

fn auth_path() -> Result<std::path::PathBuf> {
    mnm_core::paths::auth_file_path(&mnm_core::config::StdEnv)
        .ok_or_else(|| anyhow!("could not resolve auth.toml path (set XDG_CONFIG_HOME or HOME)"))
}

fn status(json: bool) -> Result<()> {
    let path = auth_path()?;
    status_with_path(&path, json)
}

/// Path-explicit variant of the internal `status` dispatch, exposed for
/// integration testing.
///
/// # Errors
///
/// Returns an error when `path` exists but is malformed (parse / perms).
pub fn status_with_path(path: &Path, json: bool) -> Result<()> {
    let file = mnm_core::auth_file::AuthFile::read_optional(path)?
        .unwrap_or_else(mnm_core::auth_file::AuthFile::empty);
    let now = OffsetDateTime::now_utc();

    let admin = match &file.admin {
        Some(a) => StatusEntry {
            present: true,
            identity: Some(a.user_id.clone()),
            expires_at: a
                .expires_at
                .format(&time::format_description::well_known::Rfc3339)
                .ok(),
            expires_in_s: Some((a.expires_at - now).whole_seconds()),
            expired: Some(now >= a.expires_at),
        },
        None => StatusEntry {
            present: false,
            identity: None,
            expires_at: None,
            expires_in_s: None,
            expired: None,
        },
    };
    let read_uplift = match &file.read_uplift {
        Some(r) => StatusEntry {
            present: true,
            identity: Some(r.github_login.clone()),
            expires_at: r
                .expires_at
                .format(&time::format_description::well_known::Rfc3339)
                .ok(),
            expires_in_s: Some((r.expires_at - now).whole_seconds()),
            expired: Some(now >= r.expires_at),
        },
        None => StatusEntry {
            present: false,
            identity: None,
            expires_at: None,
            expires_in_s: None,
            expired: None,
        },
    };

    if json {
        let body = StatusOutput {
            action: "auth.status",
            auth_file: path.display().to_string(),
            admin,
            read_uplift,
        };
        let s = serde_json::to_string(&body).context("serialize status json")?;
        println!("{s}");
    } else {
        println!("auth file: {}", path.display());
        render_status_line("admin", &admin, "mnm login");
        render_status_line("read_uplift", &read_uplift, "mnm auth github");
    }
    Ok(())
}

fn render_status_line(kind: &str, e: &StatusEntry, renew_cmd: &str) {
    if !e.present {
        println!("{kind:<12} absent (run `{renew_cmd}` to create)");
        return;
    }
    let id = e.identity.as_deref().unwrap_or("?");
    let expired = e.expired.unwrap_or(false);
    let mins = e.expires_in_s.unwrap_or(0) / 60;
    if expired {
        println!("{kind:<12} present ({id}) — EXPIRED ({mins} min ago); run `{renew_cmd}`");
    } else {
        println!("{kind:<12} present ({id}) — expires in {mins} min");
    }
}

#[derive(Debug, Serialize)]
struct LogoutOutput<'a> {
    action: &'a str,
    auth_file: String,
    cleared: bool,
}

fn logout(json: bool) -> Result<()> {
    let path = auth_path()?;
    logout_with_path(&path, json)
}

/// Path-explicit variant of the internal `logout` dispatch, exposed for
/// integration testing.
///
/// # Errors
///
/// Returns an error when the file can't be read or written.
pub fn logout_with_path(path: &Path, json: bool) -> Result<()> {
    let mut file = match mnm_core::auth_file::AuthFile::read_optional(path)? {
        Some(f) => f,
        None => mnm_core::auth_file::AuthFile::empty(),
    };
    let cleared = file.read_uplift.is_some();
    file.read_uplift = None;
    if cleared {
        file.write(path)
            .with_context(|| format!("update {}", path.display()))?;
    }
    if json {
        let body = LogoutOutput {
            action: "auth.logout",
            auth_file: path.display().to_string(),
            cleared,
        };
        let s = serde_json::to_string(&body).context("serialize logout json")?;
        println!("{s}");
    } else if cleared {
        println!("cleared read-uplift token from {}", path.display());
    } else {
        println!("no read-uplift token to clear at {}", path.display());
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct GithubOutput<'a> {
    action: &'a str,
    github_login: &'a str,
    expires_at: String,
    expires_in_s: i64,
    auth_file: String,
    dry_run: bool,
}

async fn github(args: GithubArgs, server_flag: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server_flag);
    let auth_path = auth_path()?;

    // Bind a local listener on a free port. We use a std listener first so
    // the bind error is synchronous and easy to surface; then promote to
    // tokio for the accept loop.
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .context("bind 127.0.0.1:0 for OAuth callback")?;
    let port = listener.local_addr().context("read local_addr")?.port();
    listener
        .set_nonblocking(true)
        .context("set local listener non-blocking")?;
    let listener = TokioTcpListener::from_std(listener).context("convert to tokio listener")?;

    let authorize_url = format!("{server_url}/v1/auth/github/start?cli_port={port}");
    if args.no_browser {
        println!("# open this URL in your browser:");
        println!("{authorize_url}");
    } else if let Err(e) = open_browser(&authorize_url) {
        tracing::warn!(error = %e, "could not open browser; falling back to manual");
        println!("# could not open browser ({e}); open this URL manually:");
        println!("{authorize_url}");
    } else {
        println!("# opened browser; complete the GitHub login flow");
    }

    let params = run_with_paths(&listener, Duration::from_secs(args.timeout_s.max(1)))
        .await
        .context("wait for OAuth callback")?;

    let token = params
        .get("token")
        .ok_or_else(|| anyhow!("callback missing `token` param"))?
        .clone();
    let github_login = params
        .get("github_login")
        .ok_or_else(|| anyhow!("callback missing `github_login` param"))?
        .clone();
    let expires_at_str = params
        .get("expires_at")
        .ok_or_else(|| anyhow!("callback missing `expires_at` param"))?;
    let expires_at_unix: i64 = expires_at_str
        .parse()
        .context("callback returned non-integer expires_at")?;
    let expires_at = OffsetDateTime::from_unix_timestamp(expires_at_unix)
        .context("callback returned out-of-range expires_at")?;

    if !args.dry_run {
        mnm_core::auth_file::AuthFile::write_read_uplift_token(
            &auth_path,
            &github_login,
            &token,
            expires_at,
        )
        .with_context(|| format!("persist read-uplift token to {}", auth_path.display()))?;
    }

    let now = OffsetDateTime::now_utc();
    let expires_in_s = (expires_at - now).whole_seconds();
    let expires_iso = expires_at
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    if json {
        let body = GithubOutput {
            action: "auth.github",
            github_login: &github_login,
            expires_at: expires_iso,
            expires_in_s,
            auth_file: auth_path.display().to_string(),
            dry_run: args.dry_run,
        };
        let s = serde_json::to_string(&body).context("serialize github json")?;
        println!("{s}");
    } else if args.dry_run {
        println!(
            "authenticated as {github_login} (DRY RUN — token not persisted; would expire in {} days)",
            expires_in_s.max(0) / 86400,
        );
    } else {
        println!(
            "authenticated as {github_login}; read-uplift token expires in {} days",
            expires_in_s.max(0) / 86400,
        );
    }
    Ok(())
}

/// Accept exactly one connection on `listener`, read the HTTP request line,
/// parse `/oauth?…` query params, write a small success page, and return
/// the parsed params. Times out after `deadline`.
pub async fn run_with_paths(
    listener: &TokioTcpListener,
    deadline: Duration,
) -> Result<HashMap<String, String>> {
    let accept = async {
        loop {
            let (mut socket, _peer) = listener.accept().await.context("accept connection")?;
            // Read up to 8 KiB of the request — enough for any plausible
            // OAuth-callback URL but tightly bounded so a stuck client
            // can't pin our memory.
            let mut buf = vec![0u8; 8192];
            let mut total = 0usize;
            loop {
                let n = socket
                    .read(&mut buf[total..])
                    .await
                    .context("read request")?;
                if n == 0 {
                    break;
                }
                total += n;
                if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if total >= buf.len() {
                    break;
                }
            }
            let head = std::str::from_utf8(&buf[..total]).unwrap_or("");
            let target = parse_request_target(head);
            // Ignore obvious favicon / health probes — we want the real
            // /oauth callback.
            if target.starts_with("/oauth") {
                let params = parse_query(&target);
                // Best-effort respond so the browser sees a friendly page.
                let body = b"<!doctype html><html><body><h1>You can close this tab.</h1><p>mnm has captured the auth token.</p></body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.write_all(body).await;
                let _ = socket.shutdown().await;
                return Ok::<_, anyhow::Error>(params);
            }
            // Not the right path; respond 404 and loop for the real one.
            let body = b"not found";
            let resp = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(resp.as_bytes()).await;
            let _ = socket.write_all(body).await;
            let _ = socket.shutdown().await;
        }
    };

    tokio::time::timeout(deadline, accept)
        .await
        .map_err(|_| anyhow!("timed out waiting for OAuth callback after {deadline:?}"))?
}

fn parse_request_target(head: &str) -> String {
    // First line is `METHOD <target> HTTP/1.1`.
    head.lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("")
        .to_owned()
}

fn parse_query(target: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some((_path, q)) = target.split_once('?') else {
        return out;
    };
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let k = percent_decode(k);
        let v = percent_decode(v);
        out.insert(k, v);
    }
    out
}

/// Minimal application/x-www-form-urlencoded decoder. Handles `+` → space
/// and `%XX` → byte. Anything malformed is preserved as-is.
fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'+' {
            out.push(b' ');
            i += 1;
        } else if b == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                #[allow(clippy::cast_possible_truncation)]
                out.push(((h << 4) | l) as u8);
                i += 3;
            } else {
                out.push(b);
                i += 1;
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(unix)]
fn open_browser(url: &str) -> Result<()> {
    // `open` on macOS, `xdg-open` on Linux.
    #[cfg(target_os = "macos")]
    let bin = "open";
    #[cfg(not(target_os = "macos"))]
    let bin = "xdg-open";

    let status = std::process::Command::new(bin)
        .arg(url)
        .status()
        .with_context(|| format!("spawn `{bin}`"))?;
    if !status.success() {
        return Err(anyhow!("`{bin}` exited with {status}"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn open_browser(url: &str) -> Result<()> {
    let status = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .status()
        .context("spawn cmd /c start")?;
    if !status.success() {
        return Err(anyhow!("`cmd /c start` exited with {status}"));
    }
    Ok(())
}

/// Test-only helper: build the auth-toml shape we'd write after the flow.
/// Exposed so the integration test can drive the listener without going
/// through the full `github()` entry point.
///
/// # Errors
///
/// Same as [`mnm_core::auth_file::AuthFile::write_read_uplift_token`].
pub fn persist_read_uplift(
    auth_path: &Path,
    github_login: &str,
    token: &str,
    expires_at: OffsetDateTime,
) -> Result<()> {
    mnm_core::auth_file::AuthFile::write_read_uplift_token(
        auth_path,
        github_login,
        token,
        expires_at,
    )
    .with_context(|| format!("persist read-uplift token to {}", auth_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_simple_strings() {
        assert_eq!(percent_decode("hello"), "hello");
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("a%3Db"), "a=b");
    }

    #[test]
    fn percent_decode_passes_through_malformed() {
        assert_eq!(percent_decode("a%ZZb"), "a%ZZb");
        // truncated %X at the end
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn parse_query_picks_up_oauth_params() {
        let q = parse_query("/oauth?token=jwt.abc&github_login=aaron&expires_at=1700000000");
        assert_eq!(q.get("token").map(String::as_str), Some("jwt.abc"));
        assert_eq!(q.get("github_login").map(String::as_str), Some("aaron"));
        assert_eq!(q.get("expires_at").map(String::as_str), Some("1700000000"));
    }

    #[test]
    fn parse_query_empty_when_no_qs() {
        let q = parse_query("/oauth");
        assert!(q.is_empty());
    }

    #[test]
    fn parse_request_target_extracts_path() {
        let head = "GET /oauth?x=1 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        assert_eq!(parse_request_target(head), "/oauth?x=1");
    }
}

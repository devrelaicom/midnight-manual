//! `mnm sources list | show <slug>` — developer-facing source registry inspection.

use anyhow::{Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};

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
    /// List active sources from the cloud.
    List,
    /// Show one source's metadata by slug.
    Show {
        /// Source slug.
        slug: String,
    },
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error on network failure or unexpected non-2xx responses.
pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = resolve_server_url(server);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;

    match args.cmd {
        SourcesCmd::List => {
            let resp = client
                .get(format!("{server_url}/v1/sources"))
                .send()
                .await
                .context("send list request")?
                .error_for_status()
                .context("list sources non-2xx")?;
            let value: serde_json::Value = resp.json().await.context("parse list response")?;
            emit_value(&value, json);
        }
        SourcesCmd::Show { slug } => {
            let resp = client
                .get(format!("{server_url}/v1/sources/{slug}"))
                .send()
                .await
                .context("send show request")?
                .error_for_status()
                .context("show source non-2xx")?;
            let value: serde_json::Value = resp.json().await.context("parse show response")?;
            emit_value(&value, json);
        }
    }
    Ok(())
}

fn resolve_server_url(flag: Option<&str>) -> String {
    if let Some(s) = flag {
        return s.trim_end_matches('/').to_owned();
    }
    let env = mn_core::config::StdEnv;
    let (cfg, _) = mn_core::config::Config::discover(None, &env).unwrap_or_default();
    cfg.server.url
}

fn emit_value(v: &serde_json::Value, json: bool) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(v) {
            println!("{s}");
        }
    } else if v.is_array() {
        for row in v.as_array().unwrap() {
            let slug = row["slug"].as_str().unwrap_or("?");
            let display = row["display_name"].as_str().unwrap_or("?");
            let kind = row["kind"].as_str().unwrap_or("?");
            println!("{slug:<32} {kind:<12} {display}");
        }
    } else {
        let slug = v["slug"].as_str().unwrap_or("?");
        let display = v["display_name"].as_str().unwrap_or("?");
        let kind = v["kind"].as_str().unwrap_or("?");
        let origin = v["origin_url"].as_str().unwrap_or("(none)");
        println!("slug:           {slug}");
        println!("display name:   {display}");
        impl_show_field("kind", kind);
        impl_show_field("origin url", origin);
    }
}

fn impl_show_field(key: &str, val: &str) {
    println!("{key:<16}{val}");
}

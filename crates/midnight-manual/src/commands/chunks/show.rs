//! `mnm chunks show <chunk-id>` — fetch and render one chunk with bundled context.

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use uuid::Uuid;

/// Arguments for `mnm chunks show`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Chunk UUID.
    pub chunk_id: Uuid,
}

/// Run the `chunks show` subcommand.
pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let url = format!("{server_url}/v1/chunks/{}", args.chunk_id);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let bearer = resolve_best_bearer_optional();
    let mut req = client.get(&url);
    if let Some(t) = bearer.as_deref() {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::anyhow!("{status} from {url}: {body}"));
    }
    if json {
        println!("{body}");
    } else {
        render_chunk(&body)?;
    }
    Ok(())
}

fn resolve_best_bearer_optional() -> Option<String> {
    use mnm_core::config::StdEnv;
    let auth_path = mnm_core::paths::auth_file_path(&StdEnv)?;
    let file = mnm_core::auth_file::AuthFile::read_optional(&auth_path)
        .ok()
        .flatten()?;
    let now = time::OffsetDateTime::now_utc();
    file.active_admin_token(now)
        .or_else(|| file.active_read_uplift_token(now))
        .map(str::to_owned)
}

pub(super) fn render_chunk(body: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(body).context("parse response body")?;
    let chunk_index = v["chunk_index"].as_i64().unwrap_or(0) + 1;
    let total = v["total_chunks"].as_i64().unwrap_or(1).max(1);
    let slug = v["source"]["slug"].as_str().unwrap_or("?");
    let path = v["document"]["source_path"].as_str().unwrap_or("?");
    let url = v["document"]["published_url"].as_str().unwrap_or("(none)");
    let headings: Vec<&str> = v["heading_path"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    let content = v["content"].as_str().unwrap_or("");

    println!("chunk {chunk_index}/{total} — {slug}/{path}");
    println!("URL: {url}");
    if !headings.is_empty() {
        println!("heading: > {}", headings.join(" > "));
    }
    println!();
    println!("{content}");
    Ok(())
}

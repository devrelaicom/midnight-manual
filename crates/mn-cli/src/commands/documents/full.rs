//! `mnm documents full <doc-id>` — render the complete document with chunks.

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use uuid::Uuid;

/// Arguments for `mnm documents full`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Document UUID.
    pub document_id: Uuid,
}

/// Run the `documents full` subcommand.
pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let url = format!("{server_url}/v1/documents/{}/full", args.document_id);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build HTTP client")?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::PRECONDITION_FAILED {
        let v: serde_json::Value = serde_json::from_str(&body).context("parse 412 body")?;
        let count = v["chunk_count"].as_u64().unwrap_or(0);
        let cap = v["cap"].as_u64().unwrap_or(0);
        return Err(anyhow::anyhow!(
            "document has {count} chunks (cap {cap}). Use:\n  mnm documents chunks {} --from 0 --limit 100",
            args.document_id
        ));
    }
    if !status.is_success() {
        return Err(anyhow::anyhow!("{status} from {url}: {body}"));
    }
    if json {
        println!("{body}");
    } else {
        render_full(&body)?;
    }
    Ok(())
}

fn render_full(body: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(body).context("parse response body")?;
    let path = v["source_path"].as_str().unwrap_or("?");
    let slug = v["source"]["slug"].as_str().unwrap_or("?");
    let url = v["published_url"].as_str().unwrap_or("(none)");
    let chunks = v["chunks"].as_array().cloned().unwrap_or_default();

    println!("document: {slug}/{path}");
    println!("URL:      {url}");
    println!("chunks:   {} chunks", chunks.len());
    println!();
    for (i, c) in chunks.iter().enumerate() {
        let chunk_index = c["chunk_index"].as_i64().unwrap_or(0);
        let content = c["content"].as_str().unwrap_or("");
        let headings: Vec<&str> = c["heading_path"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        println!("--- chunk {}/{} (index {chunk_index}) ---", i + 1, chunks.len());
        if !headings.is_empty() {
            println!("heading: > {}", headings.join(" > "));
        }
        println!("{content}");
        println!();
    }
    Ok(())
}

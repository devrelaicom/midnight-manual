//! `mnm documents show <doc-id>` — render the overview.

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use uuid::Uuid;

/// Arguments for `mnm documents show`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Document UUID.
    pub document_id: Uuid,
}

/// Run the `documents show` subcommand.
pub async fn run(
    args: Args,
    server: Option<&str>,
    config: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server, config);
    let url = format!("{server_url}/v1/documents/{}", args.document_id);
    let body = super::fetch(&url).await?;
    if json {
        println!("{body}");
    } else {
        render_overview(&body)?;
    }
    Ok(())
}

fn render_overview(body: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(body).context("parse response body")?;
    let path = v["source_path"].as_str().unwrap_or("?");
    let slug = v["source"]["slug"].as_str().unwrap_or("?");
    let url = v["published_url"].as_str().unwrap_or("(none)");
    let lang = v["language"].as_str().unwrap_or("?");
    let chunks = v["chunks"].as_array().cloned().unwrap_or_default();

    println!("document: {slug}/{path}");
    println!("URL:      {url}");
    println!("language: {lang}");
    println!("chunks:   {} chunks", chunks.len());
    println!();
    for (i, c) in chunks.iter().enumerate() {
        let chunk_index = c["chunk_index"].as_i64().unwrap_or(0);
        let tokens = c["token_count"].as_i64().unwrap_or(0);
        println!(
            "  {}. chunk_index={chunk_index}  tokens={tokens}  id={}",
            i + 1,
            c["id"].as_str().unwrap_or(""),
        );
    }
    Ok(())
}

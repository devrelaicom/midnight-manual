//! `mnm documents chunks <doc-id> --from K --limit N` — render a windowed slice.

use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use uuid::Uuid;

/// Arguments for `mnm documents chunks`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Document UUID.
    pub document_id: Uuid,
    /// Starting chunk index offset (0-based).
    #[arg(long, default_value_t = 0)]
    pub from: usize,
    /// Maximum number of chunks to return.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

/// Run the `documents chunks` subcommand.
pub async fn run(
    args: Args,
    server: Option<&str>,
    config: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server, config);
    let url = format!(
        "{server_url}/v1/documents/{}/chunks?from={}&limit={}",
        args.document_id, args.from, args.limit
    );
    let body = super::fetch(&url).await?;
    if json {
        println!("{body}");
    } else {
        render_window(&body)?;
    }
    Ok(())
}

fn render_window(body: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(body).context("parse response body")?;
    let from = v["from"].as_u64().unwrap_or(0);
    let _limit = v["limit"].as_u64().unwrap_or(0);
    let total = v["total_chunks"].as_u64().unwrap_or(0);
    let chunks = v["chunks"].as_array().cloned().unwrap_or_default();
    let to = from + (chunks.len() as u64);

    println!("chunks {from}..{to} of {total} total");
    if chunks.is_empty() {
        println!("(none in range)");
        return Ok(());
    }
    for c in &chunks {
        let chunk_index = c["chunk_index"].as_i64().unwrap_or(0);
        let content = c["content"].as_str().unwrap_or("");
        println!("--- chunk_index {chunk_index} ---");
        println!("{content}");
        println!();
    }
    Ok(())
}

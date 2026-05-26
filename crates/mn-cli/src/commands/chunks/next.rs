//! `mnm chunks next <chunk-id>` — fetch the next N chunks in the same document.

use anyhow::Result;
use clap::Args as ClapArgs;
use uuid::Uuid;

#[derive(Debug, ClapArgs)]
pub struct Args {
    pub chunk_id: Uuid,
    /// Number of chunks to fetch (clamped to [1,100] server-side).
    #[arg(long, default_value_t = 5)]
    pub count: u32,
    /// Show full content instead of a 240-char preview.
    #[arg(long)]
    pub full: bool,
}

pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    super::run_chunk_list(args, server, json, "next").await
}

pub(super) fn render_chunks(body: &str, full: bool) -> anyhow::Result<()> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("parse response body: {e}"))?;
    let chunks = v["chunks"].as_array().cloned().unwrap_or_default();
    if chunks.is_empty() {
        println!("(no further chunks)");
        return Ok(());
    }
    for (i, c) in chunks.iter().enumerate() {
        let n = i + 1;
        let chunk_index = c["chunk_index"].as_i64().unwrap_or(0) + 1;
        let total = c["total_chunks"].as_i64().unwrap_or(1).max(1);
        let slug = c["source"]["slug"].as_str().unwrap_or("?");
        let path = c["document"]["source_path"].as_str().unwrap_or("?");
        let url = c["document"]["published_url"].as_str().unwrap_or("(none)");
        let headings: Vec<&str> = c["heading_path"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        let content_full = c["content"].as_str().unwrap_or("");
        let preview = if full {
            content_full.to_owned()
        } else {
            preview_240(content_full)
        };

        println!("{n}. chunk {chunk_index}/{total} — {slug}/{path}");
        println!("   URL: {url}");
        if !headings.is_empty() {
            println!("   heading: > {}", headings.join(" > "));
        }
        println!();
        for line in preview.lines() {
            println!("   {line}");
        }
        println!();
    }
    Ok(())
}

fn preview_240(s: &str) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= 240 {
        one_line
    } else {
        let head: String = one_line.chars().take(237).collect();
        format!("{head}...")
    }
}

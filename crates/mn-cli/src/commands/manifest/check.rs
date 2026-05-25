//! `mnm manifest check` — purely-local manifest validation.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mn_content::manifest::Manifest;

#[derive(Debug, ClapArgs)]
pub struct Args {
    pub manifest: PathBuf,
    #[arg(long)]
    pub base: Option<PathBuf>,
    #[arg(long = "sitemap")]
    pub sitemap: Vec<String>,
    #[arg(long)]
    pub strict: bool,
}

pub async fn run(args: Args) -> Result<()> {
    let body = std::fs::read_to_string(&args.manifest)
        .with_context(|| format!("read {}", args.manifest.display()))?;
    let manifest = Manifest::parse(&body).context("parse manifest")?;

    let mut issues: Vec<String> = Vec::new();

    if let Err(e) = manifest.validate() {
        issues.push(format!("schema/paths: {e}"));
    }
    let base = args.base.clone().unwrap_or_else(|| {
        args.manifest
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });
    let missing = manifest.validate_files_exist(&base);
    for m in &missing {
        issues.push(format!("missing file: {}", m.display()));
    }

    if !args.sitemap.is_empty() {
        let sitemap_urls = super::generate::load_sitemaps(&args.sitemap).await?;
        let leaves = mn_content::manifest::resolve::resolve(&manifest, &base);
        let matched = leaves
            .iter()
            .filter(|l| {
                l.published_url
                    .as_ref()
                    .map_or(false, |u| sitemap_urls.iter().any(|s| s.as_str() == u))
            })
            .count();
        eprintln!(
            "sitemap coverage: {}/{} ({}%)",
            matched,
            leaves.len(),
            if leaves.is_empty() { 100 } else { matched * 100 / leaves.len() }
        );
    }

    if issues.is_empty() {
        eprintln!("ok");
        return Ok(());
    }
    for i in &issues {
        eprintln!("- {i}");
    }
    Err(anyhow!("{} issue(s)", issues.len()))
}

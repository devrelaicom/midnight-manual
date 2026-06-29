//! `mnm manifest check` — purely-local manifest validation.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mnm_content::manifest::Manifest;

/// Arguments for `mnm manifest check`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the `hierarchy.yaml` manifest to validate.
    pub manifest: PathBuf,
    /// Override the base directory for file-existence checks.
    #[arg(long)]
    pub base: Option<PathBuf>,
    /// Sitemap URL or file path to check coverage against (repeatable).
    #[arg(long = "sitemap")]
    pub sitemap: Vec<String>,
    /// Fail on any missing-file or unmatched-URL issue.
    #[arg(long)]
    pub strict: bool,
}

/// Run `mnm manifest check`.
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
        let leaves = mnm_content::manifest::resolve::resolve(
            &manifest,
            &base,
            mnm_content::manifest::resolve::FilterRunOptions::default(),
        );
        let matched = leaves
            .iter()
            .filter(|l| {
                l.published_url
                    .as_ref()
                    .is_some_and(|u| sitemap_urls.iter().any(|s| s.as_str() == u))
            })
            .count();
        eprintln!(
            "sitemap coverage: {}/{} ({}%)",
            matched,
            leaves.len(),
            if leaves.is_empty() {
                100
            } else {
                matched * 100 / leaves.len()
            }
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

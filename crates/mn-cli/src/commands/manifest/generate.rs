//! `mnm manifest generate` — populate a hierarchy.yaml from globs +
//! optional sitemap.

use std::path::PathBuf;

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mn_content::manifest::generate::{generate, GenerateOptions};
use mn_content::manifest::sitemap;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Glob patterns to include (positional). Combined with --include.
    pub globs: Vec<String>,
    /// Glob include pattern (repeatable).
    #[arg(long = "include")]
    pub include: Vec<String>,
    /// Glob exclude pattern (repeatable).
    #[arg(long = "exclude")]
    pub exclude: Vec<String>,
    /// Base directory to resolve globs against.
    #[arg(long, default_value = ".")]
    pub base: PathBuf,
    /// Sitemap URL or file path (repeatable).
    #[arg(long = "sitemap")]
    pub sitemap: Vec<String>,
    /// Fallback URL prefix when no sitemap match.
    #[arg(long = "url-base")]
    pub url_base: Option<String>,
    /// Root node name (default: --base dir name).
    #[arg(long)]
    pub name: Option<String>,
    /// Output path (default `./hierarchy.yaml`).
    #[arg(short, long, default_value = "hierarchy.yaml")]
    pub output: PathBuf,
    /// Overwrite existing output.
    #[arg(long)]
    pub force: bool,
    /// Fail if any file is unmatched.
    #[arg(long)]
    pub strict: bool,
    /// Write unmatched-files report here.
    #[arg(long)]
    pub report: Option<PathBuf>,
    /// Disable common-URL-prefix hoist.
    #[arg(long)]
    pub no_hoist: bool,
    /// Disable directory pinning.
    #[arg(long)]
    pub no_pin_dirs: bool,
    /// Threshold for directory pinning.
    #[arg(long, default_value_t = 5)]
    pub pin_threshold: usize,
    /// Print YAML to stdout; write nothing.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: Args) -> Result<()> {
    if !args.dry_run && args.output.exists() && !args.force {
        return Err(anyhow!(
            "refusing to overwrite {}; pass --force",
            args.output.display()
        ));
    }
    let sitemap_urls = load_sitemaps(&args.sitemap).await?;
    let opts = GenerateOptions {
        base: args.base.clone(),
        include: args.globs.iter().chain(args.include.iter()).cloned().collect(),
        exclude: args.exclude.clone(),
        sitemap_urls,
        root_name: args.name.clone(),
        url_base: args.url_base.clone(),
        hoist: !args.no_hoist,
        pin_dirs: !args.no_pin_dirs,
        pin_threshold: args.pin_threshold,
    };
    let result = generate(&opts).context("generate manifest")?;

    let date = time::OffsetDateTime::now_utc().date().to_string();
    let body = mn_content::manifest::generate::emit_yaml(&result.manifest, &date)
        .context("emit yaml")?;

    if args.dry_run {
        println!("{body}");
    } else {
        std::fs::write(&args.output, body)
            .with_context(|| format!("write {}", args.output.display()))?;
        eprintln!("wrote {}", args.output.display());
    }

    let total = result.entries.len();
    let unmatched: Vec<_> = result
        .entries
        .iter()
        .filter(|e| e.matched_url.is_none())
        .collect();
    let via_slug = result
        .entries
        .iter()
        .filter(|e| e.match_reason == "Slug")
        .count();
    let via_leaf = result
        .entries
        .iter()
        .filter(|e| e.match_reason == "Leaf" || e.match_reason == "LeafWithParentDir")
        .count();
    eprintln!(
        "generated: {total} files, {matched} URLs matched (via slug: {via_slug}, leaf: {via_leaf}), {} unmatched.",
        unmatched.len(),
        matched = total - unmatched.len(),
    );

    if let Some(report) = &args.report {
        let lines: String = unmatched
            .iter()
            .map(|e| format!("{} {}\n", e.rel_path.display(), e.match_reason))
            .collect();
        std::fs::write(report, lines)
            .with_context(|| format!("write {}", report.display()))?;
    }

    if args.strict && !unmatched.is_empty() {
        return Err(anyhow!(
            "{} files unmatched (--strict)",
            unmatched.len()
        ));
    }
    Ok(())
}

pub async fn load_sitemaps(specs: &[String]) -> Result<Vec<url::Url>> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("build HTTP client")?;
    let mut out = Vec::new();
    for spec in specs {
        if sitemap::is_http(spec) {
            let url = url::Url::parse(spec).with_context(|| format!("parse {spec}"))?;
            out.extend(sitemap::load_from_url(&client, &url).await?);
        } else {
            out.extend(sitemap::load_from_path(std::path::Path::new(spec))?);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

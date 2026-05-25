//! Generate a `Manifest` from a glob set + optional sitemaps. Pure-logic
//! core of `mnm manifest generate` (§1.2 of the spec).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Manifest, ManifestNode};
use crate::frontmatter;

/// Inputs to the generator.
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    /// Filesystem root that all globs resolve against.
    pub base: PathBuf,
    /// Glob include patterns (positional GLOBS + --include flags, unioned).
    pub include: Vec<String>,
    /// Glob exclude patterns (--exclude flags).
    pub exclude: Vec<String>,
    /// Sitemap URLs (already fetched and flattened).
    pub sitemap_urls: Vec<url::Url>,
    /// Root node name (--name). None → derived from base directory.
    pub root_name: Option<String>,
    /// Fallback URL prefix when no sitemap match.
    pub url_base: Option<String>,
    /// Hoist shared published_url to common parent (--hoist).
    pub hoist: bool,
    /// Pin directories with ≥ pin_threshold matched files (--pin-dirs).
    pub pin_dirs: bool,
    /// Threshold for pin_dirs.
    pub pin_threshold: usize,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            base: PathBuf::from("."),
            include: Vec::new(),
            exclude: Vec::new(),
            sitemap_urls: Vec::new(),
            root_name: None,
            url_base: None,
            hoist: true,
            pin_dirs: true,
            pin_threshold: 5,
        }
    }
}

/// Per-file generator outcome — useful for the coverage report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateEntry {
    pub rel_path: PathBuf,
    pub matched_url: Option<String>,
    pub match_reason: String,
}

/// Final generator output.
#[derive(Debug, Clone)]
pub struct GenerateResult {
    pub manifest: Manifest,
    pub entries: Vec<GenerateEntry>,
}

pub fn generate(opts: &GenerateOptions) -> anyhow::Result<GenerateResult> {
    let files = collect_files(opts)?;
    let entries = build_entries(opts, &files)?;
    let manifest = build_manifest(opts, &entries);
    Ok(GenerateResult { manifest, entries })
}

fn collect_files(opts: &GenerateOptions) -> anyhow::Result<Vec<PathBuf>> {
    use globset::{Glob, GlobSetBuilder};

    let mut inc = GlobSetBuilder::new();
    for pat in &opts.include {
        inc.add(Glob::new(pat)?);
    }
    let inc = inc.build()?;
    let mut exc = GlobSetBuilder::new();
    for pat in &opts.exclude {
        exc.add(Glob::new(pat)?);
    }
    let exc = exc.build()?;

    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&opts.base)
        .into_iter()
        .filter_entry(|e| {
            // Only filter hidden files from subdirectories, not the base.
            if e.path() == opts.base {
                return true;
            }
            !e.file_name().to_string_lossy().starts_with('.')
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(&opts.base)?.to_path_buf();
        if !inc.is_match(&rel) {
            continue;
        }
        if exc.is_match(&rel) {
            continue;
        }
        out.push(rel);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn build_entries(
    opts: &GenerateOptions,
    files: &[PathBuf],
) -> anyhow::Result<Vec<GenerateEntry>> {
    let mut out = Vec::with_capacity(files.len());
    for rel in files {
        let body = std::fs::read_to_string(opts.base.join(rel))?;
        let split = frontmatter::split(&body);
        let slug = slug_from_frontmatter(&split.frontmatter);
        let m = super::matcher::match_file(rel, slug.as_deref(), &opts.sitemap_urls);
        let matched_url = m.url.map(|u| u.to_string()).or_else(|| {
            opts.url_base.as_ref().map(|base| {
                let stem = rel.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let base = base.trim_end_matches('/');
                format!("{base}/{stem}/")
            })
        });
        let reason = format!("{:?}", m.reason);
        out.push(GenerateEntry {
            rel_path: rel.clone(),
            matched_url,
            match_reason: reason,
        });
    }
    Ok(out)
}

fn slug_from_frontmatter(fm: &Option<serde_json::Value>) -> Option<String> {
    fm.as_ref()?.get("slug")?.as_str().map(str::to_owned)
}

// build_manifest is implemented in Task 17.
fn build_manifest(_opts: &GenerateOptions, _entries: &[GenerateEntry]) -> Manifest {
    Manifest {
        manifest_version: 1,
        root: ManifestNode {
            name: Some("PLACEHOLDER".to_owned()),
            path: None,
            file: None,
            published_url: None,
            provenance: None,
            include: Vec::new(),
            exclude: Vec::new(),
            children: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_respects_include_exclude() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("docs")).unwrap();
        std::fs::write(base.join("docs/a.md"), "").unwrap();
        std::fs::write(base.join("docs/b.md"), "").unwrap();
        std::fs::write(base.join("docs/draft.md"), "").unwrap();
        let opts = GenerateOptions {
            base: base.to_path_buf(),
            include: vec!["docs/*.md".to_owned()],
            exclude: vec!["docs/draft.md".to_owned()],
            ..Default::default()
        };
        let files = collect_files(&opts).unwrap();
        assert_eq!(
            files,
            vec![PathBuf::from("docs/a.md"), PathBuf::from("docs/b.md")]
        );
    }
}

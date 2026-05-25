//! Generate a `Manifest` from a glob set + optional sitemaps. Pure-logic
//! core of `mnm manifest generate` (§1.2 of the spec).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    /// Hoist shared `published_url` to common parent (--hoist).
    pub hoist: bool,
    /// Pin directories with ≥ `pin_threshold` matched files (--pin-dirs).
    pub pin_dirs: bool,
    /// Threshold for `pin_dirs`.
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
    /// Relative path of the file within the source root.
    pub rel_path: PathBuf,
    /// Matched sitemap URL, if any.
    pub matched_url: Option<String>,
    /// Human-readable reason for the match (or "None").
    pub match_reason: String,
}

/// Final generator output.
#[derive(Debug, Clone)]
pub struct GenerateResult {
    /// The generated manifest.
    pub manifest: Manifest,
    /// Per-file coverage entries used to produce the coverage report.
    pub entries: Vec<GenerateEntry>,
}

/// Run the generator with the given options and return the manifest + coverage entries.
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

fn build_entries(opts: &GenerateOptions, files: &[PathBuf]) -> anyhow::Result<Vec<GenerateEntry>> {
    let mut out = Vec::with_capacity(files.len());
    for rel in files {
        let body = std::fs::read_to_string(opts.base.join(rel))?;
        let split = frontmatter::split(&body);
        let slug = slug_from_frontmatter(split.frontmatter.as_ref());
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

fn slug_from_frontmatter(fm: Option<&serde_json::Value>) -> Option<String> {
    fm?.get("slug")?.as_str().map(str::to_owned)
}

fn build_manifest(opts: &GenerateOptions, entries: &[GenerateEntry]) -> Manifest {
    let root_name = opts
        .root_name
        .clone()
        .or_else(|| {
            opts.base
                .file_name()
                .and_then(|n| n.to_str())
                .map(title_case)
        })
        .unwrap_or_else(|| "Source".to_owned());

    // Group entries by their directory prefix path.
    let mut tree: TreeNode = TreeNode::group(root_name);
    for e in entries {
        tree.insert(&e.rel_path, e);
    }

    if opts.hoist {
        hoist_common_url(&mut tree);
    }
    if opts.pin_dirs {
        pin_dirs(&mut tree, &opts.base, opts.pin_threshold);
    }

    Manifest {
        manifest_version: 1,
        root: tree.into_node(),
    }
}

fn title_case(s: &str) -> String {
    s.split('-')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + c.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Intermediate tree representation. Wrapped because we want to
/// hoist URL prefixes before lowering to `ManifestNode`.
struct TreeNode {
    name: Option<String>,
    file: Option<PathBuf>,
    path: Option<PathBuf>,
    published_url: Option<String>,
    #[allow(clippy::use_self)] // `Self` is not valid in struct field type position
    children: HashMap<String, TreeNode>,
}

impl TreeNode {
    fn group(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            file: None,
            path: None,
            published_url: None,
            children: HashMap::new(),
        }
    }

    fn leaf(file: PathBuf, url: Option<String>) -> Self {
        Self {
            name: None,
            file: Some(file),
            path: None,
            published_url: url,
            children: HashMap::new(),
        }
    }

    fn insert(&mut self, rel: &Path, e: &GenerateEntry) {
        let segs: Vec<_> = rel
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => s.to_str().map(str::to_owned),
                _ => None,
            })
            .collect();
        self.insert_segs(&segs, e);
    }

    fn insert_segs(&mut self, segs: &[String], e: &GenerateEntry) {
        if segs.len() == 1 {
            let leaf = Self::leaf(e.rel_path.clone(), e.matched_url.clone());
            self.children.insert(segs[0].clone(), leaf);
            return;
        }
        let head = segs[0].clone();
        let child = self
            .children
            .entry(head.clone())
            .or_insert_with(|| Self::group(title_case(&head)));
        child.insert_segs(&segs[1..], e);
    }

    fn into_node(self) -> ManifestNode {
        let mut children: Vec<_> = self.children.into_iter().collect();
        children.sort_by(|a, b| a.0.cmp(&b.0));
        ManifestNode {
            name: self.name,
            path: self.path,
            file: self.file,
            published_url: self.published_url,
            provenance: None,
            include: Vec::new(),
            exclude: Vec::new(),
            children: children.into_iter().map(|(_, v)| v.into_node()).collect(),
        }
    }
}

/// If every leaf in a one-level subtree has a `published_url` sharing
/// the same prefix-up-to-final-segment, lift the prefix to the parent
/// node and clear the leaves.
fn hoist_common_url(node: &mut TreeNode) {
    for child in node.children.values_mut() {
        hoist_common_url(child);
    }
    if node.children.len() < 2 {
        return;
    }
    // Gather child URLs that look like `<prefix>/<segment>/`.
    let prefixes: Vec<String> = node
        .children
        .values()
        .filter_map(|c| {
            let url = c.published_url.as_ref()?;
            let trimmed = url.trim_end_matches('/');
            let cut = trimmed.rfind('/')?;
            Some(trimmed[..=cut].to_owned())
        })
        .collect();
    if prefixes.len() != node.children.len() {
        return; // some leaf has no URL — skip hoist
    }
    let first = &prefixes[0];
    if !prefixes.iter().all(|p| p == first) {
        return;
    }
    node.published_url = Some(first.clone());
    for child in node.children.values_mut() {
        child.published_url = None;
    }
}

/// When a directory group has ≥ threshold leaf-only children whose
/// per-leaf URLs have been cleared (hoisted to the parent), replace the
/// explicit children with a single `path:` directive on the group node.
#[allow(clippy::only_used_in_recursion)] // `base` is a pass-through for future use
fn pin_dirs(node: &mut TreeNode, base: &Path, threshold: usize) {
    for child in node.children.values_mut() {
        pin_dirs(child, base, threshold);
    }
    // Pin a child group if: all its children are leaves with no
    // per-leaf URL (i.e. parent declared a hoisted URL), and the count
    // hits threshold.
    let leaves_only = node
        .children
        .values()
        .all(|c| c.file.is_some() && c.children.is_empty() && c.published_url.is_none());
    if leaves_only && node.children.len() >= threshold {
        // Derive `path:` from the common parent of every child's file path.
        let common = common_parent(
            &node
                .children
                .values()
                .filter_map(|c| c.file.clone())
                .collect::<Vec<_>>(),
        );
        if let Some(parent) = common {
            node.children.clear();
            node.path = Some(parent);
        }
    }
}

fn common_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = paths.first()?.parent()?.to_path_buf();
    for p in paths {
        if p.parent() != Some(&first) {
            return None;
        }
    }
    Some(first.join(""))
}

/// Serialize a manifest to YAML with a generated-by header comment.
pub fn emit_yaml(manifest: &Manifest, date: &str) -> anyhow::Result<String> {
    let header = format!(
        "# Generated by `mnm manifest generate` on {date}.
# Schema: crates/mn-content/src/manifest/mod.rs (manifest_version = 1).
#
# Each leaf node references one source file via `file:` (relative to
# the manifest's parent dir, or `--base` at generate time). Groups use
# `name:` and `children:` to nest. `published_url:` and `provenance:`
# on any node are inherited by descendants. A node with `path:`
# auto-discovers every supported file under that directory.
"
    );
    let body = serde_yaml::to_string(manifest)?;
    Ok(format!("{header}\n{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_yaml_starts_with_header_comment() {
        let m = Manifest {
            manifest_version: 1,
            root: ManifestNode {
                name: Some("docs".to_owned()),
                path: None,
                file: None,
                published_url: None,
                provenance: None,
                include: Vec::new(),
                exclude: Vec::new(),
                children: Vec::new(),
            },
        };
        let body = emit_yaml(&m, "2026-05-25").unwrap();
        assert!(body.starts_with("# Generated by `mnm manifest generate` on 2026-05-25."));
        assert!(body.contains("manifest_version: 1"));
    }

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
        assert_eq!(files, vec![PathBuf::from("docs/a.md"), PathBuf::from("docs/b.md")]);
    }

    #[test]
    fn build_manifest_produces_a_tree_and_hoists_common_url_prefix() {
        let entries = vec![
            GenerateEntry {
                rel_path: PathBuf::from("docs/auth.md"),
                matched_url: Some("https://docs.example.com/auth/".to_owned()),
                match_reason: "Leaf".to_owned(),
            },
            GenerateEntry {
                rel_path: PathBuf::from("docs/tls.md"),
                matched_url: Some("https://docs.example.com/tls/".to_owned()),
                match_reason: "Leaf".to_owned(),
            },
        ];
        let opts = GenerateOptions {
            root_name: Some("docs".to_owned()),
            hoist: true,
            pin_dirs: false,
            ..Default::default()
        };
        let m = build_manifest(&opts, &entries);
        // Root group exists.
        assert_eq!(m.root.name.as_deref(), Some("docs"));
        assert_eq!(m.root.children.len(), 1); // "docs" subgroup
        let docs_group = &m.root.children[0];
        // Hoisted prefix sits on the docs group.
        assert_eq!(docs_group.published_url.as_deref(), Some("https://docs.example.com/"));
        // Leaves no longer declare published_url (it's inherited).
        assert!(docs_group
            .children
            .iter()
            .all(|c| c.published_url.is_none()));
    }

    #[test]
    fn pin_dirs_collapses_dense_child_set_to_path_node() {
        // Five files in one directory, all matched by the same rule, none
        // with leaf-level URL overrides (after hoist).
        let entries = (0..5)
            .map(|i| GenerateEntry {
                rel_path: PathBuf::from(format!("docs/cookbook/file-{i}.md")),
                matched_url: Some(format!("https://docs.example.com/cookbook/file-{i}/")),
                match_reason: "Leaf".to_owned(),
            })
            .collect::<Vec<_>>();
        let opts = GenerateOptions {
            root_name: Some("docs".to_owned()),
            hoist: true,
            pin_dirs: true,
            pin_threshold: 5,
            ..Default::default()
        };
        let m = build_manifest(&opts, &entries);
        // docs → cookbook
        let docs = &m.root.children[0];
        let cookbook = &docs.children[0];
        assert_eq!(cookbook.path.as_deref(), Some(Path::new("docs/cookbook/")));
        assert!(cookbook.children.is_empty());
    }
}

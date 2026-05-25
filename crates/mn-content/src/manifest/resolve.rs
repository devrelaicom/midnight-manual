//! Flatten a parsed `Manifest` into `Vec<ResolvedLeaf>` by walking the
//! tree top-down, applying `published_url` and `provenance` inheritance,
//! and (optionally — added in a later task) discovering files under
//! `path:` nodes.
//!
//! Spec: §3.1, §3.2 of docs/superpowers/specs/2026-05-25-ingest-ux-design.md

use std::path::{Path, PathBuf};

use mn_core::provenance::Provenance;
use mn_core::types::DocumentKind;
use serde::{Deserialize, Serialize};

use super::Manifest;

/// One leaf produced by walking the manifest, with inheritance applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedLeaf {
    /// Repo-relative path (relative to the resolver's `base`).
    pub rel_path: PathBuf,
    /// Document kind, derived from the file extension.
    pub kind: DocumentKind,
    /// Display name override, if any (from leaf `name:` or inherited).
    pub name: Option<String>,
    /// Final `published_url` after inheritance + leaf overrides.
    pub published_url: Option<String>,
    /// Final `source_url` after inheritance + leaf overrides.
    pub source_url: Option<String>,
    /// Provenance override merged from ancestor `provenance:` nodes.
    /// Frontmatter wins over this at ingest time; this only fills gaps.
    pub provenance_override: Provenance,
}

/// Resolve a manifest into its leaves. The `base` is informational at this
/// stage — file existence is checked by the walker, not the resolver.
///
/// At this checkpoint the resolver only handles explicit `file:` leaves
/// without any inheritance. Inheritance and `path:` discovery are added
/// in subsequent tasks.
#[must_use]
pub fn resolve(manifest: &Manifest, _base: &Path) -> Vec<ResolvedLeaf> {
    let mut out = Vec::new();
    walk(&manifest.root, &mut out);
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    out
}

fn walk(node: &super::ManifestNode, out: &mut Vec<ResolvedLeaf>) {
    if let Some(file) = &node.file {
        out.push(ResolvedLeaf {
            rel_path: file.clone(),
            kind: kind_for(file),
            name: node.name.clone(),
            published_url: node.published_url.clone(),
            source_url: None,
            provenance_override: Provenance::default(),
        });
    }
    for child in &node.children {
        walk(child, out);
    }
}

fn kind_for(path: &Path) -> DocumentKind {
    match crate::language::from_path(path) {
        Some("markdown") => DocumentKind::Markdown,
        Some(_) => DocumentKind::Code,
        None => DocumentKind::Plaintext,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn explicit_files_become_leaves() {
        let body = r"
manifest_version: 1
root:
  name: docs
  children:
    - file: a.md
    - file: dir/b.md
";
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, Path::new("."));
        let paths: Vec<_> = leaves.iter().map(|l| l.rel_path.clone()).collect();
        assert_eq!(paths, vec![PathBuf::from("a.md"), PathBuf::from("dir/b.md")]);
        assert_eq!(leaves[0].kind, DocumentKind::Markdown);
    }
}

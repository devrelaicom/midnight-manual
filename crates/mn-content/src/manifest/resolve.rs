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
use serde_json;

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
    let empty = serde_json::Map::new();
    walk(&manifest.root, None, &empty, &mut out);
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    out
}

fn walk(
    node: &super::ManifestNode,
    parent_url: Option<&str>,
    parent_prov: &serde_json::Map<String, serde_json::Value>,
    out: &mut Vec<ResolvedLeaf>,
) {
    let merged_prov = merge_prov(parent_prov, node.provenance.as_ref());

    let inherited_url = match (&node.published_url, parent_url) {
        // Leaf-level explicit empty string = clear inheritance.
        (Some(s), _) if s.is_empty() => None,
        (Some(s), _) => Some(s.as_str()),
        (None, p) => p,
    };

    if let Some(file) = &node.file {
        // If this node has its own published_url, use it directly (no composition).
        // Otherwise, compose from the inherited URL.
        let final_url = if let Some(own_url) = &node.published_url {
            if own_url.is_empty() {
                None
            } else {
                Some(own_url.clone())
            }
        } else {
            compose_url(inherited_url, file)
        };
        let prov_override = serde_json::from_value::<Provenance>(
            serde_json::Value::Object(merged_prov.clone()),
        )
        .unwrap_or_default();
        out.push(ResolvedLeaf {
            rel_path: file.clone(),
            kind: kind_for(file),
            name: node.name.clone(),
            published_url: final_url,
            source_url: None,
            provenance_override: prov_override,
        });
    }
    for child in &node.children {
        walk(child, inherited_url, &merged_prov, out);
    }
}

/// Compose the file's final `published_url` from an inherited prefix.
///
/// - When the inherited URL ends in `/`, append the file basename (no
///   extension) plus a trailing slash.
/// - When it doesn't end in `/`, treat it as a verbatim leaf override
///   (already final).
/// - When there is no inherited URL, return None.
fn compose_url(inherited: Option<&str>, file: &Path) -> Option<String> {
    let prefix = inherited?;
    if !prefix.ends_with('/') {
        return Some(prefix.to_owned());
    }
    let stem = file.file_stem()?.to_str()?;
    Some(format!("{prefix}{stem}/"))
}

fn kind_for(path: &Path) -> DocumentKind {
    match crate::language::from_path(path) {
        Some("markdown") => DocumentKind::Markdown,
        Some(_) => DocumentKind::Code,
        None => DocumentKind::Plaintext,
    }
}

/// Field-by-field merge of two provenance maps: leaf wins per field.
fn merge_prov(
    parent: &serde_json::Map<String, serde_json::Value>,
    leaf: Option<&serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = parent.clone();
    if let Some(serde_json::Value::Object(map)) = leaf {
        for (k, v) in map {
            out.insert(k.clone(), v.clone());
        }
    }
    out
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

    #[test]
    fn published_url_inherits_with_prefix_join() {
        let body = r"
manifest_version: 1
root:
  name: docs
  published_url: https://docs.example.com/
  children:
    - name: Cookbook
      published_url: https://docs.example.com/cookbook/
      children:
        - file: auth.md
        - file: tls.md
";
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, Path::new("."));
        let by_path: std::collections::HashMap<PathBuf, String> = leaves
            .iter()
            .map(|l| (l.rel_path.clone(), l.published_url.clone().unwrap()))
            .collect();
        assert_eq!(
            by_path[&PathBuf::from("auth.md")],
            "https://docs.example.com/cookbook/auth/"
        );
        assert_eq!(
            by_path[&PathBuf::from("tls.md")],
            "https://docs.example.com/cookbook/tls/"
        );
    }

    #[test]
    fn leaf_published_url_override_wins() {
        let body = r"
manifest_version: 1
root:
  published_url: https://docs.example.com/cookbook/
  children:
    - file: auth.md
      published_url: https://docs.example.com/elsewhere/sign-in/
";
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, Path::new("."));
        assert_eq!(
            leaves[0].published_url.as_deref(),
            Some("https://docs.example.com/elsewhere/sign-in/")
        );
    }

    #[test]
    fn published_url_null_at_leaf_clears_inherited() {
        // serde_yaml maps `null` to None for Option<String>; the node-level
        // None should be distinguishable from "not declared" so that leaves
        // can opt out of inheritance. We use a sentinel string for "explicit
        // null" rather than YAML null — see §3.2 of the spec.
        //
        // Implemented as: a leaf published_url of exactly "" means "clear".
        let body = r#"
manifest_version: 1
root:
  published_url: https://docs.example.com/cookbook/
  children:
    - file: internal.md
      published_url: ""
"#;
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, Path::new("."));
        assert_eq!(leaves[0].published_url, None);
    }

    #[test]
    fn provenance_merges_field_by_field_top_down() {
        let body = r#"
manifest_version: 1
root:
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
  children:
    - file: a.md
"#;
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, Path::new("."));
        let p = &leaves[0].provenance_override;
        assert_eq!(p.attribution, mn_core::provenance::Attribution::Foundation);
        assert!(p.verified);
        assert_eq!(p.verified_by.as_deref(), Some("midnight-foundation"));
    }

    #[test]
    fn leaf_provenance_overrides_ancestor_fieldwise() {
        let body = r#"
manifest_version: 1
root:
  provenance:
    attribution: foundation
    verified: true
  children:
    - file: a.md
      provenance:
        verified: false
"#;
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, Path::new("."));
        let p = &leaves[0].provenance_override;
        // Inherited attribution stays.
        assert_eq!(p.attribution, mn_core::provenance::Attribution::Foundation);
        // Leaf-level verified wins.
        assert!(!p.verified);
    }
}

//! Flatten a parsed `Manifest` into `Vec<ResolvedLeaf>`.
//!
//! Walks the tree top-down, applying `published_url` and `provenance`
//! inheritance, and discovers files under `path:` nodes.
//!
//! Spec: §3.1, §3.2 of the ingest-UX design spec.

use std::path::{Path, PathBuf};

use mnm_core::provenance::Provenance;
use mnm_core::types::DocumentKind;
use serde::{Deserialize, Serialize};
use serde_json;

use super::Manifest;

/// Run-level toggles that flow into file discovery. Defaults are hermetic:
/// gitignore off, default skip-list on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterRunOptions {
    /// When `true`, honour `.gitignore` rules during `path:` discovery.
    pub respect_gitignore: bool,
    /// When `true`, apply the built-in directory skip-list (`node_modules`,
    /// `.git`, `target`, `dist`) during `path:` discovery.
    pub default_ignore_list: bool,
}

impl FilterRunOptions {
    /// Hermetic defaults: gitignore off, default skip-list on.
    pub const HERMETIC: Self = Self {
        respect_gitignore: false,
        default_ignore_list: true,
    };
}

impl Default for FilterRunOptions {
    fn default() -> Self {
        Self::HERMETIC
    }
}

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
    /// Inherited extraction opt-out (default false).
    pub no_extract: bool,
}

/// Resolve a manifest into its leaves.
///
/// `base` is the on-disk root from which relative paths in the manifest are
/// resolved. For `path:` nodes the resolver enumerates files under
/// `base/<path>` using [`crate::ingest::filter::FileFilter`].
#[must_use]
pub fn resolve(manifest: &Manifest, base: &Path, opts: FilterRunOptions) -> Vec<ResolvedLeaf> {
    let mut out = Vec::new();
    let empty = serde_json::Map::new();
    walk(base, &manifest.root, None, &empty, false, opts, &mut out);
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    out
}

fn walk(
    base: &Path,
    node: &super::ManifestNode,
    parent_url: Option<&str>,
    parent_prov: &serde_json::Map<String, serde_json::Value>,
    parent_no_extract: bool,
    opts: FilterRunOptions,
    out: &mut Vec<ResolvedLeaf>,
) {
    let merged_prov = merge_prov(parent_prov, node.provenance.as_ref());

    let inherited_url = match (&node.published_url, parent_url) {
        // Leaf-level explicit empty string = clear inheritance.
        (Some(s), _) if s.is_empty() => None,
        (Some(s), _) => Some(s.as_str()),
        (None, p) => p,
    };

    // A node's explicit `no_extract` overrides the inherited value; otherwise
    // the ancestor's effective value flows down.
    let inherited_no_extract = node.no_extract.unwrap_or(parent_no_extract);

    if let Some(file) = &node.file {
        // If this node has its own published_url, use it directly (no composition).
        // Otherwise, compose from the inherited URL.
        let final_url = node.published_url.as_ref().map_or_else(
            || compose_url(inherited_url, file),
            |own_url| {
                if own_url.is_empty() {
                    None
                } else {
                    Some(own_url.clone())
                }
            },
        );
        let prov_override =
            serde_json::from_value::<Provenance>(serde_json::Value::Object(merged_prov.clone()))
                .unwrap_or_default();
        out.push(ResolvedLeaf {
            rel_path: file.clone(),
            kind: kind_for(file),
            name: node.name.clone(),
            published_url: final_url,
            source_url: None,
            provenance_override: prov_override,
            no_extract: inherited_no_extract,
        });
    }

    // Collect the set of files explicitly declared as `file:` children so that
    // `path:` discovery does not emit duplicates for them.
    let explicit_files: std::collections::HashSet<PathBuf> = node
        .children
        .iter()
        .filter_map(|c| c.file.clone())
        .collect();

    // If this node declares a `path:`, discover all matching files under that
    // directory and emit a leaf for each one that is not already covered by an
    // explicit `file:` child.
    if let Some(path) = &node.path {
        for rel in
            discover_under_path(base, path, &node.include, &node.exclude, &explicit_files, opts)
        {
            let url = compose_url(inherited_url, &rel);
            let prov_override = serde_json::from_value::<Provenance>(serde_json::Value::Object(
                merged_prov.clone(),
            ))
            .unwrap_or_default();
            out.push(ResolvedLeaf {
                rel_path: rel.clone(),
                kind: kind_for(&rel),
                name: None,
                published_url: url,
                source_url: None,
                provenance_override: prov_override,
                no_extract: inherited_no_extract,
            });
        }
    }

    for child in &node.children {
        walk(base, child, inherited_url, &merged_prov, inherited_no_extract, opts, out);
    }
}

// ---------------------------------------------------------------------------
// path: discovery helpers
// ---------------------------------------------------------------------------

/// Walk `base/rel_dir` recursively and return all files that pass the unified
/// [`crate::ingest::filter::FileFilter`] semantics (ingest defaults: hidden
/// files and the default skip-list excluded, unknown file kinds dropped unless
/// an explicit include glob matches).
///
/// Returned paths are relative to `base` and sorted lexicographically.
fn discover_under_path(
    base: &Path,
    rel_dir: &Path,
    include: &[String],
    exclude: &[String],
    explicit_files: &std::collections::HashSet<PathBuf>,
    opts: FilterRunOptions,
) -> Vec<PathBuf> {
    use crate::ingest::filter::{FileFilter, FilterOptions};
    let filter = FileFilter::new(FilterOptions {
        includes: include.to_vec(),
        excludes: exclude.to_vec(),
        respect_gitignore: opts.respect_gitignore,
        default_ignore_list: opts.default_ignore_list,
        skip_hidden: true,
        require_known_kind: true,
    });
    let mut out: Vec<PathBuf> = filter
        .walk_subtree(base, rel_dir)
        .into_iter()
        .filter(|rel| !explicit_files.contains(rel))
        .collect();
    out.sort();
    out
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
        // `.txt` resolves to "plaintext" but must NOT take the Code path —
        // it has no grammar and belongs with unknown files in line-window land.
        Some("plaintext") | None => DocumentKind::Plaintext,
        Some(_) => DocumentKind::Code,
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
        let leaves = resolve(&m, Path::new("."), FilterRunOptions::default());
        let paths: Vec<_> = leaves.iter().map(|l| l.rel_path.clone()).collect();
        assert_eq!(paths, vec![PathBuf::from("a.md"), PathBuf::from("dir/b.md")]);
        assert_eq!(leaves[0].kind, DocumentKind::Markdown);
    }

    #[test]
    fn txt_is_plaintext_json_is_code() {
        assert_eq!(kind_for(Path::new("notes.txt")), DocumentKind::Plaintext);
        assert_eq!(kind_for(Path::new("cfg.json")), DocumentKind::Code);
        assert_eq!(kind_for(Path::new("doc.md")), DocumentKind::Markdown);
        assert_eq!(kind_for(Path::new("lib.rs")), DocumentKind::Code);
        assert_eq!(kind_for(Path::new("mystery.zzz")), DocumentKind::Plaintext);
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
        let leaves = resolve(&m, Path::new("."), FilterRunOptions::default());
        let by_path: std::collections::HashMap<PathBuf, String> = leaves
            .iter()
            .map(|l| (l.rel_path.clone(), l.published_url.clone().unwrap()))
            .collect();
        assert_eq!(by_path[&PathBuf::from("auth.md")], "https://docs.example.com/cookbook/auth/");
        assert_eq!(by_path[&PathBuf::from("tls.md")], "https://docs.example.com/cookbook/tls/");
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
        let leaves = resolve(&m, Path::new("."), FilterRunOptions::default());
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
        let leaves = resolve(&m, Path::new("."), FilterRunOptions::default());
        assert_eq!(leaves[0].published_url, None);
    }

    #[test]
    fn provenance_merges_field_by_field_top_down() {
        let body = r"
manifest_version: 1
root:
  provenance:
    attribution: foundation
    verified: true
    verified_by: midnight-foundation
  children:
    - file: a.md
";
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, Path::new("."), FilterRunOptions::default());
        let p = &leaves[0].provenance_override;
        assert_eq!(p.attribution, mnm_core::provenance::Attribution::Foundation);
        assert!(p.verified);
        assert_eq!(p.verified_by.as_deref(), Some("midnight-foundation"));
    }

    #[test]
    fn leaf_provenance_overrides_ancestor_fieldwise() {
        let body = r"
manifest_version: 1
root:
  provenance:
    attribution: foundation
    verified: true
  children:
    - file: a.md
      provenance:
        verified: false
";
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, Path::new("."), FilterRunOptions::default());
        let p = &leaves[0].provenance_override;
        // Inherited attribution stays.
        assert_eq!(p.attribution, mnm_core::provenance::Attribution::Foundation);
        // Leaf-level verified wins.
        assert!(!p.verified);
    }

    #[test]
    fn path_node_discovers_files_under_directory() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("docs/sub")).unwrap();
        std::fs::write(base.join("docs/a.md"), "# A").unwrap();
        std::fs::write(base.join("docs/sub/b.md"), "# B").unwrap();
        std::fs::write(base.join("docs/sub/skip.draft.md"), "# draft").unwrap();
        let body = r#"
manifest_version: 1
root:
  name: docs
  path: docs/
  published_url: https://docs.example.com/
  exclude: ["**/*.draft.md"]
"#;
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, base, FilterRunOptions::default());
        let paths: Vec<_> = leaves.iter().map(|l| l.rel_path.clone()).collect();
        assert_eq!(paths, vec![PathBuf::from("docs/a.md"), PathBuf::from("docs/sub/b.md"),]);
        // Inherited URL prefix is joined with each discovered file's stem.
        assert!(leaves[0].published_url.as_deref().unwrap().ends_with("/a/"));
        assert!(leaves[1].published_url.as_deref().unwrap().ends_with("/b/"));
    }

    #[test]
    fn explicit_file_in_children_wins_over_path_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("docs")).unwrap();
        std::fs::write(base.join("docs/a.md"), "# A").unwrap();
        let body = r"
manifest_version: 1
root:
  name: docs
  path: docs/
  published_url: https://docs.example.com/
  children:
    - file: docs/a.md
      published_url: https://override.example.com/special/
";
        let m = Manifest::parse(body).unwrap();
        let leaves = resolve(&m, base, FilterRunOptions::default());
        assert_eq!(leaves.len(), 1);
        assert_eq!(
            leaves[0].published_url.as_deref(),
            Some("https://override.example.com/special/")
        );
    }

    #[test]
    fn discover_matches_unified_filter_semantics() {
        use crate::ingest::filter::{FileFilter, FilterOptions};
        let dir = tempfile::tempdir().unwrap();
        let b = dir.path();
        for p in [
            "keep.rs",
            "notes.weirdext",
            ".hidden.md",
            "node_modules/dep.js",
            "vendor/v.rs",
            "build/o.js",
            "package-lock.json",
            "package.json",
            "SECURITY.md",
        ] {
            let abs = b.join(p);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(abs, "x").unwrap();
        }
        let m = Manifest::parse("manifest_version: 1\nroot:\n  name: r\n  path: .\n").unwrap();
        let leaves = resolve(&m, b, FilterRunOptions::default());
        let mut got: Vec<String> = leaves
            .iter()
            .map(|l| l.rel_path.to_string_lossy().replace('\\', "/"))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec!["keep.rs", "package.json"],
            "drops unknown/hidden/node_modules/vendor/build/lockfile/SECURITY; keeps source + package.json"
        );

        // Cross-check: identical to FileFilter::walk_subtree with ingest defaults.
        let filter = FileFilter::new(FilterOptions {
            includes: vec![],
            excludes: vec![],
            respect_gitignore: false,
            default_ignore_list: true,
            skip_hidden: true,
            require_known_kind: true,
        });
        let mut direct: Vec<String> = filter
            .walk_subtree(b, std::path::Path::new("."))
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        direct.sort();
        assert_eq!(direct, got);
    }

    #[test]
    fn no_extract_inherits_and_leaf_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("docs")).unwrap();
        std::fs::write(base.join("docs/a.compact"), "// a").unwrap();
        std::fs::write(base.join("docs/b.compact"), "// b").unwrap();
        let yaml = r"
manifest_version: 1
root:
  path: docs
  no_extract: true
  children:
    - file: docs/a.compact
    - file: docs/b.compact
      no_extract: false
";
        let m = Manifest::parse(yaml).unwrap();
        let leaves = resolve(&m, base, FilterRunOptions::default());
        let by_path: std::collections::HashMap<PathBuf, bool> = leaves
            .iter()
            .map(|l| (l.rel_path.clone(), l.no_extract))
            .collect();
        // a inherits true from the root node
        assert!(by_path[&PathBuf::from("docs/a.compact")]);
        // b overrides to false
        assert!(!by_path[&PathBuf::from("docs/b.compact")]);
    }
}

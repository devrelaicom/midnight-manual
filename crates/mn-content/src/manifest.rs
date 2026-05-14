//! `hierarchy.yaml` manifest loader (FR-017, FR-050).
//!
//! Manifests let the maintainer override the on-disk directory tree with an
//! explicit hierarchy. Files referenced by the manifest inherit its
//! `published_url` / `provenance` / `name` overrides; files NOT referenced
//! fall back to directory-tree inference unless `--strict-manifest` is set.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Top-level manifest shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema sentinel. Always `1` in v1.
    pub manifest_version: u32,
    /// The root of the hierarchy.
    pub root: ManifestNode,
}

/// One node in the manifest hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestNode {
    /// Display name (for groups).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional directory pin — files under this path inherit this node's metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Optional file (mutually exclusive with `path` / `children` in practice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    /// Optional published URL for this node and its descendants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_url: Option<String>,
    /// Optional provenance override merged with frontmatter at ingest time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<serde_json::Value>,
    /// Child nodes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ManifestNode>,
}

impl Manifest {
    /// Parse a manifest from a YAML body.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Parse`] on malformed YAML or
    /// [`ManifestError::SchemaVersionMismatch`] when the sentinel disagrees.
    pub fn parse(body: &str) -> Result<Self, ManifestError> {
        let m: Self =
            serde_yaml::from_str(body).map_err(|e| ManifestError::Parse(e.to_string()))?;
        if m.manifest_version != 1 {
            return Err(ManifestError::SchemaVersionMismatch {
                found: m.manifest_version,
                expected: 1,
            });
        }
        Ok(m)
    }

    /// Run pre-flight validations that don't require touching the filesystem:
    /// - every `file:` / `path:` is relative, contains no `..` components, and
    ///   contains no absolute / scheme prefix (path-traversal guard).
    /// - no file appears as a `file:` under more than one parent (EC-14).
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::DuplicateFile`] when a path is referenced twice
    /// or [`ManifestError::UnsafePath`] when a path escapes the source root.
    pub fn validate(&self) -> Result<(), ManifestError> {
        check_paths(&self.root)?;
        let mut seen: HashSet<PathBuf> = HashSet::new();
        check_unique(&self.root, &mut seen)
    }

    /// Validate that every file: reference exists on the filesystem
    /// (EC-13). Returns the list of missing files; empty on success.
    ///
    /// # Errors
    ///
    /// Never returns an error; the missing-files set is part of the success
    /// value so callers can render a structured report.
    pub fn validate_files_exist(&self, base: &Path) -> Vec<PathBuf> {
        let mut missing = Vec::new();
        gather_missing(&self.root, base, &mut missing);
        missing
    }
}

/// Reject any `file:` / `path:` that is absolute or contains parent-dir
/// (`..`) components. Such paths could traverse outside the source root and
/// pull arbitrary filesystem content into the ingest (e.g. `/etc/passwd`).
fn check_paths(node: &ManifestNode) -> Result<(), ManifestError> {
    for p in [node.path.as_ref(), node.file.as_ref()]
        .into_iter()
        .flatten()
    {
        if !is_safe_relative(p) {
            return Err(ManifestError::UnsafePath(p.clone()));
        }
    }
    for child in &node.children {
        check_paths(child)?;
    }
    Ok(())
}

fn is_safe_relative(p: &Path) -> bool {
    use std::path::Component;
    if p.is_absolute() {
        return false;
    }
    for c in p.components() {
        match c {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => return false,
            Component::Normal(_) | Component::CurDir => {}
        }
    }
    true
}

fn check_unique(node: &ManifestNode, seen: &mut HashSet<PathBuf>) -> Result<(), ManifestError> {
    if let Some(p) = &node.file {
        if !seen.insert(p.clone()) {
            return Err(ManifestError::DuplicateFile(p.clone()));
        }
    }
    for child in &node.children {
        check_unique(child, seen)?;
    }
    Ok(())
}

fn gather_missing(node: &ManifestNode, base: &Path, missing: &mut Vec<PathBuf>) {
    if let Some(file) = &node.file {
        let abs = base.join(file);
        if !abs.exists() {
            missing.push(file.clone());
        }
    }
    for child in &node.children {
        gather_missing(child, base, missing);
    }
}

/// All the ways manifest loading can fail.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// Malformed YAML body.
    #[error("failed to parse manifest YAML: {0}")]
    Parse(String),
    /// `manifest_version` disagrees with the supported version.
    #[error("manifest_version={found} (expected {expected})")]
    SchemaVersionMismatch {
        /// The version that was found.
        found: u32,
        /// The version that was expected.
        expected: u32,
    },
    /// A file path was referenced under more than one parent (EC-14).
    #[error("file referenced under multiple parents: {0}")]
    DuplicateFile(PathBuf),
    /// A `file:` or `path:` value is absolute, contains `..` components, or
    /// otherwise escapes the source-root sandbox.
    #[error("unsafe path {0:?}: must be a relative path with no `..` components")]
    UnsafePath(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_manifest_round_trips() {
        let body = r"
manifest_version: 1
root:
  name: docs
  children:
    - name: getting-started
      path: getting-started/
      published_url: https://docs.midnight.network/getting-started/
      children:
        - file: getting-started/quickstart.mdx
          name: Quickstart
";
        let m = Manifest::parse(body).unwrap();
        assert_eq!(m.manifest_version, 1);
        assert_eq!(m.root.name.as_deref(), Some("docs"));
        assert_eq!(m.root.children.len(), 1);
        assert_eq!(
            m.root.children[0].children[0].file,
            Some(PathBuf::from("getting-started/quickstart.mdx"))
        );
    }

    #[test]
    fn rejects_unknown_version() {
        let body = "manifest_version: 99\nroot:\n  name: docs\n";
        let err = Manifest::parse(body).unwrap_err();
        assert!(matches!(err, ManifestError::SchemaVersionMismatch { found: 99, expected: 1 }));
    }

    #[test]
    fn validate_rejects_duplicate_file_reference() {
        let body = r"
manifest_version: 1
root:
  name: docs
  children:
    - file: foo.md
    - file: foo.md
";
        let m = Manifest::parse(body).unwrap();
        let err = m.validate().unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateFile(_)));
    }

    #[test]
    fn validate_files_exist_reports_missing() {
        let body = r"
manifest_version: 1
root:
  children:
    - file: definitely/missing.md
";
        let m = Manifest::parse(body).unwrap();
        let missing = m.validate_files_exist(Path::new("."));
        assert_eq!(missing, vec![PathBuf::from("definitely/missing.md")]);
    }

    #[test]
    fn validate_rejects_absolute_file_path() {
        let body = r"
manifest_version: 1
root:
  children:
    - file: /etc/passwd
";
        let m = Manifest::parse(body).unwrap();
        let err = m.validate().unwrap_err();
        assert!(matches!(err, ManifestError::UnsafePath(_)));
    }

    #[test]
    fn validate_rejects_parent_dir_components() {
        let body = r"
manifest_version: 1
root:
  children:
    - file: ../../../etc/shadow
";
        let m = Manifest::parse(body).unwrap();
        let err = m.validate().unwrap_err();
        assert!(matches!(err, ManifestError::UnsafePath(_)));
    }

    #[test]
    fn validate_rejects_unsafe_path_field() {
        let body = r"
manifest_version: 1
root:
  path: ../escape
  children: []
";
        let m = Manifest::parse(body).unwrap();
        let err = m.validate().unwrap_err();
        assert!(matches!(err, ManifestError::UnsafePath(_)));
    }
}

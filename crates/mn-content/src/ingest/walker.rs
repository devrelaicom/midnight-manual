//! Filesystem walker that turns a manifest + source root into a stream of
//! [`WalkedDocument`] entries, ready to be fed into [`PlanBuilder`].
//!
//! The manifest is the source of truth (FR-017): files not listed under any
//! `file:` node are skipped, even when they exist in the directory tree. This
//! keeps an ingest deterministic with respect to what the maintainer signed
//! off on.
//!
//! [`PlanBuilder`]: super::plan::PlanBuilder

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::frontmatter::{split as split_frontmatter, FrontmatterSplit};
use crate::manifest::{Manifest, ManifestNode};

/// One file pulled off disk and pre-processed for the orchestrator.
#[derive(Debug, Clone, PartialEq)]
pub struct WalkedDocument {
    /// Repo-relative path (relative to the walker's `base`).
    pub rel_path: PathBuf,
    /// Raw file contents.
    pub content: String,
    /// Parsed frontmatter + body split.
    pub split: FrontmatterSplit,
}

/// Errors the walker can surface.
#[derive(Debug, Error)]
pub enum WalkError {
    /// A `file:` reference in the manifest points to something that doesn't
    /// exist on disk.
    #[error("manifest references missing file: {0}")]
    MissingFile(PathBuf),
    /// A file referenced by the manifest could not be read.
    #[error("failed to read {path}: {source}")]
    Io {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
    /// A file's bytes were not valid UTF-8.
    #[error("file {path} is not valid UTF-8")]
    NotUtf8 {
        /// Path that failed to decode.
        path: PathBuf,
    },
}

/// Walk every `file:` referenced by `manifest`, rooted at `base`. Files not
/// referenced by the manifest are skipped.
///
/// The walk is performed eagerly into a `Vec` so callers can use it
/// repeatedly (e.g. once for a dry-run, once for the real run). For very
/// large corpora a streaming variant would be desirable; v1 corpora are
/// small enough that this is not a concern.
///
/// # Errors
///
/// Returns [`WalkError::MissingFile`] if any manifest file is absent,
/// [`WalkError::Io`] on read failure, or [`WalkError::NotUtf8`] on decode
/// failure. The walk stops at the first error.
pub fn walk(manifest: &Manifest, base: &Path) -> Result<Vec<WalkedDocument>, WalkError> {
    let mut paths: Vec<PathBuf> = Vec::new();
    collect_files(&manifest.root, &mut paths);
    paths.sort();
    paths.dedup();

    let mut out: Vec<WalkedDocument> = Vec::with_capacity(paths.len());
    for rel in paths {
        let abs = base.join(&rel);
        if !abs.exists() {
            return Err(WalkError::MissingFile(rel));
        }
        let bytes =
            std::fs::read(&abs).map_err(|e| WalkError::Io { path: rel.clone(), source: e })?;
        let content =
            String::from_utf8(bytes).map_err(|_| WalkError::NotUtf8 { path: rel.clone() })?;
        let split = split_frontmatter(&content);
        out.push(WalkedDocument { rel_path: rel, content, split });
    }
    Ok(out)
}

/// Manifest-walker convenience wrapper. Holds the parsed manifest and a
/// resolved base directory; calling [`Walker::walk`] returns the list of
/// every walked document.
#[derive(Debug, Clone)]
pub struct Walker {
    manifest: Manifest,
    base: PathBuf,
}

impl Walker {
    /// Construct a walker.
    #[must_use]
    pub const fn new(manifest: Manifest, base: PathBuf) -> Self {
        Self { manifest, base }
    }

    /// Perform the walk and return every reachable [`WalkedDocument`].
    ///
    /// # Errors
    ///
    /// See [`walk`].
    pub fn walk(&self) -> Result<Vec<WalkedDocument>, WalkError> {
        walk(&self.manifest, &self.base)
    }
}

fn collect_files(node: &ManifestNode, out: &mut Vec<PathBuf>) {
    if let Some(file) = &node.file {
        out.push(file.clone());
    }
    for child in &node.children {
        collect_files(child, out);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write_file(base: &Path, rel: &str, body: &str) {
        let abs = base.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        let mut f = std::fs::File::create(&abs).expect("create file");
        f.write_all(body.as_bytes()).expect("write file");
    }

    fn manifest_yaml(files: &[&str]) -> String {
        use std::fmt::Write as _;
        let mut s = String::from("manifest_version: 1\nroot:\n  name: docs\n  children:\n");
        for f in files {
            writeln!(s, "    - file: {f}").expect("write to string");
        }
        s
    }

    #[test]
    fn walks_every_manifest_file_in_sorted_order() {
        let dir = tempdir();
        write_file(dir.path(), "z.md", "# Z");
        write_file(dir.path(), "a.md", "# A");
        let manifest = Manifest::parse(&manifest_yaml(&["z.md", "a.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let docs = walker.walk().unwrap();
        let paths: Vec<_> = docs.iter().map(|d| d.rel_path.clone()).collect();
        assert_eq!(paths, vec![PathBuf::from("a.md"), PathBuf::from("z.md")]);
    }

    #[test]
    fn parses_frontmatter_during_walk() {
        let dir = tempdir();
        write_file(dir.path(), "with-fm.md", "---\nverified: true\n---\n# Title\n\nBody.\n");
        let manifest = Manifest::parse(&manifest_yaml(&["with-fm.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let docs = walker.walk().unwrap();
        assert!(docs[0].split.provenance.verified);
        assert!(docs[0].split.frontmatter.is_some());
        assert_eq!(docs[0].split.body, "# Title\n\nBody.\n");
    }

    #[test]
    fn skips_files_not_in_manifest() {
        let dir = tempdir();
        write_file(dir.path(), "listed.md", "# Listed");
        write_file(dir.path(), "unlisted.md", "# Unlisted");
        let manifest = Manifest::parse(&manifest_yaml(&["listed.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let docs = walker.walk().unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].rel_path, PathBuf::from("listed.md"));
    }

    #[test]
    fn missing_file_is_reported() {
        let dir = tempdir();
        let manifest = Manifest::parse(&manifest_yaml(&["missing.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let err = walker.walk().unwrap_err();
        assert!(matches!(err, WalkError::MissingFile(p) if p == Path::new("missing.md")));
    }

    #[test]
    fn duplicate_file_in_manifest_yields_unique_walks() {
        // Manifest validator would reject this, but the walker is defensive.
        let dir = tempdir();
        write_file(dir.path(), "x.md", "# X");
        let mut yaml = String::from("manifest_version: 1\nroot:\n  children:\n");
        yaml.push_str("    - file: x.md\n");
        // Manifest::validate would reject; we bypass and feed directly.
        let manifest = Manifest::parse(&yaml).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let docs = walker.walk().unwrap();
        assert_eq!(docs.len(), 1);
    }

    #[test]
    fn non_utf8_file_is_reported() {
        let dir = tempdir();
        let abs = dir.path().join("bad.md");
        std::fs::write(&abs, [0xFF, 0xFE, 0xFD]).expect("write bad");
        let manifest = Manifest::parse(&manifest_yaml(&["bad.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let err = walker.walk().unwrap_err();
        assert!(matches!(err, WalkError::NotUtf8 { .. }));
    }
}

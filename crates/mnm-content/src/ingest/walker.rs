//! Filesystem walker that turns a manifest + source root into a stream of
//! [`WalkedDocument`] entries, ready to be fed into [`PlanBuilder`].
//!
//! The manifest is the source of truth (FR-017): files not listed under any
//! `file:` node are skipped, even when they exist in the directory tree. This
//! keeps an ingest deterministic with respect to what the maintainer signed
//! off on.
//!
//! Resilient ingestion (EC-52): a referenced file that is oversized, binary,
//! not valid UTF-8, or not a regular file (e.g. a directory) is *skipped with a
//! recorded reason* rather than aborting the whole walk. The skips are returned
//! alongside the documents in [`WalkOutcome`] so callers can warn the operator
//! and surface a count.
//!
//! [`PlanBuilder`]: super::plan::PlanBuilder

use std::path::{Path, PathBuf};

use mnm_core::types::DocumentKind;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::chunk::{DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_LINE_BYTES};
use crate::frontmatter::{
    passthrough as passthrough_frontmatter, split as split_frontmatter, FrontmatterSplit,
};
use crate::manifest::resolve::FilterRunOptions;
use crate::manifest::Manifest;

/// How many leading bytes the binary sniffer inspects for a NUL byte. Mirrors
/// git's heuristic: a NUL in the first 8 KiB marks the file as binary.
const BINARY_SNIFF_LEN: usize = 8192;

/// One file pulled off disk and pre-processed for the orchestrator.
#[derive(Debug, Clone, PartialEq)]
pub struct WalkedDocument {
    /// Repo-relative path (relative to the walker's `base`).
    pub rel_path: PathBuf,
    /// Raw file contents.
    pub content: String,
    /// Parsed frontmatter + body split.
    pub split: FrontmatterSplit,
    /// Resolver-derived inheritance — fed to `PlanBuilder` so it can be
    /// threaded to the upload layer.
    pub resolved: crate::manifest::resolve::ResolvedLeaf,
    /// Filesystem modification timestamp captured at walk time.
    /// `None` if the OS could not supply `mtime` for the file.
    pub source_modified_at: Option<OffsetDateTime>,
}

/// Errors the walker can surface. Per-file content problems (oversize, binary,
/// non-UTF-8) are *not* errors — they are recorded as [`SkippedFile`] entries.
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
}

/// Why a referenced file was skipped (EC-52).
///
/// Produced by the walker (non-regular / oversize / binary / non-UTF-8) and by
/// the planner ([`EmptyNoChunks`](SkipReason::EmptyNoChunks), for a new document
/// whose body chunks to nothing). Skips never abort a walk or a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipReason {
    /// The path resolved to something other than a regular file (e.g. a
    /// directory mistakenly listed under a `file:` node, or a special file).
    NotRegularFile,
    /// File size exceeded the configured `max_file_bytes` ceiling.
    TooLarge {
        /// Actual file size in bytes.
        size: u64,
        /// The ceiling the file exceeded.
        limit: u64,
    },
    /// A single line exceeded the configured `max_line_bytes` ceiling. Marks
    /// machine-generated data (chain-specs, minified/serialized blobs) that is
    /// low-value to search and can form an un-splittable oversize chunk.
    LongLine {
        /// Longest line found, in bytes.
        longest: usize,
        /// The ceiling it exceeded.
        limit: usize,
    },
    /// A NUL byte was found in the leading bytes (binary sniff over the first 8 KiB).
    Binary,
    /// The bytes could not be decoded as UTF-8.
    NotUtf8,
    /// The file is empty / whitespace-only / frontmatter-only — its body
    /// produced zero chunks, so there is nothing searchable to persist. The
    /// server refuses such documents (an unsearchable document), so the planner
    /// drops them rather than uploading a doc that can never persist (issue:
    /// finalize completeness mismatch).
    EmptyNoChunks,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRegularFile => write!(f, "not a regular file (e.g. a directory)"),
            Self::TooLarge { size, limit } => {
                write!(f, "exceeds max file size ({size} > {limit} bytes)")
            }
            Self::LongLine { longest, limit } => {
                write!(f, "has a line exceeding max line size ({longest} > {limit} bytes)")
            }
            Self::Binary => write!(f, "looks binary (NUL byte in first {BINARY_SNIFF_LEN} bytes)"),
            Self::NotUtf8 => write!(f, "not valid UTF-8"),
            Self::EmptyNoChunks => write!(f, "empty / no searchable content (produced 0 chunks)"),
        }
    }
}

/// One file that was skipped, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedFile {
    /// Repo-relative path of the skipped file.
    pub rel_path: PathBuf,
    /// Why it was skipped.
    pub reason: SkipReason,
}

/// The result of a walk: documents that were ingested plus the files skipped
/// (EC-52). `skipped` is empty on the happy path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WalkOutcome {
    /// Files successfully read and pre-processed.
    pub documents: Vec<WalkedDocument>,
    /// Files skipped (non-regular / oversize / binary / non-UTF-8), in walk order.
    pub skipped: Vec<SkippedFile>,
}

/// True iff the first [`BINARY_SNIFF_LEN`] bytes contain a NUL byte. A NUL is
/// itself valid UTF-8 (`U+0000`), so this sniff catches binaries that would
/// otherwise decode into a string of control characters.
///
/// This is only the fast/typical path (matching git's first-8-KiB heuristic):
/// the real backstop is the full-buffer `String::from_utf8` check below, which
/// rejects any non-UTF-8 file regardless of where the offending byte sits. So a
/// binary whose first NUL is past 8 KiB is still skipped — just classified as
/// `NotUtf8` rather than `Binary`. Don't "fix" the `.take()` thinking it's a bug.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SNIFF_LEN).any(|&b| b == 0)
}

/// Longest line by byte length (newline `\n` as the separator; the trailing
/// `\r` of a CRLF is counted, which is immaterial at the threshold). Returns 0
/// for empty input.
///
/// Used by the walker to skip machine-generated files whose longest line
/// exceeds `max_line_bytes` (chain-specs, minified/serialized blobs). Kept
/// dependency-free on purpose — no `memchr`.
fn longest_line_bytes(bytes: &[u8]) -> usize {
    bytes
        .split(|&b| b == b'\n')
        .map(<[u8]>::len)
        .max()
        .unwrap_or(0)
}

/// Walk every `file:` referenced by `manifest`, rooted at `base`. Files not
/// referenced by the manifest are skipped.
///
/// Paths that are not regular files (e.g. a directory), files larger than
/// `max_file_bytes`, files containing a single line longer than
/// `max_line_bytes` (machine-generated data), files that sniff as binary, and
/// files that are not valid UTF-8 are recorded in [`WalkOutcome::skipped`]
/// rather than aborting the walk (EC-52). A `max_line_bytes` of `0` disables
/// the long-line check.
///
/// The walk is performed eagerly into a `Vec` so callers can use it
/// repeatedly (e.g. once for a dry-run, once for the real run). For very
/// large corpora a streaming variant would be desirable; v1 corpora are
/// small enough that this is not a concern.
///
/// # Errors
///
/// Returns [`WalkError::MissingFile`] if any manifest file is absent, or
/// [`WalkError::Io`] on read/metadata failure. The walk stops at the first
/// such error.
pub fn walk(
    manifest: &Manifest,
    base: &Path,
    max_file_bytes: u64,
    max_line_bytes: usize,
    opts: FilterRunOptions,
) -> Result<WalkOutcome, WalkError> {
    let leaves = crate::manifest::resolve::resolve(manifest, base, opts);
    let mut documents: Vec<WalkedDocument> = Vec::with_capacity(leaves.len());
    let mut skipped: Vec<SkippedFile> = Vec::new();
    for leaf in leaves {
        let abs = base.join(&leaf.rel_path);
        let meta = match std::fs::metadata(&abs) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(WalkError::MissingFile(leaf.rel_path));
            }
            Err(source) => {
                return Err(WalkError::Io { path: leaf.rel_path, source });
            }
        };
        // `metadata` follows symlinks, so this also accepts a symlink to a
        // regular file. A directory (or socket/FIFO) listed under `file:` is
        // skipped rather than read — reading it would otherwise blow up the
        // whole walk with an EISDIR-style `Io` error.
        if !meta.is_file() {
            skipped.push(SkippedFile {
                rel_path: leaf.rel_path,
                reason: SkipReason::NotRegularFile,
            });
            continue;
        }
        if meta.len() > max_file_bytes {
            skipped.push(SkippedFile {
                rel_path: leaf.rel_path,
                reason: SkipReason::TooLarge {
                    size: meta.len(),
                    limit: max_file_bytes,
                },
            });
            continue;
        }
        let bytes = std::fs::read(&abs).map_err(|e| WalkError::Io {
            path: leaf.rel_path.clone(),
            source: e,
        })?;
        if looks_binary(&bytes) {
            skipped.push(SkippedFile {
                rel_path: leaf.rel_path,
                reason: SkipReason::Binary,
            });
            continue;
        }
        if max_line_bytes != 0 {
            let longest = longest_line_bytes(&bytes);
            if longest > max_line_bytes {
                skipped.push(SkippedFile {
                    rel_path: leaf.rel_path,
                    reason: SkipReason::LongLine { longest, limit: max_line_bytes },
                });
                continue;
            }
        }
        let Ok(content) = String::from_utf8(bytes) else {
            skipped.push(SkippedFile {
                rel_path: leaf.rel_path,
                reason: SkipReason::NotUtf8,
            });
            continue;
        };
        // Frontmatter extraction is a Markdown-only convention (FR-017). Running
        // it on YAML/code files silently swallows a leading `---`-delimited block
        // into the frontmatter column instead of the searchable body — routine
        // content loss on multi-document or `---`-prefixed YAML (issue #161). Gate
        // the split on the resolved `DocumentKind` and pass every other kind
        // through verbatim.
        let split = if leaf.kind == DocumentKind::Markdown {
            split_frontmatter(&content)
        } else {
            passthrough_frontmatter(&content)
        };
        let modified = meta.modified().ok().map(OffsetDateTime::from);
        documents.push(WalkedDocument {
            rel_path: leaf.rel_path.clone(),
            content,
            split,
            resolved: leaf,
            source_modified_at: modified,
        });
    }
    Ok(WalkOutcome { documents, skipped })
}

/// Manifest-walker convenience wrapper. Holds the parsed manifest, a resolved
/// base directory, and the per-file size ceiling; calling [`Walker::walk`]
/// returns the [`WalkOutcome`].
#[derive(Debug, Clone)]
pub struct Walker {
    manifest: Manifest,
    base: PathBuf,
    max_file_bytes: u64,
    max_line_bytes: usize,
    filter_opts: FilterRunOptions,
}

impl Walker {
    /// Construct a walker with the default size ceiling
    /// ([`DEFAULT_MAX_FILE_BYTES`]).
    #[must_use]
    pub const fn new(manifest: Manifest, base: PathBuf) -> Self {
        Self {
            manifest,
            base,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            filter_opts: FilterRunOptions::HERMETIC,
        }
    }

    /// Override the per-file size ceiling. Files larger than this are skipped.
    #[must_use]
    pub const fn with_max_file_bytes(mut self, max_file_bytes: u64) -> Self {
        self.max_file_bytes = max_file_bytes;
        self
    }

    /// Override the per-file longest-line ceiling (bytes). Files containing a
    /// line longer than this are skipped. `0` disables the check.
    #[must_use]
    pub const fn with_max_line_bytes(mut self, max_line_bytes: usize) -> Self {
        self.max_line_bytes = max_line_bytes;
        self
    }

    /// Override the filter run options used during `path:` discovery.
    #[must_use]
    pub const fn with_filter_options(mut self, opts: FilterRunOptions) -> Self {
        self.filter_opts = opts;
        self
    }

    /// Return the current filter run options.
    #[must_use]
    pub const fn filter_options(&self) -> FilterRunOptions {
        self.filter_opts
    }

    /// Perform the walk and return the [`WalkOutcome`].
    ///
    /// # Errors
    ///
    /// See [`walk`].
    pub fn walk(&self) -> Result<WalkOutcome, WalkError> {
        walk(
            &self.manifest,
            &self.base,
            self.max_file_bytes,
            self.max_line_bytes,
            self.filter_opts,
        )
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
        let docs = walker.walk().unwrap().documents;
        let paths: Vec<_> = docs.iter().map(|d| d.rel_path.clone()).collect();
        assert_eq!(paths, vec![PathBuf::from("a.md"), PathBuf::from("z.md")]);
    }

    #[test]
    fn parses_frontmatter_during_walk() {
        let dir = tempdir();
        write_file(dir.path(), "with-fm.md", "---\nverified: true\n---\n# Title\n\nBody.\n");
        let manifest = Manifest::parse(&manifest_yaml(&["with-fm.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let docs = walker.walk().unwrap().documents;
        assert!(docs[0].split.provenance.verified);
        assert!(docs[0].split.frontmatter.is_some());
        assert_eq!(docs[0].split.body, "# Title\n\nBody.\n");
    }

    #[test]
    fn multi_document_yaml_keeps_both_documents_in_body() {
        // A two-document YAML manifest (the common Kubernetes/CI case). The
        // leading `Deployment` doc must NOT be swallowed into frontmatter — both
        // documents have to survive into the chunked body (issue #161). `.yaml`
        // resolves to DocumentKind::Code, so frontmatter extraction is skipped.
        let dir = tempdir();
        let yaml =
            "---\napiVersion: apps/v1\nkind: Deployment\n---\napiVersion: v1\nkind: Service\n";
        write_file(dir.path(), "manifests.yaml", yaml);
        let manifest = Manifest::parse(&manifest_yaml(&["manifests.yaml"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let outcome = walker.walk().unwrap();
        assert!(outcome.skipped.is_empty());
        let doc = &outcome.documents[0];
        assert_eq!(doc.resolved.kind, DocumentKind::Code);
        // Nothing was diverted into the frontmatter JSONB column.
        assert!(doc.split.frontmatter.is_none());
        // The entire file (both documents) is the searchable body.
        assert_eq!(doc.split.body, yaml);
        assert!(doc.split.body.contains("kind: Deployment"));
        assert!(doc.split.body.contains("kind: Service"));
    }

    #[test]
    fn single_fenced_yaml_body_is_not_emptied() {
        // A single `---`-fenced YAML file: `split` would strip it to an empty
        // body, dropping the whole file as EmptyNoChunks. Passthrough keeps the
        // content so it still has a searchable body (issue #161).
        let dir = tempdir();
        write_file(dir.path(), "config.yml", "---\nfoo: bar\n---\n");
        let manifest = Manifest::parse(&manifest_yaml(&["config.yml"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let outcome = walker.walk().unwrap();
        let doc = &outcome.documents[0];
        assert_eq!(doc.resolved.kind, DocumentKind::Code);
        assert!(doc.split.frontmatter.is_none());
        assert_eq!(doc.split.body, "---\nfoo: bar\n---\n");
        assert!(!doc.split.body.is_empty());
    }

    #[test]
    fn markdown_frontmatter_is_still_extracted_after_gating() {
        // The Markdown path is unchanged: a `.md` file's frontmatter is still
        // parsed into provenance and the JSONB column, with the body stripped.
        let dir = tempdir();
        write_file(dir.path(), "doc.md", "---\nverified: true\n---\n# Title\n\nBody.\n");
        let manifest = Manifest::parse(&manifest_yaml(&["doc.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let doc = &walker.walk().unwrap().documents[0];
        assert_eq!(doc.resolved.kind, DocumentKind::Markdown);
        assert!(doc.split.frontmatter.is_some());
        assert!(doc.split.provenance.verified);
        assert_eq!(doc.split.body, "# Title\n\nBody.\n");
    }

    #[test]
    fn skips_files_not_in_manifest() {
        let dir = tempdir();
        write_file(dir.path(), "listed.md", "# Listed");
        write_file(dir.path(), "unlisted.md", "# Unlisted");
        let manifest = Manifest::parse(&manifest_yaml(&["listed.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let docs = walker.walk().unwrap().documents;
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
        let docs = walker.walk().unwrap().documents;
        assert_eq!(docs.len(), 1);
    }

    #[test]
    fn non_utf8_file_is_skipped_not_fatal() {
        let dir = tempdir();
        // 0xFF 0xFE 0xFD is not a valid UTF-8 sequence and contains no NUL,
        // so it trips the UTF-8 decode rather than the binary sniffer.
        let abs = dir.path().join("bad.md");
        std::fs::write(&abs, [0xFF, 0xFE, 0xFD]).expect("write bad");
        let manifest = Manifest::parse(&manifest_yaml(&["bad.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let outcome = walker.walk().unwrap();
        assert!(outcome.documents.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].rel_path, PathBuf::from("bad.md"));
        assert_eq!(outcome.skipped[0].reason, SkipReason::NotUtf8);
    }

    #[test]
    fn directory_referenced_as_file_is_skipped_not_fatal() {
        let dir = tempdir();
        // A directory listed under a `file:` node: reading it would otherwise
        // abort the walk with an EISDIR-style Io error. It must be skipped.
        std::fs::create_dir(dir.path().join("a-dir")).expect("create dir");
        write_file(dir.path(), "real.md", "# Real");
        let manifest = Manifest::parse(&manifest_yaml(&["a-dir", "real.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let outcome = walker.walk().unwrap();
        let kept: Vec<_> = outcome
            .documents
            .iter()
            .map(|d| d.rel_path.clone())
            .collect();
        assert_eq!(kept, vec![PathBuf::from("real.md")]);
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].rel_path, PathBuf::from("a-dir"));
        assert_eq!(outcome.skipped[0].reason, SkipReason::NotRegularFile);
    }

    #[test]
    fn binary_file_is_skipped() {
        let dir = tempdir();
        // Embedded NUL byte → binary sniffer skips it (even though the rest
        // decodes as UTF-8).
        std::fs::write(dir.path().join("logo.md"), b"PNG\x00\x01\x02binary").expect("write bin");
        let manifest = Manifest::parse(&manifest_yaml(&["logo.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let outcome = walker.walk().unwrap();
        assert!(outcome.documents.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].reason, SkipReason::Binary);
    }

    #[test]
    fn oversize_file_is_skipped_and_smaller_ones_kept() {
        let dir = tempdir();
        write_file(dir.path(), "big.md", "# Big\n\nthis body is comfortably over the tiny limit\n");
        write_file(dir.path(), "small.md", "# S");
        let manifest = Manifest::parse(&manifest_yaml(&["big.md", "small.md"])).unwrap();
        // Limit small enough that big.md exceeds it but small.md fits.
        let walker = Walker::new(manifest, dir.path().to_path_buf()).with_max_file_bytes(8);
        let outcome = walker.walk().unwrap();
        let kept: Vec<_> = outcome
            .documents
            .iter()
            .map(|d| d.rel_path.clone())
            .collect();
        assert_eq!(kept, vec![PathBuf::from("small.md")]);
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].rel_path, PathBuf::from("big.md"));
        assert!(matches!(outcome.skipped[0].reason, SkipReason::TooLarge { limit: 8, .. }));
    }

    #[test]
    fn default_walk_keeps_normal_files_with_no_skips() {
        let dir = tempdir();
        write_file(dir.path(), "a.md", "# A\n\nbody");
        let manifest = Manifest::parse(&manifest_yaml(&["a.md"])).unwrap();
        let outcome = Walker::new(manifest, dir.path().to_path_buf())
            .walk()
            .unwrap();
        assert_eq!(outcome.documents.len(), 1);
        assert!(outcome.skipped.is_empty());
    }

    #[test]
    fn long_line_file_is_skipped_and_normal_ones_kept() {
        let dir = tempdir();
        // `long.md`'s second line is 30 bytes — over the 20-byte limit. `ok.md`
        // has only short lines, so it is kept.
        write_file(dir.path(), "long.md", "short\n123456789012345678901234567890\n");
        write_file(dir.path(), "ok.md", "short\nlines\nhere\n");
        let manifest = Manifest::parse(&manifest_yaml(&["long.md", "ok.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf()).with_max_line_bytes(20);
        let outcome = walker.walk().unwrap();
        let kept: Vec<_> = outcome
            .documents
            .iter()
            .map(|d| d.rel_path.clone())
            .collect();
        assert_eq!(kept, vec![PathBuf::from("ok.md")]);
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].rel_path, PathBuf::from("long.md"));
        assert!(matches!(
            outcome.skipped[0].reason,
            SkipReason::LongLine { longest: 30, limit: 20 }
        ));
    }

    #[test]
    fn long_line_boundary_is_strictly_greater() {
        // A line of exactly `limit` bytes is KEPT; `limit + 1` is skipped.
        let dir = tempdir();
        // Single lines with no trailing newline: byte length == content length.
        write_file(dir.path(), "exact.md", &"a".repeat(20));
        write_file(dir.path(), "over.md", &"a".repeat(21));
        let manifest = Manifest::parse(&manifest_yaml(&["exact.md", "over.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf()).with_max_line_bytes(20);
        let outcome = walker.walk().unwrap();
        let kept: Vec<_> = outcome
            .documents
            .iter()
            .map(|d| d.rel_path.clone())
            .collect();
        assert_eq!(kept, vec![PathBuf::from("exact.md")]);
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].rel_path, PathBuf::from("over.md"));
        assert!(matches!(
            outcome.skipped[0].reason,
            SkipReason::LongLine { longest: 21, limit: 20 }
        ));
    }

    #[test]
    fn max_line_bytes_zero_disables_the_check() {
        let dir = tempdir();
        write_file(dir.path(), "huge-line.md", &"a".repeat(100_000));
        let manifest = Manifest::parse(&manifest_yaml(&["huge-line.md"])).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf()).with_max_line_bytes(0);
        let outcome = walker.walk().unwrap();
        assert_eq!(outcome.documents.len(), 1);
        assert_eq!(outcome.documents[0].rel_path, PathBuf::from("huge-line.md"));
        assert!(outcome.skipped.is_empty());
    }

    #[test]
    fn default_walk_keeps_normal_multiline_file_with_no_long_line_skip() {
        let dir = tempdir();
        write_file(
            dir.path(),
            "a.md",
            "# Title\n\nSome ordinary prose across several lines.\n\nMore text.\n",
        );
        let manifest = Manifest::parse(&manifest_yaml(&["a.md"])).unwrap();
        let outcome = Walker::new(manifest, dir.path().to_path_buf())
            .walk()
            .unwrap();
        assert_eq!(outcome.documents.len(), 1);
        assert!(!outcome
            .skipped
            .iter()
            .any(|s| matches!(s.reason, SkipReason::LongLine { .. })));
    }

    #[test]
    fn chain_spec_like_single_long_hex_line_is_dropped_at_default() {
        let dir = tempdir();
        // One machine-generated hex line: "0x" + 6000 * "ab" = 12002 bytes,
        // comfortably over the 10,000-byte default ceiling.
        let hex_line = format!("0x{}", "ab".repeat(6000));
        assert_eq!(hex_line.len(), 12_002);
        write_file(dir.path(), "chain-spec.json", &hex_line);
        let manifest = Manifest::parse(&manifest_yaml(&["chain-spec.json"])).unwrap();
        // Default Walker → default threshold (DEFAULT_MAX_LINE_BYTES = 10_000).
        let outcome = Walker::new(manifest, dir.path().to_path_buf())
            .walk()
            .unwrap();
        assert!(outcome.documents.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].rel_path, PathBuf::from("chain-spec.json"));
        assert!(matches!(
            outcome.skipped[0].reason,
            SkipReason::LongLine { longest: 12_002, limit: 10_000 }
        ));
    }

    #[test]
    fn long_line_skip_reason_display() {
        let reason = SkipReason::LongLine { longest: 12_002, limit: 10_000 };
        let msg = reason.to_string();
        assert!(msg.contains("has a line exceeding max line size"));
        assert!(msg.contains("12002"));
        assert!(msg.contains("10000"));
    }

    #[test]
    fn crlf_carriage_return_counts_toward_line_length() {
        // The `\r` of a CRLF is counted (documented on `longest_line_bytes`):
        // a 20-byte content line plus its CR is 21 bytes, tripping a 20-byte
        // limit. Were the CR not counted it would sit exactly at the limit and
        // be kept — so this pins the documented behavior.
        let dir = tempdir();
        write_file(dir.path(), "crlf.md", "aaaaaaaaaaaaaaaaaaaa\r\n");
        let manifest = Manifest::parse(&manifest_yaml(&["crlf.md"])).unwrap();
        let outcome = Walker::new(manifest, dir.path().to_path_buf())
            .with_max_line_bytes(20)
            .walk()
            .unwrap();
        assert_eq!(outcome.skipped.len(), 1);
        assert!(matches!(
            outcome.skipped[0].reason,
            SkipReason::LongLine { longest: 21, limit: 20 }
        ));
    }

    #[test]
    fn empty_and_newline_only_files_are_not_long_line_skipped() {
        // `slice::split` always yields at least one element, so an empty or
        // newline-only file must measure a longest line of 0 and never trip the
        // check — even at the tightest possible limit.
        let dir = tempdir();
        write_file(dir.path(), "empty.md", "");
        write_file(dir.path(), "newlines.md", "\n\n\n");
        let manifest = Manifest::parse(&manifest_yaml(&["empty.md", "newlines.md"])).unwrap();
        let outcome = Walker::new(manifest, dir.path().to_path_buf())
            .with_max_line_bytes(1)
            .walk()
            .unwrap();
        assert!(!outcome
            .skipped
            .iter()
            .any(|s| matches!(s.reason, SkipReason::LongLine { .. })));
    }

    #[test]
    fn walker_captures_source_modified_at() {
        let dir = tempdir();
        write_file(dir.path(), "a.md", "# A");
        let body = "manifest_version: 1\nroot:\n  children:\n    - file: a.md\n";
        let manifest = Manifest::parse(body).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let docs = walker.walk().unwrap().documents;
        assert!(docs[0].source_modified_at.is_some());
    }

    #[test]
    fn walker_emits_resolved_leaves_including_path_discovery() {
        let dir = tempdir();
        write_file(dir.path(), "docs/a.md", "# A");
        write_file(dir.path(), "docs/sub/b.md", "# B");
        write_file(dir.path(), "outside.md", "# not in manifest");
        let body = r"
manifest_version: 1
root:
  name: docs
  path: docs/
";
        let manifest = Manifest::parse(body).unwrap();
        let walker = Walker::new(manifest, dir.path().to_path_buf());
        let docs = walker.walk().unwrap().documents;
        let paths: Vec<_> = docs.iter().map(|d| d.rel_path.clone()).collect();
        assert_eq!(paths, vec![PathBuf::from("docs/a.md"), PathBuf::from("docs/sub/b.md")]);
    }

    #[test]
    fn walker_defaults_to_hermetic_filter_options() {
        use crate::manifest::resolve::FilterRunOptions;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "x").unwrap();
        let m = Manifest::parse("manifest_version: 1\nroot:\n  name: r\n  path: .\n").unwrap();
        let w = Walker::new(m, dir.path().to_path_buf());
        assert_eq!(w.filter_options(), FilterRunOptions::HERMETIC);
        let w2 = w.with_filter_options(FilterRunOptions {
            respect_gitignore: true,
            default_ignore_list: false,
        });
        assert!(w2.filter_options().respect_gitignore);
    }
}

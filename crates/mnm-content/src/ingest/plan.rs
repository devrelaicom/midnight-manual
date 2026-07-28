//! [`IngestPlan`] and [`PlanBuilder`] — the core orchestrator types.

#![allow(clippy::derive_partial_eq_without_eq)] // serde_json::Value in PlannedDocument blocks Eq

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use mnm_core::provenance::Provenance;
use mnm_core::types::{DocumentKind, SourceKind};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::chunk::ChunkerConfig;
use crate::content_hash::{chunk_hash, document_hash, hash_input};
use crate::frontmatter::FrontmatterSplit;
use crate::ingest::walker::{SkipReason, SkippedFile};

/// One pre-existing document carried over from the prior active source version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorDocument {
    /// Repo-relative path used as the carry-forward join key.
    pub path: PathBuf,
    /// SHA-256 over the normalized prior content (`document_hash`).
    pub content_hash: String,
    /// The prior `document.id`, so the caller can re-link chunks without
    /// another DB round-trip.
    pub document_id: Uuid,
}

/// Snapshot of the prior active source version's document inventory.
///
/// The orchestrator never touches the database. The caller queries
/// `mnm-store::entities::document` for the active version's documents and
/// hands the snapshot in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorState {
    /// Every document in the prior active version, keyed by repo-relative path.
    pub documents: Vec<PriorDocument>,
}

/// A single chunk produced by the chunker, ready for insertion.
///
/// The `embedding` column is filled later in the pipeline (Phase 9b
/// embed-failed lifecycle); chunks land in `embed_failed` state if the
/// embedder rejects them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedChunk {
    /// Verbatim chunk text.
    pub content: String,
    /// Ancestor heading path from the Markdown chunker. Empty for code /
    /// plaintext or for pre-heading content.
    pub heading_path: Vec<String>,
    /// Structured code-symbol path. Empty for markdown/plaintext.
    pub symbol_path: Vec<mnm_core::types::SymbolSegment>,
    /// 0-indexed position among the document's chunks.
    pub chunk_index: u32,
    /// Total chunks in the document, for `total_chunks` column.
    pub total_chunks: u32,
    /// Byte offset of the chunk's first character, in post-processed text
    /// coordinates (offsets into the preprocessed body, not the original file).
    pub start_byte: usize,
    /// Byte offset just past the chunk's last character, in post-processed
    /// text coordinates (offsets into the preprocessed body, not the original
    /// file).
    pub end_byte: usize,
    /// SHA-256 over the chunk's verbatim content.
    pub content_hash: String,
    /// Token count of this chunk (computed in Task 12; defaults to 0 until then).
    pub token_count: u32,
}

/// A document that the orchestrator decided is brand-new or content-changed.
///
/// The chunker has already run. The caller inserts `document` + every chunk
/// in `chunks` under the new `source_version`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedDocument {
    /// Repo-relative source path.
    pub path: PathBuf,
    /// Document kind discriminator.
    pub kind: DocumentKind,
    /// SHA-256 over the normalized content (`document_hash`).
    pub content_hash: String,
    /// Verbatim parsed frontmatter (YAML → JSON), if any.
    pub frontmatter: Option<serde_json::Value>,
    /// Materialized provenance extracted from frontmatter.
    pub provenance: Provenance,
    /// Character count of the source content.
    pub char_count: usize,
    /// Chunks emitted by the chunker.
    pub chunks: Vec<PlannedChunk>,
    /// Final published URL after manifest inheritance (None when neither
    /// the manifest nor a sitemap matched).
    pub published_url: Option<String>,
    /// URL to the source of the document (e.g. a github blob URL).
    pub source_url: Option<String>,
    /// Filesystem-derived modification timestamp at walk time.
    pub source_modified_at: Option<time::OffsetDateTime>,
    /// IANA-like language identifier from `mnm_content::language`.
    pub language: Option<String>,
    /// Token count of the document body (computed once at chunk time and
    /// summed across chunks; landed in Task 12).
    pub token_count: u32,
    /// Detected package membership for code documents (rust/npm). None otherwise.
    #[serde(default)]
    pub package: Option<mnm_core::types::PackageRef>,
}

/// A document whose `content_hash` matched the prior active version. The
/// caller re-links its chunks rather than running the chunker again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedDocument {
    /// Repo-relative source path.
    pub path: PathBuf,
    /// SHA-256 over the normalized content (`document_hash`).
    pub content_hash: String,
    /// The prior `document.id`, copied through from [`PriorDocument`].
    pub prior_document_id: Uuid,
}

/// A document that existed in the prior active version but is absent from
/// the current walk. Not re-linked; effectively dropped by the new version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedDocument {
    /// Repo-relative source path.
    pub path: PathBuf,
    /// The prior `document.id`, copied through from [`PriorDocument`].
    pub prior_document_id: Uuid,
}

/// Aggregate counters for telemetry and operator-facing dry-run output.
///
/// Feeds the `IngestComplete` telemetry event in Phase 9b.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestStats {
    /// Count of `new_documents` (chunker ran).
    pub documents_added: usize,
    /// Count of `carried_documents` (chunks re-linked from prior).
    pub documents_carried: usize,
    /// Count of `deleted_documents` (present in prior, absent here).
    pub documents_deleted: usize,
    /// Total chunks emitted across every `new_documents` entry.
    pub chunks_emitted: usize,
}

/// The fully-constructed plan, ready to be applied by the caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestPlan {
    /// Owning source slug (matches `source.slug`).
    pub source_slug: String,
    /// Source kind, for the `source` row when this is a new source.
    pub source_kind: SourceKind,
    /// CLI-supplied revision label (FR-019). Free-form; typically a git SHA.
    pub target_revision: String,
    /// Documents to insert in full.
    pub new_documents: Vec<PlannedDocument>,
    /// Documents to re-link from the prior active version.
    pub carried_documents: Vec<CarriedDocument>,
    /// Documents present in the prior active version but missing from this walk.
    pub deleted_documents: Vec<DeletedDocument>,
    /// New documents dropped because they chunked to nothing (empty /
    /// whitespace-only / frontmatter-only). They are NOT part of the version —
    /// the server refuses unsearchable documents — and are surfaced here (and in
    /// the CLI report's skipped files) rather than silently lost.
    pub skipped_empty: Vec<SkippedFile>,
    /// Aggregate counters.
    pub stats: IngestStats,
}

/// Errors the orchestrator can surface while building a plan.
#[derive(Debug, Error)]
pub enum IngestError {
    /// Two walked documents shared the same repo-relative path. The manifest
    /// validator should have caught this earlier, but the orchestrator guards
    /// against it as a defence-in-depth measure.
    #[error("duplicate path fed to PlanBuilder: {0}")]
    DuplicatePath(PathBuf),

    /// A chunker (or a dependency such as `pulldown-cmark`) panicked while
    /// chunking this file. Only surfaced under strict mode; the default path
    /// degrades the file to the line-window fallback with a warning and the run
    /// continues (issue #121).
    #[error("chunker panicked on {disp}: {reason}", disp = .path.display())]
    ChunkPanic {
        /// Repo-relative path of the file whose chunking panicked.
        path: PathBuf,
        /// The caught panic's message.
        reason: String,
    },
}

/// A bundle of all data for a single walked document, passed to
/// [`PlanBuilder::add_walked_document`].
///
/// Using a struct keeps the method signature stable as more fields land
/// (e.g. token counts, language overrides) without breaking every call site.
pub struct WalkContext<'a> {
    /// Repo-relative path (the join key used by the plan).
    pub path: PathBuf,
    /// Document kind discriminator.
    pub kind: DocumentKind,
    /// Raw file contents.
    pub content: &'a str,
    /// Parsed frontmatter + body split.
    pub split: &'a FrontmatterSplit,
    /// Resolver-derived inheritance from the manifest.
    pub resolved: &'a crate::manifest::resolve::ResolvedLeaf,
    /// Machine-extracted version provenance (computed by the caller; spec §1).
    pub extracted: Provenance,
    /// Filesystem modification timestamp at walk time (`None` if the OS
    /// could not supply `mtime`).
    pub source_modified_at: Option<time::OffsetDateTime>,
    /// Pre-detected package membership (computed by the caller; the planner does no filesystem I/O).
    pub package: Option<mnm_core::types::PackageRef>,
}

/// Stateful builder for [`IngestPlan`]. One instance per ingest run.
#[derive(Debug)]
pub struct PlanBuilder {
    source_slug: String,
    source_kind: SourceKind,
    target_revision: String,
    chunker_config: ChunkerConfig,
    /// When `true`, a caught chunker panic fails the run instead of degrading
    /// the offending file to the line-window fallback (issue #121).
    strict: bool,
    prior_by_path: HashMap<PathBuf, PriorDocument>,
    seen_paths: HashSet<PathBuf>,
    new_documents: Vec<PlannedDocument>,
    carried_documents: Vec<CarriedDocument>,
    skipped_empty: Vec<SkippedFile>,
}

impl PlanBuilder {
    /// Construct a builder from the prior active version's document inventory.
    #[must_use]
    pub fn new(
        source_slug: impl Into<String>,
        source_kind: SourceKind,
        target_revision: impl Into<String>,
        prior_state: PriorState,
    ) -> Self {
        let prior_by_path = prior_state
            .documents
            .into_iter()
            .map(|d| (d.path.clone(), d))
            .collect();
        Self {
            source_slug: source_slug.into(),
            source_kind,
            target_revision: target_revision.into(),
            chunker_config: ChunkerConfig::default(),
            strict: false,
            prior_by_path,
            seen_paths: HashSet::new(),
            new_documents: Vec::new(),
            carried_documents: Vec::new(),
            skipped_empty: Vec::new(),
        }
    }

    /// Override the chunker configuration (defaults to [`ChunkerConfig::default`]).
    #[must_use]
    pub const fn with_chunker_config(mut self, cfg: ChunkerConfig) -> Self {
        self.chunker_config = cfg;
        self
    }

    /// Enable strict mode (defaults to off). In strict mode a chunker panic on
    /// any file fails the whole run via [`IngestError::ChunkPanic`] instead of
    /// degrading that one file to the line-window fallback with a warning.
    /// Mirrors the `--strict` flag on `mnm ingest plan` / `mnm ingest run`.
    #[must_use]
    pub const fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Feed one walked document into the plan.
    ///
    /// Computes the document hash, compares against [`PriorState`], and either
    /// carries the document forward or runs the chunker and records a new
    /// [`PlannedDocument`].
    ///
    /// # Errors
    ///
    /// Returns [`IngestError::DuplicatePath`] if the path inside `walked` was
    /// already fed in.
    pub fn add_walked_document(&mut self, walked: &WalkContext<'_>) -> Result<(), IngestError> {
        if !self.seen_paths.insert(walked.path.clone()) {
            return Err(IngestError::DuplicatePath(walked.path.clone()));
        }

        let hash = document_hash(&hash_input(walked.split.raw.as_deref(), &walked.split.body));

        if let Some(prior) = self.prior_by_path.get(&walked.path) {
            if prior.content_hash == hash {
                self.carried_documents.push(CarriedDocument {
                    path: walked.path.clone(),
                    content_hash: hash,
                    prior_document_id: prior.document_id,
                });
                return Ok(());
            }
        }

        let ext = walked
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let (chunks, panicked) = crate::chunk::chunk_document_guarded(
            walked.kind,
            ext,
            &walked.split.body,
            &self.chunker_config,
        );
        if let Some(reason) = panicked {
            // A chunker (or a dependency it calls) panicked rather than returning
            // Err. The boundary already recovered the body via the line-window
            // fallback (issue #121). Under strict mode that is a run failure;
            // by default it degrades this one file and the run continues.
            if self.strict {
                return Err(IngestError::ChunkPanic {
                    path: walked.path.clone(),
                    reason,
                });
            }
            tracing::warn!(
                path = %walked.path.display(),
                reason = %reason,
                "chunker panicked; file degraded to line-window fallback (run continues)",
            );
        }

        // A new document that chunks to nothing (empty / whitespace-only /
        // frontmatter-only body) has no searchable content. The server refuses
        // to persist chunk-less documents, so counting it as a planned document
        // would inflate the finalize `expected_document_total` and abort the
        // whole run. Drop it here and record the skip so it is reported, not
        // silently lost. (Carried documents are handled above and never reach
        // this point, so their legitimately-empty chunk lists are unaffected.)
        if chunks.is_empty() {
            self.skipped_empty.push(SkippedFile {
                rel_path: walked.path.clone(),
                reason: SkipReason::EmptyNoChunks,
            });
            return Ok(());
        }

        let total = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
        let planned_chunks: Vec<PlannedChunk> = chunks
            .into_iter()
            .map(|c| {
                let content_hash = chunk_hash(&c.content);
                let token_count = crate::tokens::count(&c.content);
                PlannedChunk {
                    content: c.content,
                    heading_path: c.heading_path,
                    symbol_path: c.symbol_path,
                    chunk_index: c.chunk_index,
                    total_chunks: total,
                    start_byte: c.start_byte,
                    end_byte: c.end_byte,
                    content_hash,
                    token_count,
                }
            })
            .collect();

        let doc_tokens: u32 = planned_chunks.iter().map(|c| c.token_count).sum();

        self.new_documents.push(PlannedDocument {
            path: walked.path.clone(),
            kind: walked.kind,
            content_hash: hash,
            frontmatter: walked.split.frontmatter.clone(),
            provenance: merge_provenance(
                &walked.split.provenance,
                &walked.extracted,
                &walked.resolved.provenance_override,
            ),
            char_count: walked.content.chars().count(),
            chunks: planned_chunks,
            published_url: walked.resolved.published_url.clone(),
            source_url: walked.resolved.source_url.clone(),
            source_modified_at: walked.source_modified_at,
            language: crate::language::from_path(&walked.resolved.rel_path).map(str::to_owned),
            token_count: doc_tokens,
            package: walked.package.clone(),
        });
        Ok(())
    }
}

/// Most-specific wins: frontmatter > extracted > manifest ancestor (spec §1.2).
///
/// Exposed so the CLI's carried-document path can compute provenance identically
/// to the new-document path (same precedence), avoiding any drift between the
/// two upload builders.
#[must_use]
pub fn merge_provenance(
    frontmatter: &Provenance,
    extracted: &Provenance,
    ancestor: &Provenance,
) -> Provenance {
    overlay(frontmatter, &overlay(extracted, ancestor))
}

/// `top` wins per-field over `base`; non-empty lists replace wholesale.
fn overlay(top: &Provenance, base: &Provenance) -> Provenance {
    let default = Provenance::default();
    let mut out = base.clone();
    if top.attribution != default.attribution {
        out.attribution = top.attribution;
    }
    if top.verified != default.verified {
        out.verified = top.verified;
    }
    if top.verified_by != default.verified_by {
        out.verified_by.clone_from(&top.verified_by);
    }
    if top.verified_at != default.verified_at {
        out.verified_at = top.verified_at;
    }
    if top.verification_notes != default.verification_notes {
        out.verification_notes.clone_from(&top.verification_notes);
    }
    if !top.language_targets.is_empty() {
        out.language_targets.clone_from(&top.language_targets);
    }
    if !top.sdk_dependencies.is_empty() {
        out.sdk_dependencies.clone_from(&top.sdk_dependencies);
    }
    if top.deprecation != default.deprecation {
        out.deprecation.clone_from(&top.deprecation);
    }
    if !top.tags.is_empty() {
        out.tags.clone_from(&top.tags);
    }
    if top.content_type != default.content_type {
        out.content_type = top.content_type;
    }
    out
}

impl PlanBuilder {
    /// Consume the builder and produce the final [`IngestPlan`].
    ///
    /// The three document vectors are sorted lexicographically by `path` so
    /// review diffs stay stable across runs. Stats are populated from the
    /// final vectors.
    #[must_use]
    pub fn finalize(mut self) -> IngestPlan {
        self.new_documents.sort_by(|a, b| a.path.cmp(&b.path));
        self.carried_documents.sort_by(|a, b| a.path.cmp(&b.path));

        let mut deleted_documents: Vec<DeletedDocument> = self
            .prior_by_path
            .into_iter()
            .filter_map(|(path, prior)| {
                if self.seen_paths.contains(&path) {
                    None
                } else {
                    Some(DeletedDocument {
                        path,
                        prior_document_id: prior.document_id,
                    })
                }
            })
            .collect();
        deleted_documents.sort_by(|a, b| a.path.cmp(&b.path));

        let chunks_emitted: usize = self.new_documents.iter().map(|d| d.chunks.len()).sum();
        let stats = IngestStats {
            documents_added: self.new_documents.len(),
            documents_carried: self.carried_documents.len(),
            documents_deleted: deleted_documents.len(),
            chunks_emitted,
        };

        IngestPlan {
            source_slug: self.source_slug,
            source_kind: self.source_kind,
            target_revision: self.target_revision,
            new_documents: self.new_documents,
            carried_documents: self.carried_documents,
            deleted_documents,
            skipped_empty: self.skipped_empty,
            stats,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use mnm_core::provenance::Attribution;

    use super::*;
    use crate::frontmatter::split as split_frontmatter;

    /// Build a `PriorDocument` whose `content_hash` matches what
    /// `add_walked_document` computes for the same `content` via `feed()` —
    /// i.e. `hash_input(split.raw, split.body)`, not a raw `document_hash`.
    /// Mirrors the real carry-forward pipeline (the prior hash always comes
    /// from a previous run of the same `hash_input`-based computation).
    fn prior(path: &str, content: &str, id: Uuid) -> PriorDocument {
        let split = split_frontmatter(content);
        PriorDocument {
            path: PathBuf::from(path),
            content_hash: document_hash(&hash_input(split.raw.as_deref(), &split.body)),
            document_id: id,
        }
    }

    fn feed(builder: &mut PlanBuilder, path: &str, content: &str) {
        let split = split_frontmatter(content);
        let leaf = crate::manifest::resolve::ResolvedLeaf {
            rel_path: PathBuf::from(path),
            kind: DocumentKind::Markdown,
            name: None,
            published_url: None,
            source_url: None,
            provenance_override: Provenance::default(),
            no_extract: false,
        };
        let ctx = WalkContext {
            path: PathBuf::from(path),
            kind: DocumentKind::Markdown,
            content,
            split: &split,
            resolved: &leaf,
            extracted: Provenance::default(),
            source_modified_at: None,
            package: None,
        };
        builder
            .add_walked_document(&ctx)
            .expect("add_walked_document");
    }

    fn empty_builder() -> PlanBuilder {
        PlanBuilder::new("docs", SourceKind::DocsSite, "rev-1", PriorState::default())
    }

    /// Builder with a coalescing-suppressing chunker config: a tiny budget
    /// whose 90% target is smaller than any two adjacent test sections
    /// combined, so each heading stays its own chunk (and sections stay under
    /// `max_tokens` so no window split kicks in either). Used by multi-chunk
    /// bookkeeping tests that would otherwise see tiny sections greedily
    /// pack into a single chunk under the default budget.
    fn per_section_builder() -> PlanBuilder {
        empty_builder().with_chunker_config(ChunkerConfig {
            max_tokens: 28,
            ..ChunkerConfig::default()
        })
    }

    #[test]
    fn new_document_runs_chunker_and_lands_in_new_vector() {
        let mut b = empty_builder();
        feed(&mut b, "intro.md", "# Hello\n\nWelcome to the docs.");
        let plan = b.finalize();
        assert_eq!(plan.new_documents.len(), 1);
        assert!(plan.carried_documents.is_empty());
        assert!(plan.deleted_documents.is_empty());
        assert_eq!(plan.new_documents[0].path, PathBuf::from("intro.md"));
        assert_eq!(plan.new_documents[0].chunks.len(), 1);
        assert_eq!(plan.new_documents[0].chunks[0].total_chunks, 1);
        assert_eq!(plan.new_documents[0].chunks[0].chunk_index, 0);
        assert_eq!(plan.stats.documents_added, 1);
        assert_eq!(plan.stats.chunks_emitted, 1);
    }

    #[test]
    fn whitespace_only_document_is_skipped_not_planned() {
        // A body that chunks to nothing must NOT become a planned document: the
        // server refuses chunk-less docs, so counting it would inflate the
        // finalize `expected_document_total` and abort the run. It is recorded
        // as a skip instead.
        let mut b = empty_builder();
        feed(&mut b, "blank.md", "   \n\n\t  ");
        let plan = b.finalize();

        assert!(plan.new_documents.is_empty(), "empty doc must not be planned");
        assert_eq!(plan.stats.documents_added, 0);
        assert_eq!(plan.stats.chunks_emitted, 0);
        assert_eq!(plan.skipped_empty.len(), 1);
        assert_eq!(plan.skipped_empty[0].rel_path, PathBuf::from("blank.md"));
        assert_eq!(plan.skipped_empty[0].reason, SkipReason::EmptyNoChunks);
    }

    #[test]
    fn frontmatter_only_document_is_skipped() {
        // The real-world trigger (e.g. midnames-docs preprod/index.mdx): valid
        // frontmatter, no body → zero chunks → skipped, not a new document.
        let mut b = empty_builder();
        feed(&mut b, "fm-only.md", "---\ntitle: Reference\nhidden: true\n---\n");
        let plan = b.finalize();

        assert!(plan.new_documents.is_empty());
        assert_eq!(plan.skipped_empty.len(), 1);
        assert_eq!(plan.skipped_empty[0].reason, SkipReason::EmptyNoChunks);
    }

    #[test]
    fn one_empty_among_several_only_drops_the_empty() {
        // Mixed batch: two real docs + one empty. Only the empty is skipped; the
        // others plan normally and the counts line up (this is exactly what
        // makes `expected_document_total` correct again).
        let mut b = empty_builder();
        feed(&mut b, "a.md", "# A\n\nreal body a");
        feed(&mut b, "empty.md", "\n");
        feed(&mut b, "b.md", "# B\n\nreal body b");
        let plan = b.finalize();

        assert_eq!(plan.stats.documents_added, 2);
        let paths: Vec<_> = plan.new_documents.iter().map(|d| &d.path).collect();
        assert_eq!(paths, vec![&PathBuf::from("a.md"), &PathBuf::from("b.md")]);
        assert_eq!(plan.skipped_empty.len(), 1);
        assert_eq!(plan.skipped_empty[0].rel_path, PathBuf::from("empty.md"));
    }

    #[test]
    fn prior_document_emptied_is_skipped_not_deleted() {
        // A doc that had content before but is now empty: its hash differs from
        // the prior (so it is NOT carried), it chunks to nothing (so it is NOT
        // new), and — because the walk DID see its path — it must NOT be
        // classified as a deletion either. It lands only in `skipped_empty`.
        // Pins the `seen_paths`-vs-deleted interaction in `finalize()`.
        let prior_id = Uuid::new_v4();
        let prior_state = PriorState {
            documents: vec![prior("page.md", "# Was real\n\nhad a body", prior_id)],
        };
        let mut b = PlanBuilder::new("docs", SourceKind::DocsSite, "rev-2", prior_state);
        feed(&mut b, "page.md", "   \n");
        let plan = b.finalize();

        assert!(plan.new_documents.is_empty());
        assert!(plan.carried_documents.is_empty());
        assert!(
            plan.deleted_documents.is_empty(),
            "a walked (seen) path must not be classified as deleted",
        );
        assert_eq!(plan.skipped_empty.len(), 1);
        assert_eq!(plan.skipped_empty[0].rel_path, PathBuf::from("page.md"));
        assert_eq!(plan.skipped_empty[0].reason, SkipReason::EmptyNoChunks);
    }

    #[test]
    fn matching_hash_lands_in_carried_vector() {
        let content = "# Hello\n\nWelcome to the docs.";
        let prior_id = Uuid::new_v4();
        let prior_state = PriorState {
            documents: vec![prior("intro.md", content, prior_id)],
        };
        let mut b = PlanBuilder::new("docs", SourceKind::DocsSite, "rev-2", prior_state);
        feed(&mut b, "intro.md", content);
        let plan = b.finalize();
        assert!(plan.new_documents.is_empty());
        assert_eq!(plan.carried_documents.len(), 1);
        assert_eq!(plan.carried_documents[0].prior_document_id, prior_id);
        assert_eq!(plan.stats.documents_carried, 1);
        assert_eq!(plan.stats.chunks_emitted, 0);
    }

    #[test]
    fn changed_hash_lands_in_new_vector_even_when_path_matches() {
        let prior_id = Uuid::new_v4();
        let prior_state = PriorState {
            documents: vec![prior("intro.md", "# Old", prior_id)],
        };
        let mut b = PlanBuilder::new("docs", SourceKind::DocsSite, "rev-2", prior_state);
        feed(&mut b, "intro.md", "# New\n\nNew body.");
        let plan = b.finalize();
        assert_eq!(plan.new_documents.len(), 1);
        assert!(plan.carried_documents.is_empty());
        assert!(plan.deleted_documents.is_empty());
    }

    #[test]
    fn prior_path_absent_from_walk_lands_in_deleted_vector() {
        let prior_id = Uuid::new_v4();
        let prior_state = PriorState {
            documents: vec![prior("gone.md", "# Bye", prior_id)],
        };
        let b = PlanBuilder::new("docs", SourceKind::DocsSite, "rev-2", prior_state);
        let plan = b.finalize();
        assert_eq!(plan.deleted_documents.len(), 1);
        assert_eq!(plan.deleted_documents[0].path, PathBuf::from("gone.md"));
        assert_eq!(plan.deleted_documents[0].prior_document_id, prior_id);
        assert_eq!(plan.stats.documents_deleted, 1);
    }

    #[test]
    fn all_three_vectors_are_sorted_lexicographically() {
        // Prior has b.md, d.md, a.md; walk re-sees a.md (carried), b.md changed (new),
        // adds z.md (new), drops d.md (deleted).
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let id_d = Uuid::new_v4();
        let prior_state = PriorState {
            documents: vec![
                prior("b.md", "# Old B", id_b),
                prior("d.md", "# Old D", id_d),
                prior("a.md", "# A", id_a),
            ],
        };
        let mut b = PlanBuilder::new("docs", SourceKind::DocsSite, "rev-2", prior_state);
        // Fed in arbitrary (non-lexicographic) order:
        feed(&mut b, "z.md", "# Z");
        feed(&mut b, "a.md", "# A");
        feed(&mut b, "b.md", "# New B\n\nbody");
        let plan = b.finalize();

        let new_paths: Vec<_> = plan.new_documents.iter().map(|d| &d.path).collect();
        assert_eq!(new_paths, vec![&PathBuf::from("b.md"), &PathBuf::from("z.md")]);
        let carried_paths: Vec<_> = plan.carried_documents.iter().map(|d| &d.path).collect();
        assert_eq!(carried_paths, vec![&PathBuf::from("a.md")]);
        let deleted_paths: Vec<_> = plan.deleted_documents.iter().map(|d| &d.path).collect();
        assert_eq!(deleted_paths, vec![&PathBuf::from("d.md")]);
    }

    #[test]
    fn frontmatter_is_preserved_on_planned_documents() {
        let content =
            "---\nverified: true\nverified_by: midnight-foundation\nattribution: foundation\n---\n# Title\n\nBody.\n";
        let mut b = empty_builder();
        feed(&mut b, "page.md", content);
        let plan = b.finalize();
        let doc = &plan.new_documents[0];
        let fm = doc.frontmatter.as_ref().expect("frontmatter parsed");
        assert_eq!(fm["verified"], true);
        assert!(doc.provenance.verified);
        assert_eq!(doc.provenance.verified_by.as_deref(), Some("midnight-foundation"),);
        assert_eq!(doc.provenance.attribution, Attribution::Foundation);
    }

    #[test]
    fn carried_documents_do_not_emit_chunks() {
        let content = "# Stable\n\nThis body never changes.";
        let prior_id = Uuid::new_v4();
        let prior_state = PriorState {
            documents: vec![prior("stable.md", content, prior_id)],
        };
        let mut b = PlanBuilder::new("docs", SourceKind::DocsSite, "rev-2", prior_state);
        feed(&mut b, "stable.md", content);
        let plan = b.finalize();
        assert_eq!(plan.stats.chunks_emitted, 0);
        assert!(plan.new_documents.is_empty());
    }

    #[test]
    fn mixed_scenario_3_new_2_carried_1_deleted() {
        let stable_a = "# A\n\nstable A body";
        let stable_b = "# B\n\nstable B body";
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let id_old1 = Uuid::new_v4();
        let id_old2 = Uuid::new_v4();
        let id_gone = Uuid::new_v4();
        let prior_state = PriorState {
            documents: vec![
                prior("a.md", stable_a, id_a),
                prior("b.md", stable_b, id_b),
                prior("changed1.md", "# Old 1", id_old1),
                prior("changed2.md", "# Old 2", id_old2),
                prior("gone.md", "# Gone", id_gone),
            ],
        };
        let mut b = PlanBuilder::new("docs", SourceKind::DocsSite, "rev-2", prior_state);
        feed(&mut b, "a.md", stable_a); // carried
        feed(&mut b, "b.md", stable_b); // carried
        feed(&mut b, "changed1.md", "# New 1\n\nnew body"); // new (changed)
        feed(&mut b, "changed2.md", "# New 2\n\nnew body"); // new (changed)
        feed(&mut b, "brand-new.md", "# Brand new\n\nfresh"); // new (added)

        let plan = b.finalize();
        assert_eq!(plan.stats.documents_added, 3);
        assert_eq!(plan.stats.documents_carried, 2);
        assert_eq!(plan.stats.documents_deleted, 1);
        assert_eq!(plan.deleted_documents[0].path, PathBuf::from("gone.md"));
    }

    #[test]
    fn duplicate_path_is_rejected() {
        let mut b = empty_builder();
        feed(&mut b, "x.md", "# A");
        let split = split_frontmatter("# B");
        let leaf = crate::manifest::resolve::ResolvedLeaf {
            rel_path: PathBuf::from("x.md"),
            kind: DocumentKind::Markdown,
            name: None,
            published_url: None,
            source_url: None,
            provenance_override: Provenance::default(),
            no_extract: false,
        };
        let ctx = WalkContext {
            path: PathBuf::from("x.md"),
            kind: DocumentKind::Markdown,
            content: "# B",
            split: &split,
            resolved: &leaf,
            extracted: Provenance::default(),
            source_modified_at: None,
            package: None,
        };
        let err = b.add_walked_document(&ctx).unwrap_err();
        assert!(matches!(err, IngestError::DuplicatePath(_)));
    }

    /// Feed one document whose body makes the chunker panic, returning the
    /// builder's result so the caller can assert degrade-vs-fail behavior.
    fn feed_panicking(builder: &mut PlanBuilder, path: &str) -> Result<(), IngestError> {
        let body = crate::chunk::PANIC_SENTINEL;
        let split = split_frontmatter(body);
        let leaf = crate::manifest::resolve::ResolvedLeaf {
            rel_path: PathBuf::from(path),
            kind: DocumentKind::Markdown,
            name: None,
            published_url: None,
            source_url: None,
            provenance_override: Provenance::default(),
            no_extract: false,
        };
        let ctx = WalkContext {
            path: PathBuf::from(path),
            kind: DocumentKind::Markdown,
            content: body,
            split: &split,
            resolved: &leaf,
            extracted: Provenance::default(),
            source_modified_at: None,
            package: None,
        };
        builder.add_walked_document(&ctx)
    }

    #[test]
    fn chunker_panic_degrades_to_line_window_by_default() {
        // Regression for issue #121: a chunker panic on one file must not abort
        // the run. The file is planned via the line-window fallback instead.
        let mut b = empty_builder();
        let r = feed_panicking(&mut b, "boom.md");
        assert!(r.is_ok(), "default mode must absorb a chunker panic, not abort");

        let plan = b.finalize();
        assert_eq!(plan.new_documents.len(), 1);
        let doc = &plan.new_documents[0];
        assert_eq!(doc.path, PathBuf::from("boom.md"));
        // The sentinel body is non-empty, so the fallback emitted chunks.
        assert!(!doc.chunks.is_empty());
        assert_eq!(plan.stats.documents_added, 1);
    }

    #[test]
    fn chunker_panic_is_a_run_failure_under_strict() {
        // Under --strict the same panic is a hard, file-attributed failure.
        let mut b = empty_builder().with_strict(true);
        let err = feed_panicking(&mut b, "boom.md").unwrap_err();
        assert!(matches!(err, IngestError::ChunkPanic { .. }));
        let msg = err.to_string();
        assert!(msg.contains("boom.md"), "strict error must name the offending file: {msg}",);
    }

    #[test]
    fn chunks_carry_total_and_index_for_multi_chunk_documents() {
        // Suppress coalescing (each ~15-token section fits the 28-token budget
        // alone, two adjacent ones exceed the 25-token target) so the three
        // sections stay as three chunks and the total_chunks / chunk_index
        // bookkeeping is genuinely exercised.
        let mut b = per_section_builder();
        feed(
            &mut b,
            "multi.md",
            "# A\n\nthis section body has roughly fifteen tokens of filler text here\n\n\
             # B\n\nthis section body has roughly fifteen tokens of filler text here\n\n\
             # C\n\nthis section body has roughly fifteen tokens of filler text here\n",
        );
        let plan = b.finalize();
        let doc = &plan.new_documents[0];
        assert_eq!(doc.chunks.len(), 3);
        for (i, c) in doc.chunks.iter().enumerate() {
            assert_eq!(c.chunk_index, u32::try_from(i).unwrap());
            assert_eq!(c.total_chunks, 3);
        }
    }

    #[test]
    fn chunk_content_hashes_are_stable_and_unique_for_distinct_content() {
        // Suppress coalescing (sections sized to NOT merge under the 28-token
        // budget) so the two sections stay distinct chunks with distinct
        // content hashes (the property under test).
        let mut b = per_section_builder();
        feed(
            &mut b,
            "h.md",
            "# A\n\nthis section body has roughly fifteen tokens of filler text here\n\n\
             # B\n\nthis other section body differs with its own fifteen tokens of filler\n",
        );
        let plan = b.finalize();
        let chunks = &plan.new_documents[0].chunks;
        assert_eq!(chunks.len(), 2);
        assert_ne!(chunks[0].content_hash, chunks[1].content_hash);
        assert_eq!(chunks[0].content_hash, chunk_hash(&chunks[0].content));
    }

    #[test]
    fn document_paths_never_overlap_across_three_vectors() {
        // Manual single-instance check; proptest below covers the property.
        let id = Uuid::new_v4();
        let prior_state = PriorState {
            documents: vec![prior("a.md", "# A", id), prior("b.md", "# B", id)],
        };
        let mut b = PlanBuilder::new("docs", SourceKind::DocsSite, "rev-2", prior_state);
        feed(&mut b, "a.md", "# A"); // carried
        feed(&mut b, "c.md", "# C"); // new
        let plan = b.finalize();
        let mut seen: HashSet<&PathBuf> = HashSet::new();
        for p in plan
            .new_documents
            .iter()
            .map(|d| &d.path)
            .chain(plan.carried_documents.iter().map(|d| &d.path))
            .chain(plan.deleted_documents.iter().map(|d| &d.path))
        {
            assert!(seen.insert(p), "path {p:?} appeared twice");
        }
    }

    #[test]
    fn finalize_preserves_target_revision_and_source_metadata() {
        let b = PlanBuilder::new(
            "midnight-docs",
            SourceKind::DocsSite,
            "abcdef1234",
            PriorState::default(),
        );
        let plan = b.finalize();
        assert_eq!(plan.source_slug, "midnight-docs");
        assert_eq!(plan.source_kind, SourceKind::DocsSite);
        assert_eq!(plan.target_revision, "abcdef1234");
    }

    #[test]
    fn planned_document_carries_resolved_metadata() {
        use crate::manifest::resolve::ResolvedLeaf;
        use mnm_core::types::DocumentKind;

        let leaf = ResolvedLeaf {
            rel_path: PathBuf::from("a.md"),
            kind: DocumentKind::Markdown,
            name: None,
            published_url: Some("https://docs.example.com/a/".to_owned()),
            source_url: Some("https://github.com/x/y/blob/main/a.md".to_owned()),
            provenance_override: Provenance::default(),
            no_extract: false,
        };
        let mut b = empty_builder();
        let split = split_frontmatter("# A\n\nbody");
        let ctx = WalkContext {
            path: leaf.rel_path.clone(),
            kind: DocumentKind::Markdown,
            content: "# A\n\nbody",
            split: &split,
            resolved: &leaf,
            extracted: Provenance::default(),
            source_modified_at: None,
            package: None,
        };
        b.add_walked_document(&ctx).unwrap();
        let plan = b.finalize();
        let doc = &plan.new_documents[0];
        assert_eq!(doc.published_url.as_deref(), Some("https://docs.example.com/a/"));
        assert_eq!(doc.source_url.as_deref(), Some("https://github.com/x/y/blob/main/a.md"));
        assert_eq!(doc.language.as_deref(), Some("markdown"));
    }

    #[test]
    fn token_counts_are_populated_and_sum_to_document_total() {
        let mut b = empty_builder();
        feed(&mut b, "x.md", "# A\n\nbody one\n\n# B\n\nbody two with more tokens here.\n");
        let plan = b.finalize();
        let doc = &plan.new_documents[0];
        let chunk_sum: u32 = doc.chunks.iter().map(|c| c.token_count).sum();
        assert!(doc.token_count > 0);
        assert_eq!(doc.token_count, chunk_sum);
    }

    #[test]
    fn planned_chunk_has_symbol_path_field() {
        // Build a minimal markdown PlannedDocument using the existing test helpers,
        // get its first planned chunk, and assert symbol_path is an empty Vec.
        let mut b = empty_builder();
        feed(&mut b, "intro.md", "# Hello\n\nWelcome to the docs.");
        let plan = b.finalize();
        let pc = &plan.new_documents[0].chunks[0];
        assert!(pc.symbol_path.is_empty());
    }

    #[cfg(feature = "core-grammars")]
    #[test]
    fn code_documents_get_symbol_paths() {
        use crate::manifest::resolve::ResolvedLeaf;
        use mnm_core::types::DocumentKind;

        let code = "impl Foo {\n    fn bar(&self) { let x = 1; }\n}\n";
        let split = split_frontmatter(code);
        let leaf = ResolvedLeaf {
            rel_path: PathBuf::from("src/lib.rs"),
            kind: DocumentKind::Code,
            name: None,
            published_url: None,
            source_url: None,
            provenance_override: Provenance::default(),
            no_extract: false,
        };
        let ctx = WalkContext {
            path: PathBuf::from("src/lib.rs"),
            kind: DocumentKind::Code,
            content: code,
            split: &split,
            resolved: &leaf,
            extracted: Provenance::default(),
            source_modified_at: None,
            package: None,
        };
        let mut b = empty_builder();
        b.add_walked_document(&ctx).expect("add_walked_document");
        let plan = b.finalize();

        let chunks = &plan.new_documents[0].chunks;
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_path.iter().any(|s| s.kind == "impl" && s.name == "Foo")),
            "expected a chunk with symbol_path containing {{kind:\"impl\", name:\"Foo\"}}, got: {chunks:?}"
        );
        assert!(
            chunks.iter().all(|c| c.heading_path.is_empty()),
            "code chunks should have empty heading_path"
        );
    }

    #[test]
    fn merge_precedence_frontmatter_extracted_manifest() {
        use mnm_core::provenance::{LanguageTarget, Provenance};
        let fm = Provenance {
            language_targets: vec![LanguageTarget {
                name: "compact".into(),
                version_constraint: Some(">=0.30".into()),
            }],
            ..Provenance::default()
        };
        let extracted = Provenance {
            language_targets: vec![LanguageTarget {
                name: "compact".into(),
                version_constraint: Some(">=0.23".into()),
            }],
            sdk_dependencies: vec![mnm_core::provenance::SdkDependency {
                kind: "npm".into(),
                name: "@midnight-ntwrk/midnight-js".into(),
                version_constraint: Some("^1.4.0".into()),
            }],
            ..Provenance::default()
        };
        let manifest = Provenance::attributed_to(mnm_core::provenance::Attribution::Foundation);
        let merged = merge_provenance(&fm, &extracted, &manifest);
        // frontmatter beats extracted
        assert_eq!(merged.language_targets[0].version_constraint.as_deref(), Some(">=0.30"));
        // extracted fills what frontmatter lacks
        assert_eq!(merged.sdk_dependencies.len(), 1);
        // manifest fills what both lack
        assert_eq!(merged.attribution, mnm_core::provenance::Attribution::Foundation);
        // no frontmatter → extracted wins the lists
        let merged2 = merge_provenance(&Provenance::default(), &extracted, &manifest);
        assert_eq!(merged2.language_targets[0].version_constraint.as_deref(), Some(">=0.23"));
    }

    #[test]
    fn hash_covers_frontmatter_and_processed_body() {
        use std::path::Path;

        use crate::content_hash::{document_hash, hash_input};
        use crate::preprocess::preprocess;
        use mnm_core::types::DocumentKind;

        // Copyright-year-only upstream changes vanish in preprocessing, so the
        // hash is identical — the spec's noise-only carry-forward invariant.
        let v2024 = "// Copyright 2024 Foo Corp\n// Licensed under the Apache License, Version 2.0\nfn main() {}\n";
        let v2025 = "// Copyright 2025 Foo Corp\n// Licensed under the Apache License, Version 2.0\nfn main() {}\n";
        let a = preprocess(DocumentKind::Code, Path::new("m.rs"), v2024, None);
        let b = preprocess(DocumentKind::Code, Path::new("m.rs"), v2025, None);
        assert_eq!(a.body, b.body);
        let ha = document_hash(&hash_input(None, &a.body));
        assert_eq!(ha, document_hash(&hash_input(None, &b.body)));
        // …frontmatter changes still invalidate.
        let hc = document_hash(&hash_input(Some("---\nx: 1\n---\n"), &a.body));
        assert_ne!(ha, hc);
    }
}

#[cfg(test)]
mod proptests {
    use std::collections::HashSet;

    use proptest::collection::vec;
    use proptest::prelude::*;

    use super::*;
    use crate::frontmatter::split as split_frontmatter;

    fn path_strategy() -> impl Strategy<Value = String> {
        // Short ASCII filenames with .md extension, drawn from a small alphabet
        // so prior/walk path intersection is non-trivial.
        "[a-h]{1,3}\\.md"
    }

    fn body_strategy() -> impl Strategy<Value = String> {
        // Two short bodies so hashes either match or differ, exercising
        // both the carried path and the new path.
        prop_oneof![
            Just("# A\n\nbody one".to_owned()),
            Just("# B\n\nbody two".to_owned())
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

        #[test]
        fn paths_are_disjoint_across_three_vectors(
            prior in vec((path_strategy(), body_strategy()), 0..8),
            walk in vec((path_strategy(), body_strategy()), 0..8),
        ) {
            // Dedup prior by path so PriorState is well-formed.
            let mut prior_by_path: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for (p, body) in prior {
                prior_by_path.insert(p, body);
            }
            let prior_state = PriorState {
                documents: prior_by_path
                    .iter()
                    .map(|(p, body)| PriorDocument {
                        path: PathBuf::from(p),
                        content_hash: document_hash(body),
                        document_id: Uuid::new_v4(),
                    })
                    .collect(),
            };

            // Dedup walked entries by path (the builder rejects duplicates).
            let mut seen: HashSet<String> = HashSet::new();
            let mut builder = PlanBuilder::new(
                "docs",
                SourceKind::DocsSite,
                "rev",
                prior_state,
            );
            for (p, body) in walk {
                if !seen.insert(p.clone()) {
                    continue;
                }
                let split = split_frontmatter(&body);
                let leaf = crate::manifest::resolve::ResolvedLeaf {
                    rel_path: PathBuf::from(&p),
                    kind: DocumentKind::Markdown,
                    name: None,
                    published_url: None,
                    source_url: None,
                    provenance_override: Provenance::default(),
                    no_extract: false,
                };
                let ctx = WalkContext {
                    path: PathBuf::from(&p),
                    kind: DocumentKind::Markdown,
                    content: &body,
                    split: &split,
                    resolved: &leaf,
                    extracted: Provenance::default(),
                    source_modified_at: None,
                    package: None,
                };
                builder
                    .add_walked_document(&ctx)
                    .expect("dedup'd above");
            }
            let plan = builder.finalize();

            // Three vectors must be path-disjoint.
            let mut all: HashSet<PathBuf> = HashSet::new();
            for p in plan.new_documents.iter().map(|d| d.path.clone()) {
                prop_assert!(all.insert(p));
            }
            for p in plan.carried_documents.iter().map(|d| d.path.clone()) {
                prop_assert!(all.insert(p));
            }
            for p in plan.deleted_documents.iter().map(|d| d.path.clone()) {
                prop_assert!(all.insert(p));
            }

            // Stats agree with vector lengths.
            prop_assert_eq!(plan.stats.documents_added, plan.new_documents.len());
            prop_assert_eq!(plan.stats.documents_carried, plan.carried_documents.len());
            prop_assert_eq!(plan.stats.documents_deleted, plan.deleted_documents.len());

            // Each vector is lex-sorted.
            for window in plan.new_documents.windows(2) {
                prop_assert!(window[0].path <= window[1].path);
            }
            for window in plan.carried_documents.windows(2) {
                prop_assert!(window[0].path <= window[1].path);
            }
            for window in plan.deleted_documents.windows(2) {
                prop_assert!(window[0].path <= window[1].path);
            }
        }
    }
}

//! [`IngestPlan`] and [`PlanBuilder`] — the core orchestrator types.

#![allow(clippy::derive_partial_eq_without_eq)] // serde_json::Value in PlannedDocument blocks Eq

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use mn_core::provenance::Provenance;
use mn_core::types::{DocumentKind, SourceKind};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::content_hash::{chunk_hash, document_hash};
use crate::frontmatter::FrontmatterSplit;
use crate::markdown::{chunk_markdown, ChunkerConfig};

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
/// `mn-store::entities::document` for the active version's documents and
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
    /// 0-indexed position among the document's chunks.
    pub chunk_index: u32,
    /// Total chunks in the document, for `total_chunks` column.
    pub total_chunks: u32,
    /// Byte offset of the chunk's first character within the source document.
    pub start_byte: usize,
    /// Byte offset just past the chunk's last character.
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
    /// IANA-like language identifier from `mn_content::language`.
    pub language: Option<String>,
    /// Token count of the document body (computed once at chunk time and
    /// summed across chunks; landed in Task 12).
    pub token_count: u32,
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
    /// Filesystem modification timestamp at walk time (`None` if the OS
    /// could not supply `mtime`).
    pub source_modified_at: Option<time::OffsetDateTime>,
}

/// Stateful builder for [`IngestPlan`]. One instance per ingest run.
#[derive(Debug)]
pub struct PlanBuilder {
    source_slug: String,
    source_kind: SourceKind,
    target_revision: String,
    chunker_config: ChunkerConfig,
    prior_by_path: HashMap<PathBuf, PriorDocument>,
    seen_paths: HashSet<PathBuf>,
    new_documents: Vec<PlannedDocument>,
    carried_documents: Vec<CarriedDocument>,
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
            prior_by_path,
            seen_paths: HashSet::new(),
            new_documents: Vec::new(),
            carried_documents: Vec::new(),
        }
    }

    /// Override the chunker configuration (defaults to [`ChunkerConfig::default`]).
    #[must_use]
    pub const fn with_chunker_config(mut self, cfg: ChunkerConfig) -> Self {
        self.chunker_config = cfg;
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
    pub fn add_walked_document(
        &mut self,
        walked: &WalkContext<'_>,
    ) -> Result<(), IngestError> {
        if !self.seen_paths.insert(walked.path.clone()) {
            return Err(IngestError::DuplicatePath(walked.path.clone()));
        }

        let hash = document_hash(walked.content);

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

        let chunks = match walked.kind {
            DocumentKind::Markdown => chunk_markdown(&walked.split.body, self.chunker_config),
            DocumentKind::Code | DocumentKind::Plaintext => {
                // Phase 9a only ships the Markdown chunker through the
                // orchestrator. Code chunking lands in a follow-up — for now
                // route through the Markdown chunker's fallback windowing
                // path, which is content-agnostic.
                chunk_markdown(&walked.split.body, self.chunker_config)
            }
        };
        let total = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
        let planned_chunks: Vec<PlannedChunk> = chunks
            .into_iter()
            .map(|c| {
                let content_hash = chunk_hash(&c.content);
                PlannedChunk {
                    content: c.content,
                    heading_path: c.heading_path,
                    chunk_index: c.chunk_index,
                    total_chunks: total,
                    start_byte: c.start_byte,
                    end_byte: c.end_byte,
                    content_hash,
                    token_count: 0,
                }
            })
            .collect();

        self.new_documents.push(PlannedDocument {
            path: walked.path.clone(),
            kind: walked.kind,
            content_hash: hash,
            frontmatter: walked.split.frontmatter.clone(),
            provenance: merge_provenance(&walked.split.provenance, &walked.resolved.provenance_override),
            char_count: walked.content.chars().count(),
            chunks: planned_chunks,
            published_url: walked.resolved.published_url.clone(),
            source_url: walked.resolved.source_url.clone(),
            source_modified_at: walked.source_modified_at,
            language: crate::language::from_path(&walked.resolved.rel_path).map(str::to_owned),
            token_count: 0,
        });
        Ok(())
    }
}

/// Frontmatter wins per-field; ancestor `resolved` fills only the gaps.
fn merge_provenance(frontmatter: &Provenance, ancestor: &Provenance) -> Provenance {
    let default = Provenance::default();
    let mut out = ancestor.clone();
    if frontmatter.attribution != default.attribution {
        out.attribution = frontmatter.attribution;
    }
    if frontmatter.verified != default.verified {
        out.verified = frontmatter.verified;
    }
    if frontmatter.verified_by != default.verified_by {
        out.verified_by = frontmatter.verified_by.clone();
    }
    if frontmatter.verified_at != default.verified_at {
        out.verified_at = frontmatter.verified_at;
    }
    if frontmatter.verification_notes != default.verification_notes {
        out.verification_notes = frontmatter.verification_notes.clone();
    }
    if !frontmatter.language_targets.is_empty() {
        out.language_targets = frontmatter.language_targets.clone();
    }
    if !frontmatter.sdk_dependencies.is_empty() {
        out.sdk_dependencies = frontmatter.sdk_dependencies.clone();
    }
    if frontmatter.deprecation != default.deprecation {
        out.deprecation = frontmatter.deprecation.clone();
    }
    if !frontmatter.tags.is_empty() {
        out.tags = frontmatter.tags.clone();
    }
    if frontmatter.content_type != default.content_type {
        out.content_type = frontmatter.content_type;
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
            stats,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use mn_core::provenance::Attribution;

    use super::*;
    use crate::frontmatter::split as split_frontmatter;

    fn prior(path: &str, content: &str, id: Uuid) -> PriorDocument {
        PriorDocument {
            path: PathBuf::from(path),
            content_hash: document_hash(content),
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
            provenance_override: Default::default(),
        };
        let ctx = WalkContext {
            path: PathBuf::from(path),
            kind: DocumentKind::Markdown,
            content,
            split: &split,
            resolved: &leaf,
            source_modified_at: None,
        };
        builder
            .add_walked_document(&ctx)
            .expect("add_walked_document");
    }

    fn empty_builder() -> PlanBuilder {
        PlanBuilder::new("docs", SourceKind::DocsSite, "rev-1", PriorState::default())
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
            provenance_override: Default::default(),
        };
        let ctx = WalkContext {
            path: PathBuf::from("x.md"),
            kind: DocumentKind::Markdown,
            content: "# B",
            split: &split,
            resolved: &leaf,
            source_modified_at: None,
        };
        let err = b.add_walked_document(&ctx).unwrap_err();
        assert!(matches!(err, IngestError::DuplicatePath(_)));
    }

    #[test]
    fn chunks_carry_total_and_index_for_multi_chunk_documents() {
        let mut b = empty_builder();
        feed(&mut b, "multi.md", "# A\n\nbody A\n\n# B\n\nbody B\n\n# C\n\nbody C\n");
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
        let mut b = empty_builder();
        feed(&mut b, "h.md", "# A\n\nbody A\n\n# B\n\nbody B that differs.\n");
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
        use mn_core::types::DocumentKind;

        let leaf = ResolvedLeaf {
            rel_path: PathBuf::from("a.md"),
            kind: DocumentKind::Markdown,
            name: None,
            published_url: Some("https://docs.example.com/a/".to_owned()),
            source_url: Some("https://github.com/x/y/blob/main/a.md".to_owned()),
            provenance_override: Default::default(),
        };
        let mut b = empty_builder();
        let split = split_frontmatter("# A\n\nbody");
        let ctx = WalkContext {
            path: leaf.rel_path.clone(),
            kind: DocumentKind::Markdown,
            content: "# A\n\nbody",
            split: &split,
            resolved: &leaf,
            source_modified_at: None,
        };
        b.add_walked_document(&ctx).unwrap();
        let plan = b.finalize();
        let doc = &plan.new_documents[0];
        assert_eq!(doc.published_url.as_deref(), Some("https://docs.example.com/a/"));
        assert_eq!(
            doc.source_url.as_deref(),
            Some("https://github.com/x/y/blob/main/a.md")
        );
        assert_eq!(doc.language.as_deref(), Some("markdown"));
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
                    provenance_override: Default::default(),
                };
                let ctx = WalkContext {
                    path: PathBuf::from(&p),
                    kind: DocumentKind::Markdown,
                    content: &body,
                    split: &split,
                    resolved: &leaf,
                    source_modified_at: None,
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

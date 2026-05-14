//! Ingest orchestrator — builds an [`IngestPlan`] from a walked source tree.
//!
//! Phase 9a deliverable. Pure library: no database, no network, no filesystem
//! writes. The caller is responsible for reading the prior active source
//! version's document inventory, feeding it in via [`PriorState`], and writing
//! the resulting plan back through `mn-store` (Phase 9b).
//!
//! The orchestrator classifies every document into one of three buckets:
//!
//! 1. **new** — the file is present in this walk and its [`document_hash`]
//!    differs from the prior version (or the file is brand-new). The chunker
//!    runs and the resulting [`PlannedDocument`] carries the chunks ready for
//!    insertion.
//!
//! 2. **carried** — the file is present in this walk AND its `document_hash`
//!    matches the prior version. No chunk-level work is done; the caller
//!    re-links chunks from the prior `source_version` (FR-014).
//!
//! 3. **deleted** — the file was in the prior version but is absent from this
//!    walk. The caller must NOT re-link it.
//!
//! All three vectors are sorted lexicographically by `path` on [`PlanBuilder::finalize`]
//! so PR review diffs stay stable.
//!
//! [`document_hash`]: crate::content_hash::document_hash

pub mod plan;
pub mod walker;

pub use plan::{
    CarriedDocument, DeletedDocument, IngestError, IngestPlan, IngestStats, PlanBuilder,
    PlannedChunk, PlannedDocument, PriorDocument, PriorState,
};
pub use walker::{WalkError, WalkedDocument, Walker};

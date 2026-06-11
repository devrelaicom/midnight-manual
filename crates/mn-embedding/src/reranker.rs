//! Rerank result type shared by the Voyage reranker client and its callers.
//!
//! (The local fastembed/ONNX cross-encoder subsystem was removed — see
//! docs/superpowers/specs/2026-06-11-voyage-reranking-design.md §5.)

/// One reranked document: `index` points into the input `documents` slice;
/// `score` is Voyage's `relevance_score` in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RerankResult {
    /// Index into the original `documents` input.
    pub index: usize,
    /// Voyage relevance score in `[0, 1]`.
    pub score: f32,
}

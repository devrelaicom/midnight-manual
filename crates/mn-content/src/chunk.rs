//! Shared chunker contract: one trait, one config, one output shape, used by
//! the markdown chunker, the code chunkers, and the line-window fallback.

use mn_core::types::SymbolSegment;

/// One chunk emitted by any [`Chunker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Verbatim chunk content.
    pub content: String,
    /// Ancestor heading path (markdown). Empty for code/plaintext.
    pub heading_path: Vec<String>,
    /// Structured symbol path (code). Empty for markdown/plaintext.
    pub symbol_path: Vec<SymbolSegment>,
    /// Byte offset of the chunk's first character in the source.
    pub start_byte: usize,
    /// Byte offset just past the chunk's last character.
    pub end_byte: usize,
    /// Token count of this chunk (BPE, via [`crate::tokens::count`]).
    pub token_count: u32,
    /// 0-indexed position among the document's chunks.
    pub chunk_index: u32,
    /// True iff produced by the line-window fallback.
    pub fallback_used: bool,
}

/// Configuration shared by all chunkers. Token-budgeted.
#[derive(Debug, Clone, Copy)]
pub struct ChunkerConfig {
    /// Max chunk size in BPE tokens before splitting.
    pub max_tokens: u32,
    /// Soft minimum chunk size in BPE tokens — the markdown coalescing target
    /// AND the core-body floor (~70% of `max_tokens`) the rolling window pads
    /// out from. Adjacent small markdown sections merge up to this. Markdown-only.
    pub min_tokens: u32,
    /// Rolling-window: fill the smaller side up to this fraction of `max_tokens`
    /// before switching sides. Markdown-only.
    pub window_switch_pct: f32,
    /// Rolling-window: final fill target, as a fraction of `max_tokens`.
    pub window_target_pct: f32,
    /// Rolling-window: hard cap, as a fraction of `max_tokens` (never exceeded).
    pub window_cap_pct: f32,
    /// Code coalescing floor in BPE tokens: adjacent same-scope code chunks merge
    /// up to this. Code-only.
    pub code_min_tokens: u32,
    /// Line-window fallback size (lines).
    pub fallback_lines: u32,
    /// Line-window fallback overlap (lines).
    pub fallback_overlap_lines: u32,
    /// Files larger than this are skipped by callers (EC-52).
    pub max_file_bytes: u64,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            max_tokens: 400,
            min_tokens: 280,
            window_switch_pct: 0.80,
            window_target_pct: 0.90,
            window_cap_pct: 1.00,
            code_min_tokens: 64,
            fallback_lines: 60,
            fallback_overlap_lines: 20,
            max_file_bytes: 10 * 1024 * 1024,
        }
    }
}

/// Greedy coalescing target: 90% of `max_tokens` (D2). Both the markdown and
/// code coalescers pack sibling units up to this; only a single unit larger
/// than `max_tokens` is ever split.
#[must_use]
pub const fn coalesce_target(cfg: &ChunkerConfig) -> u32 {
    cfg.max_tokens.saturating_mul(9) / 10
}

/// Errors a chunker can surface for one file. Never panics; the planner maps
/// these to a per-file warning (default) or a run failure (`--strict`).
#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    /// The parser failed badly enough that we fell back to line-window.
    /// Carries the reason for the warning message.
    #[error("parser fell back to line-window: {0}")]
    ParserFallback(String),
}

/// The chunking contract. Implementations: markdown, each code language,
/// and the line-window fallback.
pub trait Chunker {
    /// Chunk `body`. Returns at least one chunk for non-empty, non-whitespace
    /// input; an empty vec for empty/whitespace input.
    ///
    /// # Errors
    /// Returns [`ChunkError`] only when the caller asked for strict behavior;
    /// the default implementations recover internally and return `Ok`.
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_token_budgeted() {
        let c = ChunkerConfig::default();
        assert_eq!(c.max_tokens, 400);
        assert_eq!(c.min_tokens, 280);
        // Bit-exact comparison: these are literal default constants, and
        // `clippy::float_cmp` (denied in CI) rejects `==` on floats.
        assert_eq!(c.window_switch_pct.to_bits(), 0.80_f32.to_bits());
        assert_eq!(c.window_target_pct.to_bits(), 0.90_f32.to_bits());
        assert_eq!(c.window_cap_pct.to_bits(), 1.00_f32.to_bits());
        assert_eq!(c.code_min_tokens, 64);
        assert_eq!(c.fallback_lines, 60);
        assert_eq!(c.fallback_overlap_lines, 20);
        assert_eq!(c.max_file_bytes, 10 * 1024 * 1024);
    }

    #[test]
    fn coalesce_target_is_90_pct_of_max_tokens() {
        // 921 == 1024 * 9 / 10 (integer division); 1024 becomes the default
        // `max_tokens` in the ChunkerConfig cleanup task.
        assert_eq!(
            coalesce_target(&ChunkerConfig {
                max_tokens: 1024,
                ..ChunkerConfig::default()
            }),
            921
        );
    }
}

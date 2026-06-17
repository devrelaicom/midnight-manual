//! Wrapper over text-splitter's `CodeSplitter`.
//!
//! Given source + a tree-sitter language + a token budget, returns byte ranges
//! of budgeted semantic chunks.

use std::ops::Range;

use text_splitter::{ChunkConfig, CodeSplitter};

use crate::chunk::{ChunkError, ChunkerConfig};

/// Split `src` into byte ranges, each within `cfg.max_tokens` BPE tokens where
/// the grammar allows, falling on the largest semantic node that fits.
///
/// # Errors
///
/// Returns [`ChunkError::ParserFallback`] if the splitter cannot build (e.g.
/// grammar/runtime ABI mismatch) — the caller then uses line-window.
pub fn split_ranges(
    src: &str,
    language: &tree_sitter::Language,
    cfg: &ChunkerConfig,
) -> Result<Vec<Range<usize>>, ChunkError> {
    let tokenizer = crate::tokens::tokenizer();
    let splitter = CodeSplitter::new(
        language.clone(),
        ChunkConfig::new(usize::try_from(cfg.max_tokens).unwrap_or(usize::MAX))
            .with_sizer(tokenizer),
    )
    .map_err(|e| ChunkError::ParserFallback(format!("code splitter init: {e}")))?;

    Ok(splitter
        .chunk_indices(src)
        .map(|(start, piece)| start..start + piece.len())
        .collect())
}

#[cfg(all(test, feature = "core-grammars"))]
mod tests {
    use super::*;
    use crate::chunk::ChunkerConfig;

    #[test]
    fn splits_rust_into_budgeted_ranges() {
        let src = "fn a() {}\nfn b() {}\nstruct S { x: u32 }\n";
        let lang = tree_sitter_rust::LANGUAGE.into();
        let cfg = ChunkerConfig {
            max_tokens: 16,
            ..ChunkerConfig::default()
        };
        let ranges = split_ranges(src, &lang, &cfg).unwrap();
        assert!(!ranges.is_empty());
        assert!(ranges.iter().all(|r| r.end <= src.len() && r.start < r.end));
    }
}

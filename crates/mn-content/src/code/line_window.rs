//! Line-window fallback — used for unknown languages, parser-error recovery,
//! and Compact (until compactp). No syntax awareness; overlapping windows.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};

/// Splits source into fixed line-count windows with overlap.
pub struct LineWindowChunker;

impl Chunker for LineWindowChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }
        // Precompute byte offset of the start of each line.
        let mut line_starts = vec![0usize];
        for (i, b) in body.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let total_lines = line_starts.len();
        let window = usize::try_from(cfg.fallback_lines.max(1)).unwrap_or(usize::MAX);
        let overlap_raw = cfg
            .fallback_overlap_lines
            .min(cfg.fallback_lines.saturating_sub(1));
        let overlap = usize::try_from(overlap_raw).unwrap_or(usize::MAX);
        let step = window.saturating_sub(overlap).max(1);

        let mut chunks = Vec::new();
        let mut start_line = 0usize;
        let mut idx = 0u32;
        while start_line < total_lines {
            let end_line = (start_line + window).min(total_lines);
            let start_byte = line_starts[start_line];
            let end_byte = if end_line < total_lines {
                line_starts[end_line]
            } else {
                body.len()
            };
            let content = body[start_byte..end_byte].to_string();
            if !content.trim().is_empty() {
                chunks.push(Chunk {
                    token_count: crate::tokens::count(&content),
                    content,
                    heading_path: Vec::new(),
                    symbol_path: Vec::new(),
                    start_byte,
                    end_byte,
                    chunk_index: idx,
                    fallback_used: true,
                });
                idx += 1;
            }
            if end_line >= total_lines {
                break;
            }
            start_line += step;
        }
        Ok(chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn splits_into_overlapping_line_windows() {
        let body: String = (0..200).fold(String::new(), |mut s, i| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line {i}");
            s
        });
        let cfg = ChunkerConfig {
            fallback_lines: 60,
            fallback_overlap_lines: 20,
            ..ChunkerConfig::default()
        };
        let chunks = LineWindowChunker.chunk(&body, &cfg).unwrap();
        assert!(chunks.len() >= 3, "200 lines / (60-20 step) ≈ 5 windows");
        assert!(chunks.iter().all(|c| c.fallback_used));
        assert!(chunks.iter().all(|c| c.symbol_path.is_empty()));
        // overlap: window 2 starts before window 1 ends (by line)
        assert!(chunks[1].start_byte < chunks[0].end_byte);
    }

    #[test]
    fn empty_input_no_chunks() {
        assert!(LineWindowChunker
            .chunk("  \n ", &ChunkerConfig::default())
            .unwrap()
            .is_empty());
    }
}

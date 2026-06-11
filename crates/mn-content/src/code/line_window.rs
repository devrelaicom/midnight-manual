//! Line-window fallback for Plaintext, unknown languages, and parser-error recovery.
//!
//! Emits token-budgeted, NON-overlapping windows (D3): each window grows line
//! by line until adding the next line would push it past 90% of `max_tokens`,
//! then the next window starts on the next line.

use crate::chunk::{coalesce_target, Chunk, ChunkError, Chunker, ChunkerConfig};

/// Splits source into token-budgeted, non-overlapping line windows.
pub struct LineWindowChunker;

impl Chunker for LineWindowChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }
        let target = coalesce_target(cfg);
        // Precompute byte offset of the start of each line.
        let mut line_starts = vec![0usize];
        for (i, b) in body.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let total_lines = line_starts.len();
        let line_end = |i: usize| -> usize {
            if i + 1 < total_lines {
                line_starts[i + 1]
            } else {
                body.len()
            }
        };

        let mut chunks = Vec::new();
        let mut start_line = 0usize;
        let mut idx = 0u32;
        while start_line < total_lines {
            // Grow: at least one line, then keep adding while within budget.
            let mut end_line = start_line + 1;
            while end_line < total_lines {
                let slice = &body[line_starts[start_line]..line_end(end_line)];
                if crate::tokens::count(slice) > target {
                    break;
                }
                end_line += 1;
            }
            let start_byte = line_starts[start_line];
            let end_byte = line_end(end_line - 1);
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
            start_line = end_line;
        }
        Ok(chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{coalesce_target, Chunker, ChunkerConfig};

    #[test]
    fn windows_are_token_budgeted_and_disjoint() {
        let body: String = (0..400).fold(String::new(), |mut s, i| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line {i} with a few more words for tokens");
            s
        });
        let cfg = ChunkerConfig {
            max_tokens: 128,
            ..ChunkerConfig::default()
        };
        let chunks = LineWindowChunker.chunk(&body, &cfg).unwrap();
        assert!(chunks.len() >= 2, "long input must produce multiple windows");
        let target = coalesce_target(&cfg);
        for c in &chunks {
            assert!(c.fallback_used);
            // Within budget OR a single line that alone exceeds it.
            let single_line = c.content.trim_end().lines().count() == 1;
            assert!(c.token_count <= target || single_line);
        }
        // Disjoint and contiguous in document order.
        for w in chunks.windows(2) {
            assert!(w[1].start_byte >= w[0].end_byte, "windows must not overlap");
        }
    }

    #[test]
    fn empty_input_no_chunks() {
        assert!(LineWindowChunker
            .chunk("  \n ", &ChunkerConfig::default())
            .unwrap()
            .is_empty());
    }
}

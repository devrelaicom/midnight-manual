//! Markdown chunker — heading-based splits with a token-budgeted window fallback.
//!
//! Heading-based strategy (FR-007): every H1-H6 heading begins a new chunk.
//! Chunks carry a `heading_path` of their ancestor headings so the caller can
//! reconstruct the document outline. Chunks longer than `max_tokens` are split
//! further via the fallback windowing.
//!
//! Fallback strategy (EC-07): documents with no headings fall through to a
//! token-counted line-growth window with overlap. Defaults driven by
//! `ChunkerConfig::default()` (400 tokens max, 20-line overlap).
//!
//! Tokenization uses the real BPE tokenizer via `crate::tokens::count` so
//! chunk-size gating matches what the embedder actually sees.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};

/// The stateless markdown chunker. Construct with `MarkdownChunker` and call
/// via the [`Chunker`] trait.
pub struct MarkdownChunker;

impl Chunker for MarkdownChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }

        // ── First pass: walk the parser and emit heading-bounded segments ──
        let mut segments: Vec<HeadingSegment> = Vec::new();
        let mut current = HeadingSegment {
            start: 0,
            end: 0,
            heading_path: Vec::new(),
        };
        let mut stack: Vec<(HeadingLevel, String)> = Vec::new();
        let mut in_heading: Option<HeadingLevel> = None;
        let mut heading_buf = String::new();

        let parser = Parser::new_ext(body, Options::all()).into_offset_iter();
        for (event, range) in parser {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    if range.start > current.start {
                        current.end = range.start;
                        segments.push(current.clone());
                    }
                    // Same-level (or higher-level) entries are siblings or aunts,
                    // not ancestors, of this new heading. Pop them BEFORE snapshotting
                    // so heading_path reflects only true ancestors.
                    while stack.last().is_some_and(|(l, _)| *l >= level) {
                        stack.pop();
                    }
                    in_heading = Some(level);
                    heading_buf.clear();
                    current = HeadingSegment {
                        start: range.start,
                        end: range.start,
                        heading_path: stack.iter().map(|(_, t)| t.clone()).collect(),
                    };
                }
                Event::Text(t) if in_heading.is_some() => heading_buf.push_str(&t),
                Event::Code(t) if in_heading.is_some() => heading_buf.push_str(&t),
                Event::End(_) if in_heading.is_some() => {
                    let level = in_heading.take().expect("inside heading");
                    stack.push((level, heading_buf.trim().to_owned()));
                    // Don't mutate current.heading_path here: the heading is the
                    // segment's OWN heading, not an ancestor.
                }
                _ => {}
            }
        }
        current.end = body.len();
        if current.end > current.start {
            segments.push(current);
        }

        // ── Second pass: split over-large segments via token-budgeted line windows ──
        let mut chunks: Vec<Chunk> = Vec::new();
        for seg in segments {
            let text = &body[seg.start..seg.end];
            if text.trim().is_empty() {
                continue;
            }
            if crate::tokens::count(text) <= cfg.max_tokens {
                chunks.push(Chunk {
                    content: text.to_owned(),
                    heading_path: seg.heading_path.clone(),
                    symbol_path: Vec::new(),
                    start_byte: seg.start,
                    end_byte: seg.end,
                    token_count: crate::tokens::count(text),
                    chunk_index: 0, // filled in below
                    fallback_used: false,
                });
            } else {
                for window in token_window_split(text, seg.start, &seg.heading_path, cfg) {
                    chunks.push(window);
                }
            }
        }

        // Assign sequential chunk indices.
        for (i, c) in chunks.iter_mut().enumerate() {
            c.chunk_index = u32::try_from(i).unwrap_or(u32::MAX);
        }
        Ok(chunks)
    }
}

/// Convenience wrapper retained for the planner. Markdown chunking never errors
/// (it recovers internally), so this unwraps to an empty vec on the impossible
/// error path.
#[must_use]
pub fn chunk_markdown(body: &str, cfg: ChunkerConfig) -> Vec<Chunk> {
    MarkdownChunker.chunk(body, &cfg).unwrap_or_default()
}

#[derive(Debug, Clone)]
struct HeadingSegment {
    start: usize,
    end: usize,
    heading_path: Vec<String>,
}

/// Split `text` into overlapping token-budgeted windows, growing line by line.
///
/// `base_offset` is the byte offset of `text[0]` within the original document,
/// used to compute absolute `start_byte`/`end_byte` on each emitted `Chunk`.
///
/// The algorithm:
/// 1. Collect all lines from `text`.
/// 2. Grow a window by appending lines until adding the next line would push the
///    token count over `cfg.max_tokens`.
/// 3. Emit the window as a `Chunk`.
/// 4. Step back by `cfg.fallback_overlap_lines` lines to create overlap, then
///    continue from that position.
/// 5. A single line that exceeds the budget on its own is emitted whole (no
///    infinite loop).
fn token_window_split(
    text: &str,
    base_offset: usize,
    heading_path: &[String],
    cfg: &ChunkerConfig,
) -> Vec<Chunk> {
    // Collect (line_str, absolute_start_byte_within_text).
    let mut line_starts: Vec<usize> = Vec::new();
    let mut pos = 0usize;
    for line in text.split('\n') {
        line_starts.push(pos);
        // +1 for the '\n' that split consumed (except possibly the very last line)
        pos += line.len() + 1;
    }
    let n_lines = line_starts.len();
    if n_lines == 0 {
        return Vec::new();
    }
    // End byte of line i within `text` (exclusive, capped at text.len()).
    let line_end = |i: usize| -> usize {
        if i + 1 < n_lines {
            // Start of next line (which includes the '\n' we consumed)
            line_starts[i + 1]
        } else {
            text.len()
        }
    };

    let overlap_lines = usize::try_from(cfg.fallback_overlap_lines).unwrap_or(usize::MAX);
    let mut out: Vec<Chunk> = Vec::new();
    let mut window_start_line = 0usize;

    while window_start_line < n_lines {
        // Grow: find the largest end (exclusive) such that lines[start..end] fits
        // the token budget.  We always include at least one line so that a single
        // over-budget line is emitted whole (no infinite loop).
        let mut end_line = window_start_line + 1; // at least one line
        while end_line < n_lines {
            let slice = &text[line_starts[window_start_line]..line_end(end_line)];
            if crate::tokens::count(slice) > cfg.max_tokens {
                // Adding line `end_line` would overflow — stop before it.
                break;
            }
            end_line += 1;
        }
        // lines [window_start_line ..= end_line-1] is the in-budget window
        // (or a single over-budget line when end_line == window_start_line + 1).
        let last_line = end_line - 1;
        let slice_start = line_starts[window_start_line];
        let slice_end = line_end(last_line);
        let slice = &text[slice_start..slice_end];

        // Guard: never emit empty slices.
        if !slice.trim().is_empty() {
            let tok = crate::tokens::count(slice);
            out.push(Chunk {
                content: slice.to_owned(),
                heading_path: heading_path.to_vec(),
                symbol_path: Vec::new(),
                start_byte: base_offset + slice_start,
                end_byte: base_offset + slice_end,
                token_count: tok,
                chunk_index: 0, // caller will renumber
                fallback_used: false,
            });
        }

        // Step the start forward, then back by overlap, ensuring forward progress.
        let lines_in_window = last_line - window_start_line + 1;
        let step = lines_in_window.saturating_sub(overlap_lines).max(1);
        window_start_line += step;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkerConfig;

    // ── helpers ─────────────────────────────────────────────────────────────

    fn default_cfg() -> ChunkerConfig {
        ChunkerConfig::default()
    }

    fn small_cfg() -> ChunkerConfig {
        // 50-token budget — small enough that a few hundred-word body splits.
        ChunkerConfig {
            max_tokens: 50,
            ..ChunkerConfig::default()
        }
    }

    // ── tests ────────────────────────────────────────────────────────────────

    #[test]
    fn empty_input_produces_no_chunks() {
        let chunks = MarkdownChunker.chunk("   \n\t  ", &default_cfg()).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_heading_produces_one_chunk() {
        let md = "# Title\n\nBody text here.";
        let chunks = MarkdownChunker.chunk(md, &default_cfg()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_index, 0);
        assert!(chunks[0].content.contains("Body text"));
        assert!(chunks[0].heading_path.is_empty());
        // Markdown chunks must have empty symbol_path and fallback_used=false.
        assert!(chunks[0].symbol_path.is_empty());
        assert!(!chunks[0].fallback_used);
    }

    #[test]
    fn nested_headings_record_path() {
        let md = "# Top\n\nintro\n\n## Sub A\n\ncontent A\n\n## Sub B\n\ncontent B\n";
        let chunks = MarkdownChunker.chunk(md, &default_cfg()).unwrap();
        assert_eq!(chunks.len(), 3);
        // First chunk is from `# Top` heading; its path is empty (it IS Top).
        assert_eq!(chunks[1].heading_path, vec!["Top".to_string()]);
        assert_eq!(chunks[2].heading_path, vec!["Top".to_string()]);
    }

    #[test]
    fn over_sized_chunk_falls_back_to_window() {
        // Build a segment whose token count comfortably exceeds max_tokens=50.
        // Each line is ~10 tokens; 20 lines * 10 ≈ 200 tokens >> 50.
        let line = "the quick brown fox jumps over the lazy dog near the river\n";
        let big_body: String = line.repeat(20);
        let md = format!("# Title\n\n{big_body}");
        let chunks = MarkdownChunker.chunk(&md, &small_cfg()).unwrap();
        assert!(chunks.len() > 1, "oversized chunk must split into windows");
        let cfg = small_cfg();
        for c in &chunks {
            // Every chunk must either fit within the token budget, OR be a single
            // line that exceeds the budget on its own (unavoidable — can't split
            // finer than one line).
            let single_line = c.content.trim_end().lines().count() == 1;
            assert!(
                c.token_count <= cfg.max_tokens || single_line,
                "chunk token_count={} exceeds budget={} and is not a single line",
                c.token_count,
                cfg.max_tokens
            );
        }
        // Verify the trait contract: markdown windowing sets fallback_used=false
        // and symbol_path is empty.
        for c in &chunks {
            assert!(!c.fallback_used);
            assert!(c.symbol_path.is_empty());
        }
    }

    #[test]
    fn windows_overlap() {
        // Long body of many lines to guarantee multiple windows.
        let line = "the quick brown fox jumps over the lazy dog near the river bank\n";
        let body: String = line.repeat(30);
        let md = format!("# X\n\n{body}");
        let chunks = MarkdownChunker.chunk(&md, &small_cfg()).unwrap();
        assert!(chunks.len() >= 2, "expected multiple windows, got {}", chunks.len());
        // Adjacent windows must overlap: chunks[1].start_byte < chunks[0].end_byte.
        assert!(
            chunks[1].start_byte < chunks[0].end_byte,
            "chunks[1].start_byte={} should be < chunks[0].end_byte={} (overlap required)",
            chunks[1].start_byte,
            chunks[0].end_byte,
        );
    }

    #[test]
    fn headingless_document_produces_chunks() {
        let md = "Just a plain paragraph with no headings.\n\nAnother one.";
        let chunks = MarkdownChunker.chunk(md, &default_cfg()).unwrap();
        assert_eq!(chunks.len(), 1, "small heading-less doc fits in one chunk");
        assert!(chunks[0].heading_path.is_empty());
    }

    #[test]
    fn chunk_indices_are_sequential() {
        let md = "# A\n\ntext\n\n# B\n\ntext\n\n# C\n\ntext\n";
        let chunks = MarkdownChunker.chunk(md, &default_cfg()).unwrap();
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.chunk_index, u32::try_from(i).unwrap());
        }
    }
}

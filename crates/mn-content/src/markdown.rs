//! Markdown chunker — heading-based splits with a fixed-window fallback.
//!
//! Heading-based strategy (FR-007): every H1-H6 heading begins a new chunk.
//! Chunks carry a `heading_path` of their ancestor headings so the caller can
//! reconstruct the document outline. Chunks longer than `max_tokens` are split
//! further via the fallback windowing.
//!
//! Fallback strategy (EC-07): documents with no headings fall through to a
//! fixed-token-count window with overlap (defaults: 800 tokens, 100-token overlap).
//!
//! Tokenization here is byte-count-based — fast and good enough for chunk-size
//! gating. The real embedding tokenizer (BPE in `mn-embedding`) is used at
//! embed time.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};
use serde::{Deserialize, Serialize};

/// One chunk emitted by [`chunk_markdown`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownChunk {
    /// Verbatim chunk content (Markdown, including the leading heading line
    /// that began the chunk where applicable).
    pub content: String,
    /// Ancestor heading text from root → immediate parent. Empty for
    /// pre-heading content or for heading-less documents.
    pub heading_path: Vec<String>,
    /// Byte offset of the chunk's first character in the source document.
    pub start_byte: usize,
    /// Byte offset just past the chunk's last character.
    pub end_byte: usize,
    /// 0-indexed position among the document's chunks.
    pub chunk_index: u32,
}

/// Configuration for the chunker.
#[derive(Debug, Clone, Copy)]
pub struct ChunkerConfig {
    /// Maximum chunk size in bytes before the fallback window kicks in.
    pub max_bytes: usize,
    /// Fixed-window size for heading-less / over-sized chunks (bytes).
    pub window_bytes: usize,
    /// Overlap between adjacent windows (bytes).
    pub overlap_bytes: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        // Defaults from spec EC-07: 800 tokens ≈ 3200 bytes with avg 4-byte
        // tokens, 100-token overlap ≈ 400 bytes. Keeping byte units throughout
        // the chunker for predictability.
        Self {
            max_bytes: 4096,
            window_bytes: 3200,
            overlap_bytes: 400,
        }
    }
}

/// Split a Markdown body into chunks. Returns at least one chunk for any
/// non-empty input.
#[must_use]
pub fn chunk_markdown(body: &str, cfg: ChunkerConfig) -> Vec<MarkdownChunk> {
    if body.trim().is_empty() {
        return Vec::new();
    }

    // First pass: walk the parser and emit heading-bounded segments.
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

    // Second pass: split over-large segments via the fixed-window fallback.
    let mut chunks: Vec<MarkdownChunk> = Vec::new();
    for seg in segments {
        let text = &body[seg.start..seg.end];
        if text.trim().is_empty() {
            continue;
        }
        if text.len() <= cfg.max_bytes {
            chunks.push(MarkdownChunk {
                content: text.to_owned(),
                heading_path: seg.heading_path.clone(),
                start_byte: seg.start,
                end_byte: seg.end,
                chunk_index: 0, // filled in below
            });
        } else {
            for window in window_split(text, seg.start, cfg.window_bytes, cfg.overlap_bytes) {
                chunks.push(MarkdownChunk {
                    content: window.content,
                    heading_path: seg.heading_path.clone(),
                    start_byte: window.start_byte,
                    end_byte: window.end_byte,
                    chunk_index: 0,
                });
            }
        }
    }

    // Heading-less documents produce a single oversized segment; if the
    // document has no chunks (all-whitespace), early-returned above.
    for (i, c) in chunks.iter_mut().enumerate() {
        c.chunk_index = u32::try_from(i).unwrap_or(u32::MAX);
    }
    chunks
}

#[derive(Debug, Clone)]
struct HeadingSegment {
    start: usize,
    end: usize,
    heading_path: Vec<String>,
}

struct Window {
    content: String,
    start_byte: usize,
    end_byte: usize,
}

/// Yields overlapping fixed-size windows over `text`. Returned slices respect
/// UTF-8 character boundaries.
fn window_split(text: &str, base_offset: usize, window: usize, overlap: usize) -> Vec<Window> {
    if text.len() <= window {
        return vec![Window {
            content: text.to_owned(),
            start_byte: base_offset,
            end_byte: base_offset + text.len(),
        }];
    }
    let step = window.saturating_sub(overlap).max(1);
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + window).min(text.len());
        // Snap end to char boundary
        let end_snapped = char_boundary_at_or_below(text, end);
        // Snap start to char boundary
        let start_snapped = char_boundary_at_or_below(text, start);
        out.push(Window {
            content: text[start_snapped..end_snapped].to_owned(),
            start_byte: base_offset + start_snapped,
            end_byte: base_offset + end_snapped,
        });
        if end_snapped == text.len() {
            break;
        }
        start = start_snapped.saturating_add(step);
    }
    out
}

const fn char_boundary_at_or_below(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_produces_no_chunks() {
        let chunks = chunk_markdown("   \n\t  ", ChunkerConfig::default());
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_heading_produces_one_chunk() {
        let md = "# Title\n\nBody text here.";
        let chunks = chunk_markdown(md, ChunkerConfig::default());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_index, 0);
        assert!(chunks[0].content.contains("Body text"));
        assert!(chunks[0].heading_path.is_empty());
    }

    #[test]
    fn nested_headings_record_path() {
        let md = "# Top\n\nintro\n\n## Sub A\n\ncontent A\n\n## Sub B\n\ncontent B\n";
        let chunks = chunk_markdown(md, ChunkerConfig::default());
        assert_eq!(chunks.len(), 3);
        // First chunk is from `# Top` heading; its path is empty (it IS Top).
        assert_eq!(chunks[1].heading_path, vec!["Top".to_string()]);
        assert_eq!(chunks[2].heading_path, vec!["Top".to_string()]);
    }

    #[test]
    fn over_sized_chunk_falls_back_to_window() {
        let big = "x".repeat(10_000);
        let md = format!("# Title\n\n{big}");
        let cfg = ChunkerConfig {
            max_bytes: 1000,
            window_bytes: 800,
            overlap_bytes: 100,
        };
        let chunks = chunk_markdown(&md, cfg);
        assert!(chunks.len() > 1, "oversized chunk must split into windows");
        for c in &chunks {
            assert!(c.content.len() <= cfg.window_bytes);
        }
    }

    #[test]
    fn windows_overlap() {
        let text: String = (0..2000)
            .map(|i| (b'a' + u8::try_from(i % 26).unwrap()) as char)
            .collect();
        let cfg = ChunkerConfig {
            max_bytes: 500,
            window_bytes: 400,
            overlap_bytes: 100,
        };
        let chunks = chunk_markdown(&format!("# X\n\n{text}"), cfg);
        assert!(chunks.len() >= 2);
        // Adjacent windows must overlap by ~ overlap_bytes
        if chunks.len() >= 2 {
            // The body offset within the original (after `# X\n\n`) is 5.
            let body_start = chunks[0].start_byte;
            assert!(chunks[1].start_byte > body_start);
            let gap = chunks[1].start_byte.saturating_sub(chunks[0].start_byte);
            assert!(gap < cfg.window_bytes, "windows must overlap, gap={gap}");
        }
    }

    #[test]
    fn headingless_document_produces_chunks() {
        let md = "Just a plain paragraph with no headings.\n\nAnother one.";
        let chunks = chunk_markdown(md, ChunkerConfig::default());
        assert_eq!(chunks.len(), 1, "small heading-less doc fits in one chunk");
        assert!(chunks[0].heading_path.is_empty());
    }

    #[test]
    fn chunk_indices_are_sequential() {
        let md = "# A\n\ntext\n\n# B\n\ntext\n\n# C\n\ntext\n";
        let chunks = chunk_markdown(md, ChunkerConfig::default());
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.chunk_index, u32::try_from(i).unwrap());
        }
    }
}

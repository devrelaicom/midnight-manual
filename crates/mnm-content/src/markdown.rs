//! Markdown chunker — heading splits + greedy coalescing + non-overlapping
//! window splits.
//!
//! Heading-based strategy (FR-007): every H1-H6 heading begins a new segment.
//! Chunks carry a `heading_path` of their ancestor headings so the caller can
//! reconstruct the document outline.
//!
//! Coalescing pass (D2): adjacent segments pack greedily up to
//! [`crate::chunk::coalesce_target`] (90% of `max_tokens`), never absorbing a
//! segment shallower than the run anchor. Segments still larger than
//! `max_tokens` are split via a token-budgeted line window.
//!
//! Every emitted chunk's byte range is disjoint from every other chunk's (D3):
//! no window overlap, no context expansion — surrounding context is supplied at
//! embedding time by contextualized embeddings, not by inflating chunk ranges.
//!
//! Tokenization uses the real BPE tokenizer via `crate::tokens::count` so
//! chunk-size gating matches what the embedder actually sees.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

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
            // Preamble before the first heading has no heading depth.
            depth: 0,
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
                        depth: heading_depth(level),
                    };
                }
                Event::Text(t) if in_heading.is_some() => heading_buf.push_str(&t),
                Event::Code(t) if in_heading.is_some() => heading_buf.push_str(&t),
                // Close only on the heading's OWN end tag. Matching `Event::End(_)`
                // would close early on the first inline tag end (emphasis, strong,
                // link…), truncating the captured heading text at that point.
                Event::End(TagEnd::Heading(_)) if in_heading.is_some() => {
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

        // ── Coalescing pass: greedily pack adjacent sections to the 90% target ──
        let segments = coalesce_segments(body, &segments, cfg);

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

#[derive(Debug, Clone)]
struct HeadingSegment {
    start: usize,
    end: usize,
    heading_path: Vec<String>,
    /// Heading depth: 0 = preamble/no-heading, 1..=6 = H1..H6. Drives the
    /// coalescing pass's "never absorb a shallower section" rule.
    depth: u8,
}

/// Map a `pulldown_cmark::HeadingLevel` to a 1-based depth (H1 => 1 .. H6 => 6).
///
/// Done with an explicit match rather than `as u8` so the mapping is auditable
/// and independent of the enum's discriminant layout.
const fn heading_depth(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Level-aware greedy coalescing pass (FR-007 small-section merge, D2).
///
/// Walks the first-pass `segments` and greedily packs adjacent sections into
/// runs up to [`crate::chunk::coalesce_target`] (90% of `max_tokens`). A run
/// absorbs its descendants and same-level siblings but never a *shallower*
/// section (that would pull the run up out of its subtree). A section that is
/// already larger than the target on its own passes through alone (and is
/// windowed later by [`token_window_split`] if it also exceeds `max_tokens`).
/// The merged run keeps the FIRST segment's `heading_path` (shallowest
/// ancestor context) and `depth`.
fn coalesce_segments(
    body: &str,
    segments: &[HeadingSegment],
    cfg: &ChunkerConfig,
) -> Vec<HeadingSegment> {
    let target = crate::chunk::coalesce_target(cfg);
    let mut out: Vec<HeadingSegment> = Vec::new();
    let mut i = 0usize;
    while i < segments.len() {
        let start = segments[i].start;
        let start_depth = segments[i].depth;
        let head_path = segments[i].heading_path.clone();
        let mut end = segments[i].end;
        let mut j = i + 1;
        while j < segments.len() {
            let next = &segments[j];
            // Structural: never absorb a segment SHALLOWER than the run anchor
            // (don't pull the run up into an aunt/ancestor section).
            if next.depth < start_depth {
                break;
            }
            // Greedy fill (D2): stop when absorbing the next section would push
            // the run past the 90% target. A single >target segment passes
            // through alone (windowed later).
            if crate::tokens::count(&body[start..next.end]) > target {
                break;
            }
            end = next.end;
            j += 1;
        }
        out.push(HeadingSegment {
            start,
            end,
            heading_path: head_path,
            depth: start_depth,
        });
        i = j;
    }
    out
}

/// Split `text` into disjoint token-budgeted windows, growing line by line.
///
/// `base_offset` is the byte offset of `text[0]` within the original document,
/// used to compute absolute `start_byte`/`end_byte` on each emitted `Chunk`.
///
/// The algorithm:
/// 1. Collect all lines from `text`.
/// 2. Grow a window by appending lines until adding the next line would push
///    the token count over [`crate::chunk::coalesce_target`] (90% of
///    `max_tokens`, so split windows aim at the same fill target as
///    coalescing).
/// 3. Emit the window as a `Chunk`, then continue from the next line —
///    windows never overlap (D3).
/// 4. A single line that exceeds the budget on its own is emitted whole (no
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

    let target = crate::chunk::coalesce_target(cfg);
    let mut out: Vec<Chunk> = Vec::new();
    let mut window_start_line = 0usize;

    while window_start_line < n_lines {
        // Grow: find the largest end (exclusive) such that lines[start..end] fits
        // the token budget.  We always include at least one line so that a single
        // over-budget line is emitted whole (no infinite loop).
        let mut end_line = window_start_line + 1; // at least one line
        while end_line < n_lines {
            let slice = &text[line_starts[window_start_line]..line_end(end_line)];
            if crate::tokens::count(slice) > target {
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

        // Continue from the line after this window — no overlap (D3).
        window_start_line = last_line + 1;
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

    /// Coalescing-suppressing config for per-heading assertions: a tiny budget
    /// whose 90% target is smaller than any two adjacent test sections combined,
    /// so each heading stays its own chunk (and sections stay under `max_tokens`
    /// so no window split kicks in either).
    fn per_section_cfg() -> ChunkerConfig {
        ChunkerConfig {
            max_tokens: 28,
            ..ChunkerConfig::default()
        }
    }

    fn small_cfg() -> ChunkerConfig {
        // 50-token budget — small enough that a few hundred-word body splits.
        ChunkerConfig {
            max_tokens: 50,
            ..ChunkerConfig::default()
        }
    }

    // ── tests ────────────────────────────────────────────────────────────────

    /// THE no-overlap invariant (D3): every emitted chunk's byte range is
    /// disjoint from every other chunk's, in document order.
    #[test]
    fn markdown_chunks_never_overlap() {
        use std::fmt::Write as _;

        let line = "the quick brown fox jumps over the lazy dog near the river bank.\n";
        let mut md = String::from("# Top\n\nintro paragraph.\n\n");
        for h in ["A", "B", "C", "D", "E", "F"] {
            write!(md, "## Section {h}\n\n{}", line.repeat(8)).unwrap();
        }
        let cfg = ChunkerConfig {
            max_tokens: 64,
            ..ChunkerConfig::default()
        };
        let chunks = MarkdownChunker.chunk(&md, &cfg).unwrap();
        assert!(chunks.len() >= 2);
        for w in chunks.windows(2) {
            assert!(
                w[1].start_byte >= w[0].end_byte,
                "overlap: [{}, {}) then [{}, {})",
                w[0].start_byte,
                w[0].end_byte,
                w[1].start_byte,
                w[1].end_byte
            );
        }
    }

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
        // Under the default budget these sections would greedily coalesce into
        // a single chunk; `per_section_cfg()` keeps the per-heading semantics
        // this test asserts (one chunk per heading + ancestor heading_path):
        // each ~15-token section fits the 28-token budget alone, but two
        // adjacent sections exceed the 25-token coalesce target.
        let md = "# Top\n\nthis section body has roughly fifteen tokens of filler text here\n\n\
                  ## Sub A\n\nthis section body has roughly fifteen tokens of filler text here\n\n\
                  ## Sub B\n\nthis section body has roughly fifteen tokens of filler text here\n";
        let chunks = MarkdownChunker.chunk(md, &per_section_cfg()).unwrap();
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
    fn headingless_document_produces_chunks() {
        let md = "Just a plain paragraph with no headings.\n\nAnother one.";
        let chunks = MarkdownChunker.chunk(md, &default_cfg()).unwrap();
        assert_eq!(chunks.len(), 1, "small heading-less doc fits in one chunk");
        assert!(chunks[0].heading_path.is_empty());
    }

    #[test]
    fn chunk_indices_are_sequential() {
        // `per_section_cfg()` so the three `# A/# B/# C` sections stay as
        // three distinct chunks (each fits the 28-token budget alone; two
        // adjacent ones exceed the 25-token coalesce target) and the
        // sequential-index contract is exercised across more than one chunk.
        let md = "# A\n\nthis section body has roughly fifteen tokens of filler text here\n\n\
                  # B\n\nthis section body has roughly fifteen tokens of filler text here\n\n\
                  # C\n\nthis section body has roughly fifteen tokens of filler text here\n";
        let chunks = MarkdownChunker.chunk(md, &per_section_cfg()).unwrap();
        assert!(chunks.len() >= 2, "expected multiple chunks, got {}", chunks.len());
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.chunk_index, u32::try_from(i).unwrap());
        }
    }

    // ── heading inline-formatting capture (#168) ────────────────────────────────

    /// #168 regression: a heading containing an inline element (`**bold**`) must
    /// record its FULL text in descendants' `heading_path`, not stop at the first
    /// inline tag's `End` event. Before the fix this recorded `"Using bold"`,
    /// dropping `" in practice"`.
    #[test]
    fn heading_with_inline_emphasis_captures_full_text() {
        // `per_section_cfg()` keeps the H1 and its H2 child as separate chunks
        // (each ~15-token body fits the 28-token budget alone; the pair exceeds
        // the 25-token coalesce target), so the child records the H1 as an
        // ancestor in its `heading_path`.
        let md = "# Using **bold** in practice\n\n\
                  this section body has roughly fifteen tokens of filler text here\n\n\
                  ## child section\n\n\
                  this section body has roughly fifteen tokens of filler text here\n";
        let chunks = MarkdownChunker.chunk(md, &per_section_cfg()).unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.heading_path.iter().any(|h| h == "Using bold in practice")),
            "expected un-truncated heading 'Using bold in practice' in a heading_path, got {:#?}",
            chunks.iter().map(|c| &c.heading_path).collect::<Vec<_>>()
        );
    }

    /// Same defect via an inline link: `## The [API](url) reference` must capture
    /// `"The API reference"`, not truncate at the link's `End` event.
    #[test]
    fn heading_with_inline_link_captures_full_text() {
        let md = "# The [API](https://example.test) reference\n\n\
                  this section body has roughly fifteen tokens of filler text here\n\n\
                  ## child section\n\n\
                  this section body has roughly fifteen tokens of filler text here\n";
        let chunks = MarkdownChunker.chunk(md, &per_section_cfg()).unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.heading_path.iter().any(|h| h == "The API reference")),
            "expected un-truncated heading 'The API reference' in a heading_path, got {:#?}",
            chunks.iter().map(|c| &c.heading_path).collect::<Vec<_>>()
        );
    }

    // ── coalescing tests (the bug fix) ─────────────────────────────────────────

    /// A real-sentence body so each section is small-but-not-trivial; orphan
    /// parent heading `## Constructors` must NOT survive as its own chunk under
    /// the default soft floor — it merges down with `### new()`.
    #[test]
    fn orphan_parent_heading_merges_with_child() {
        let md = "# Counter\n\n## Constructors\n\n### new()\n\n\
                  Creates a fresh counter initialised to zero for the caller.\n";
        let chunks = MarkdownChunker.chunk(md, &default_cfg()).unwrap();

        // No chunk is *just* the orphan parent heading.
        assert!(
            chunks.iter().all(|c| c.content.trim() != "## Constructors"),
            "## Constructors should not be a standalone chunk: {:#?}",
            chunks.iter().map(|c| c.content.trim()).collect::<Vec<_>>()
        );
        // The orphan heading co-occurs with its child in one merged chunk.
        assert!(
            chunks
                .iter()
                .any(|c| c.content.contains("## Constructors") && c.content.contains("new()")),
            "## Constructors and new() should share a chunk: {:#?}",
            chunks.iter().map(|c| c.content.trim()).collect::<Vec<_>>()
        );
    }

    /// Tiny leaf sections (heading + one-word type) must merge — no ~6-token
    /// heading-only chunk like a lone `#### rand`.
    #[test]
    fn tiny_leaf_sections_merge() {
        let md = "## Properties\n\n#### rand\n\n`string`\n\n#### seed\n\n`number`\n";
        let chunks = MarkdownChunker.chunk(md, &default_cfg()).unwrap();

        for c in &chunks {
            let trimmed = c.content.trim();
            // No standalone heading-only chunk.
            assert!(
                !(trimmed.starts_with('#') && trimmed.lines().count() == 1),
                "found a standalone heading-only chunk: {trimmed:?}"
            );
            // Nothing as tiny as the orphan leaf sections we are fixing.
            assert!(
                c.token_count > 8,
                "chunk is still sub-useful ({} tokens): {trimmed:?}",
                c.token_count
            );
        }
    }

    /// Flat same-level siblings (no parent wrapper) collapse into fewer chunks
    /// than there are members.
    #[test]
    fn flat_sibling_members_coalesce() {
        let md = "### alpha\n\nThe alpha member does the first thing for callers.\n\n\
                  ### beta\n\nThe beta member does the second thing for callers.\n\n\
                  ### gamma\n\nThe gamma member does the third thing for callers.\n\n\
                  ### delta\n\nThe delta member does the fourth thing for callers.\n";
        let members = 4;
        let chunks = MarkdownChunker.chunk(md, &default_cfg()).unwrap();
        assert!(
            chunks.len() < members,
            "expected coalescing below {members} members, got {} chunks",
            chunks.len()
        );
    }

    /// A single section larger than `max_tokens` still splits via windowing;
    /// every chunk is within budget or is a single over-budget line.
    #[test]
    fn large_single_section_still_windows() {
        let line = "the quick brown fox jumps over the lazy dog near the river bank\n";
        let body: String = line.repeat(30);
        let md = format!("# Big\n\n{body}");
        let cfg = small_cfg();
        let chunks = MarkdownChunker.chunk(&md, &cfg).unwrap();
        assert!(chunks.len() > 1, "oversized section must window-split");
        for c in &chunks {
            let single_line = c.content.trim_end().lines().count() == 1;
            assert!(
                c.token_count <= cfg.max_tokens || single_line,
                "chunk token_count={} exceeds max={} and is not a single line",
                c.token_count,
                cfg.max_tokens
            );
        }
    }

    /// A merged run's `heading_path` is the FIRST segment's ancestor path.
    /// Under greedy packing the `# Top` intro absorbs `## A` + `### a1` (depth
    /// ≥ anchor, all tiny), so the single merged chunk keeps the anchor's path
    /// (`[]`, the preamble anchored at `# Top`) — NOT a deeper child's path.
    #[test]
    fn merged_chunk_uses_first_segments_path() {
        let md = "# Top\n\nintro.\n\n## A\n\nshort\n\n### a1\n\nalso short\n";
        let chunks = MarkdownChunker
            .chunk(md, &ChunkerConfig::default())
            .unwrap();
        assert_eq!(chunks.len(), 1, "tiny sections all pack into the anchor run");
        assert!(chunks[0].content.contains("### a1"));
        assert!(chunks[0].heading_path.is_empty(), "run keeps the FIRST segment's path");
    }

    /// Structural invariants on every emitted chunk: non-empty byte range
    /// within bounds and sequential indices.
    #[test]
    fn coalesced_chunks_have_valid_offsets_and_indices() {
        let md = "# Doc\n\n## One\n\nbody one\n\n## Two\n\nbody two\n\n## Three\n\nbody three\n";
        let chunks = MarkdownChunker.chunk(md, &default_cfg()).unwrap();
        for (i, c) in chunks.iter().enumerate() {
            assert!(c.start_byte < c.end_byte, "start_byte >= end_byte: {c:?}");
            assert!(c.end_byte <= md.len(), "end_byte past body len: {c:?}");
            assert_eq!(c.chunk_index, u32::try_from(i).unwrap());
        }
    }
}

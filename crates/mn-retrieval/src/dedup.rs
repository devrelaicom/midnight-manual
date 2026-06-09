//! Result-set overlap dedup.
//!
//! When several retrieved chunks come from the same document their stored
//! `content` can overlap (the markdown chunker embeds a rolling context window,
//! so neighbouring chunks share text). This pass walks results in rank order and
//! removes already-shown text: fully-covered chunks are dropped, partially-
//! covered chunks have the duplicated span trimmed out (`…` marks the cut).
//!
//! Pure and storage-agnostic: callers implement [`OverlapItem`]. Only
//! *byte-aligned* content (where `content.len() == end - start`, i.e. verbatim
//! `body[start..end]`) is trimmed; content carrying a synthetic prefix (e.g. a
//! code breadcrumb) is left intact but still contributes its byte span.

use std::collections::HashMap;
use std::hash::Hash;

/// Marker inserted between non-contiguous kept spans of a trimmed item.
const ELISION: &str = "\n…\n";

/// A retrieval result that can be overlap-deduplicated.
///
/// # Contract for safe trimming
///
/// Trimming relies on byte offsets being meaningful against [`content`]:
/// - When `content().len() == end - start` (the "byte-aligned" case), the
///   content MUST be exactly the verbatim document slice `body[start..end]`.
///   Trimming maps covered document bytes to content offsets directly, so a
///   same-length-but-non-verbatim content would have genuinely-new bytes
///   discarded. Content that is *not* a verbatim slice (e.g. carries a synthetic
///   prefix such as a code breadcrumb) is simply longer than the span, fails the
///   length check, and is left untouched — which is correct.
/// - `byte_range()` offsets MUST fall on UTF-8 char boundaries of the document.
///   They do for the intended source (chunk `start_byte`/`end_byte`, derived from
///   verbatim byte slices); a mismatch would silently drop the affected fragment.
///
/// [`content`]: OverlapItem::content
pub trait OverlapItem {
    /// Document grouping key. Only items sharing a key are compared.
    type Key: Eq + Hash;

    /// The document key for this item.
    fn document_key(&self) -> Self::Key;
    /// Byte range `[start, end)` of this item within its document. Offsets must
    /// be UTF-8 char boundaries (see the trait-level contract).
    fn byte_range(&self) -> (usize, usize);
    /// The item's current text content. When byte-aligned, must be verbatim
    /// `body[start..end]` (see the trait-level contract).
    fn content(&self) -> &str;
    /// Replace the item's content (used when trimming overlap).
    fn set_content(&mut self, content: String);
}

/// Outcome counts from a dedup pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DedupStats {
    /// Items dropped because every byte was already shown by a higher-ranked
    /// same-document item (or because trimming left nothing but whitespace).
    pub dropped: usize,
    /// Items whose content was trimmed (some, not all, bytes already shown).
    pub trimmed: usize,
}

/// Trim overlapping text from same-document items.
///
/// Items are processed in the given order (callers pass results already sorted
/// best-rank-first). Returns the surviving items in their original order, plus
/// [`DedupStats`]. Cross-document items never interact; ordering is preserved.
#[must_use]
pub fn trim_overlaps<T: OverlapItem>(items: Vec<T>) -> (Vec<T>, DedupStats) {
    let mut covered: HashMap<T::Key, Vec<(usize, usize)>> = HashMap::new();
    let mut out = Vec::with_capacity(items.len());
    let mut stats = DedupStats::default();

    for mut item in items {
        let key = item.document_key();
        let (start, end) = item.byte_range();
        let intervals = covered.entry(key).or_default();

        let gaps = subtract(start, end, intervals);
        insert(intervals, start, end);

        if gaps.is_empty() {
            stats.dropped += 1;
            continue;
        }

        let aligned = item.content().len() == end.saturating_sub(start);
        let fully_uncovered = gaps.len() == 1 && gaps[0] == (start, end);
        if aligned && !fully_uncovered {
            let content = item.content();
            let mut kept = String::new();
            for (i, &(gs, ge)) in gaps.iter().enumerate() {
                if i > 0 {
                    kept.push_str(ELISION);
                }
                // Gap offsets are char boundaries per the trait contract
                // (byte_range derives from verbatim slices); `.get()` is a safety
                // net that skips a fragment rather than panicking if that ever
                // fails to hold.
                if let Some(slice) = content.get(gs - start..ge - start) {
                    kept.push_str(slice);
                }
            }
            // A trim that leaves only whitespace (e.g. a lone overlap-gap
            // newline) is not worth keeping — drop it.
            if kept.trim().is_empty() {
                stats.dropped += 1;
                continue;
            }
            item.set_content(kept);
            stats.trimmed += 1;
        }

        out.push(item);
    }

    (out, stats)
}

/// Uncovered sub-ranges of `[start, end)` given `covered` (need not be sorted).
fn subtract(start: usize, end: usize, covered: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if start >= end {
        return Vec::new();
    }
    let mut overlaps: Vec<(usize, usize)> = covered
        .iter()
        .copied()
        .filter(|&(s, e)| e > start && s < end)
        .collect();
    overlaps.sort_unstable();

    let mut gaps = Vec::new();
    let mut cursor = start;
    for (s, e) in overlaps {
        if s > cursor {
            gaps.push((cursor, s.min(end)));
        }
        cursor = cursor.max(e);
        if cursor >= end {
            break;
        }
    }
    if cursor < end {
        gaps.push((cursor, end));
    }
    gaps
}

/// Insert `[start, end)`, keeping `intervals` merged and sorted.
fn insert(intervals: &mut Vec<(usize, usize)>, start: usize, end: usize) {
    if start >= end {
        return;
    }
    intervals.push((start, end));
    intervals.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(intervals.len());
    for &(s, e) in intervals.iter() {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    *intervals = merged;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct R {
        doc: u32,
        s: usize,
        e: usize,
        c: String,
    }
    impl OverlapItem for R {
        type Key = u32;
        fn document_key(&self) -> u32 {
            self.doc
        }
        fn byte_range(&self) -> (usize, usize) {
            (self.s, self.e)
        }
        fn content(&self) -> &str {
            &self.c
        }
        fn set_content(&mut self, c: String) {
            self.c = c;
        }
    }
    fn r(doc: u32, s: usize, e: usize, c: &str) -> R {
        R { doc, s, e, c: c.into() }
    }

    #[test]
    fn trims_trailing_overlap_of_lower_ranked_neighbour() {
        let items = vec![r(1, 0, 10, "0123456789"), r(1, 5, 15, "56789ABCDE")];
        let (out, stats) = trim_overlaps(items);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].c, "0123456789");
        assert_eq!(out[1].c, "ABCDE");
        assert_eq!(stats, DedupStats { dropped: 0, trimmed: 1 });
    }

    #[test]
    fn drops_fully_covered_item() {
        let items = vec![r(1, 0, 10, "0123456789"), r(1, 2, 8, "234567")];
        let (out, stats) = trim_overlaps(items);
        assert_eq!(out.len(), 1);
        assert_eq!(stats.dropped, 1);
    }

    #[test]
    fn cross_document_never_interacts() {
        let items = vec![r(1, 0, 10, "0123456789"), r(2, 0, 10, "abcdefghij")];
        let (out, stats) = trim_overlaps(items);
        assert_eq!(out.len(), 2);
        assert_eq!(stats, DedupStats::default());
    }

    #[test]
    fn non_byte_aligned_content_is_left_intact() {
        let items = vec![
            r(1, 0, 10, "0123456789"),
            r(1, 5, 15, "// crumb\n56789ABCDE"),
        ];
        let (out, stats) = trim_overlaps(items);
        assert_eq!(out[1].c, "// crumb\n56789ABCDE");
        assert_eq!(stats.trimmed, 0);
    }

    #[test]
    fn middle_gap_uses_elision_marker() {
        // Two interior covered spans ([2,4) and [8,10)) leave the wide third
        // item with three non-contiguous gaps ([0,2), [4,8), [10,12)), so the
        // kept middle span "4567" is separated from its neighbours by an elision
        // marker.
        let items = vec![
            r(1, 2, 4, "23"),
            r(1, 8, 10, "89"),
            r(1, 0, 12, "0123456789AB"),
        ];
        let (out, _) = trim_overlaps(items);
        let third = &out[2];
        assert!(third.c.contains("4567"));
        assert!(third.c.contains('…'));
    }

    #[test]
    fn drops_item_when_trim_leaves_only_whitespace() {
        // [0,8) shown; next is [0,9) where the lone uncovered byte is a newline.
        let items = vec![r(1, 0, 8, "01234567"), r(1, 0, 9, "01234567\n")];
        let (out, stats) = trim_overlaps(items);
        assert_eq!(out.len(), 1, "whitespace-only remainder must be dropped");
        assert_eq!(stats.dropped, 1);
        assert_eq!(stats.trimmed, 0);
    }
}

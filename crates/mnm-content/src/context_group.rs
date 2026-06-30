//! Balanced context-group splitting (spec §6, D8).
//!
//! voyage-context-3 accepts ≤32 000 tokens per inner list (one document's
//! chunks embedded together). Documents over 80% of that limit are split into
//! the minimum number of contiguous, roughly-equal-token groups. Grouping only
//! changes what context Voyage sees — chunk rows are unaffected.
//!
//! Token counts here are OUR BPE counts (`crate::tokens`, a `bge-base-en-v1.5`
//! `WordPiece` vocabulary), NOT voyage-context-3's tokenizer, which counts more
//! tokens for code-dense text. The 20% headroom (25 600 vs 32 000) absorbs the
//! common divergence; the ingest CLI additionally retry-splits any group Voyage
//! still rejects as >32 000, so a worse-than-expected divergence degrades to a
//! finer split rather than a failed run.

use std::ops::Range;

/// Voyage's per-document (inner list) token limit for contextualized embeddings.
pub const VOYAGE_CONTEXT_DOC_TOKEN_LIMIT: u32 = 32_000;

/// The grouping budget: 80% of the Voyage per-document limit.
///
/// The headroom (vs the full 32 000) absorbs the divergence between our BPE
/// token counts and voyage-context-3's own tokenizer. It was 90% (28 800), but
/// code-dense documents tokenize denser under Voyage than under our
/// `bge-base-en-v1.5` counter and overran the 10% margin; 80% (25 600) widens
/// it, and the ingest CLI retry-splits anything Voyage still rejects.
#[must_use]
pub const fn context_group_limit() -> u32 {
    VOYAGE_CONTEXT_DOC_TOKEN_LIMIT / 10 * 8
}

/// Partition one document's contiguous chunk sequence into context groups.
///
/// Returns index ranges into `token_counts` (chunk order preserved; ranges
/// are contiguous and cover the whole slice). A document at or under `limit`
/// is one group. Larger documents split into the minimal feasible number of
/// groups `n` (usually `ceil(total/limit)`, occasionally one more when chunk
/// granularity at the capacity boundary makes the ceil count unreachable)
/// with roughly equal token totals (greedy fill toward the remaining
/// per-group share, never exceeding `limit`).
///
/// `n` comes from a max-fill greedy pass, which is optimal for minimizing
/// contiguous partitions under a cap; if the balancing greedy cannot realize
/// an `n`-partition, the max-fill partition itself is returned — minimal `n`
/// is the hard guarantee (spec §6), balance is best-effort where granularity
/// allows.
///
/// # Panics
/// Debug-asserts that no single chunk exceeds `limit` — impossible by
/// construction with `max_tokens = 1024` (spec §6 edge case).
#[must_use]
pub fn balanced_groups(token_counts: &[u32], limit: u32) -> Vec<Range<usize>> {
    if token_counts.is_empty() {
        return Vec::new();
    }
    debug_assert!(
        token_counts.iter().all(|&t| t <= limit),
        "single chunk exceeds the context-group limit"
    );
    let total: u64 = token_counts.iter().map(|&t| u64::from(t)).sum();
    if total <= u64::from(limit) {
        // A one-element Vec of Range is the intended value; the lint guards
        // against `vec![0..n]` written where `(0..n).collect()` was meant.
        #[allow(clippy::single_range_in_vec_init)]
        return vec![0..token_counts.len()];
    }
    let max_fill = max_fill_groups(token_counts, limit);
    let n = max_fill.len();
    // A balanced n-partition usually exists; the max-fill partition is the
    // guaranteed-feasible fallback (same minimal n, possibly lopsided).
    try_partition(token_counts, limit, n).unwrap_or(max_fill)
}

/// Greedy max-fill partition: each group takes as many chunks as fit under
/// `limit`. Produces the provably minimal number of contiguous groups.
fn max_fill_groups(token_counts: &[u32], limit: u32) -> Vec<Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    let mut acc = 0u64;
    for (i, &t) in token_counts.iter().enumerate() {
        if acc > 0 && acc + u64::from(t) > u64::from(limit) {
            groups.push(start..i);
            start = i;
            acc = 0;
        }
        acc += u64::from(t);
    }
    groups.push(start..token_counts.len());
    groups
}

/// Try to partition into exactly `n` groups, each ≤ `limit`, each filled
/// greedily toward its share of the remaining total. `None` when the hard
/// limit forces leftover chunks past the last group.
fn try_partition(token_counts: &[u32], limit: u32, n: usize) -> Option<Vec<Range<usize>>> {
    let mut groups = Vec::with_capacity(n);
    let mut remaining_total: u64 = token_counts.iter().map(|&t| u64::from(t)).sum();
    let mut i = 0usize;
    for g in 0..n {
        if i >= token_counts.len() {
            return None;
        }
        let remaining_groups = u64::try_from(n - g).unwrap_or(1);
        let target = remaining_total.div_ceil(remaining_groups);
        let is_last = g + 1 == n;
        let start = i;
        let mut acc: u64 = 0;
        while i < token_counts.len() {
            let next = u64::from(token_counts[i]);
            if acc > 0 && acc + next > u64::from(limit) {
                break;
            }
            if acc > 0 && !is_last && acc >= target {
                break;
            }
            acc += next;
            i += 1;
        }
        remaining_total -= acc;
        groups.push(start..i);
    }
    (i == token_counts.len()).then_some(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Reference minimal contiguous group count: greedy max-fill, which is
    /// provably optimal for minimizing contiguous partitions under a cap.
    fn reference_min_groups(chunks: &[u32], limit: u32) -> usize {
        let mut count = 0usize;
        let mut acc = 0u64;
        for &c in chunks {
            if acc == 0 || acc + u64::from(c) > u64::from(limit) {
                count += 1;
                acc = 0;
            }
            acc += u64::from(c);
        }
        count
    }

    #[test]
    // One group spanning all three chunks — the single-element Range
    // collection is intentional.
    #[allow(clippy::single_range_in_vec_init)]
    fn single_small_doc_is_one_group() {
        assert_eq!(balanced_groups(&[100, 200, 300], 28_800), vec![0..3]);
    }

    #[test]
    fn empty_doc_yields_no_groups() {
        assert!(balanced_groups(&[], 28_800).is_empty());
    }

    #[test]
    fn spec_example_220pct_doc_splits_into_three_balanced_groups() {
        // ~220% of the limit (§6 example) → 3 groups ≈ 73/73/74%, never 90/90/40.
        let limit = 28_800u32;
        let chunks = vec![920u32; 69]; // 63_480 tokens ≈ 220% of 28_800
        let groups = balanced_groups(&chunks, limit);
        assert_eq!(groups.len(), 3);
        let totals: Vec<u64> = groups
            .iter()
            .map(|r| chunks[r.clone()].iter().map(|&t| u64::from(t)).sum())
            .collect();
        let max = *totals.iter().max().unwrap();
        let min = *totals.iter().min().unwrap();
        assert!(max - min <= 920, "groups must be balanced within one chunk: {totals:?}");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn grouping_properties(chunks in proptest::collection::vec(1u32..=1024, 0..300)) {
            let limit = 28_800u32;
            let groups = balanced_groups(&chunks, limit);
            let total: u64 = chunks.iter().map(|&t| u64::from(t)).sum();
            if chunks.is_empty() {
                prop_assert!(groups.is_empty());
                return Ok(());
            }
            // Concatenation reproduces the original order exactly.
            let mut cursor = 0usize;
            for g in &groups {
                prop_assert_eq!(g.start, cursor);
                prop_assert!(g.end > g.start);
                cursor = g.end;
            }
            prop_assert_eq!(cursor, chunks.len());
            // Every group within the hard limit.
            for g in &groups {
                let sum: u64 = chunks[g.clone()].iter().map(|&t| u64::from(t)).sum();
                prop_assert!(sum <= u64::from(limit));
            }
            // Minimal group count (spec §6): equals the optimal contiguous
            // partition count (greedy max-fill reference). Note this can
            // exceed ceil(total/limit) — chunk granularity at the capacity
            // boundary can make the ceil count infeasible.
            prop_assert_eq!(groups.len(), reference_min_groups(&chunks, limit));
            prop_assert!(groups.len() as u64 >= total.div_ceil(u64::from(limit)));
        }
    }
}

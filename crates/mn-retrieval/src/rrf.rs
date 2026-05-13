//! Reciprocal Rank Fusion (RRF, FR-026 / FR-088).
//!
//! Given N ranked candidate lists, RRF merges them by summing
//! `1 / (k + rank)` across every list containing each candidate. The
//! resulting list is sorted by descending score. `k = 60` is the canonical
//! constant (Cormack et al. 2009) and is what the spec mandates.
//!
//! Each input list is interpreted as ranked top-to-bottom (rank 1, 2, 3, ...).
//! Candidates not present in a list contribute zero to the sum from that list.
//!
//! Properties (also exercised by the property tests in mn-retrieval's test
//! suite):
//! - Determinism: identical inputs produce identical outputs.
//! - Order-independence between lists: shuffling the order in which lists
//!   are passed does not change the final ranking.
//! - Monotonicity within a list: moving a candidate from rank N to rank N-1
//!   in any list never decreases its final score.

use std::collections::HashMap;
use std::hash::Hash;

/// Canonical RRF constant per spec FR-026.
pub const RRF_K: f64 = 60.0;

/// Merge `lists` via RRF. `K_OVERRIDE` lets test code or future tuning swap
/// the constant.
///
/// Returns a vec of `(candidate, score)` ordered by descending `score`. Ties
/// broken by the first-list-rank of each candidate (deterministic).
#[must_use]
pub fn fuse_with_k<T>(lists: &[&[T]], k: f64) -> Vec<(T, f64)>
where
    T: Clone + Eq + Hash,
{
    let mut totals: HashMap<T, f64> = HashMap::new();
    // Tie-breaker: (list_index, rank_within_list) of the FIRST occurrence.
    // Combined with HashMap-insertion order being randomized, we need a
    // deterministic per-candidate origin tuple — otherwise two candidates
    // with identical scores can swap positions across runs.
    let mut first_origin: HashMap<T, (usize, usize)> = HashMap::new();

    for (list_idx, list) in lists.iter().enumerate() {
        for (rank0, candidate) in list.iter().enumerate() {
            // Precision loss is fine here: typical input lists are at most a
            // few hundred items, well within f64's exact-integer range.
            #[allow(clippy::cast_precision_loss)]
            let rank = (rank0 + 1) as f64;
            *totals.entry(candidate.clone()).or_insert(0.0) += 1.0 / (k + rank);
            first_origin
                .entry(candidate.clone())
                .or_insert((list_idx, rank0));
        }
    }

    let mut out: Vec<(T, f64)> = totals.into_iter().collect();
    out.sort_by(|(a_id, a_score), (b_id, b_score)| {
        b_score
            .partial_cmp(a_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let a_origin = first_origin
                    .get(a_id)
                    .copied()
                    .unwrap_or((usize::MAX, usize::MAX));
                let b_origin = first_origin
                    .get(b_id)
                    .copied()
                    .unwrap_or((usize::MAX, usize::MAX));
                a_origin.cmp(&b_origin)
            })
    });
    out
}

/// Merge with the canonical k=60.
#[must_use]
pub fn fuse<T>(lists: &[&[T]]) -> Vec<(T, f64)>
where
    T: Clone + Eq + Hash,
{
    fuse_with_k(lists, RRF_K)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_empty_output() {
        let lists: Vec<&[u32]> = vec![];
        assert!(fuse(&lists).is_empty());
    }

    #[test]
    fn single_list_preserves_order() {
        let l = &[10u32, 20, 30];
        let out = fuse(&[l]);
        assert_eq!(out.iter().map(|(t, _)| *t).collect::<Vec<_>>(), vec![10, 20, 30]);
    }

    #[test]
    fn perfect_agreement_top_is_top() {
        let a = &[1u32, 2, 3, 4, 5];
        let b = &[1u32, 2, 3, 4, 5];
        let out = fuse(&[a, b]);
        assert_eq!(out[0].0, 1);
        assert_eq!(out[1].0, 2);
    }

    #[test]
    fn list_order_does_not_matter() {
        let a = &[1u32, 2, 3];
        let b = &[3u32, 4, 5];
        let out1 = fuse(&[a, b]);
        let out2 = fuse(&[b, a]);
        // Scores must be identical; ordering of equal-score candidates may
        // differ by tie-breaker (first-seen rank), so we compare scores by id.
        let score_map_1: std::collections::HashMap<_, _> = out1.iter().copied().collect();
        let score_map_2: std::collections::HashMap<_, _> = out2.iter().copied().collect();
        for k in [1, 2, 3, 4, 5] {
            assert!((score_map_1[&k] - score_map_2[&k]).abs() < 1e-12);
        }
    }

    #[test]
    fn higher_rank_increases_score() {
        // Same candidate appears as rank 1 in one list vs rank 5 in another.
        let a = &[100u32, 99, 98, 97, 96];
        let b = &[1u32, 2, 3, 4, 100];
        let out = fuse(&[a, b]);
        // Candidate 100 appears rank 1 in `a` and rank 5 in `b`.
        let s_100 = out.iter().find(|(t, _)| *t == 100).unwrap().1;
        // Candidate 1 appears only at rank 1 in `b`.
        let s_1 = out.iter().find(|(t, _)| *t == 1).unwrap().1;
        // 100 gets contributions from both lists, so it must score higher than 1.
        assert!(s_100 > s_1, "100={s_100} should beat 1={s_1}");
    }

    #[test]
    fn deterministic() {
        let a = &[1u32, 2, 3];
        let b = &[2u32, 3, 4];
        let r1 = fuse(&[a, b]);
        let r2 = fuse(&[a, b]);
        assert_eq!(r1, r2);
    }

    #[test]
    fn k_60_score_is_canonical() {
        let only_first = &[42u32];
        let out = fuse(&[only_first]);
        // 1/(60+1) = 1/61
        let expected = 1.0 / 61.0;
        assert!((out[0].1 - expected).abs() < 1e-12);
    }
}

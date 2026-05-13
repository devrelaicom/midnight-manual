//! Property tests for RRF (SC-047, FR-026):
//! - determinism: identical inputs produce identical outputs
//! - order-independence: shuffling input lists does not change per-candidate scores
//! - monotonicity: moving a candidate up in any list never decreases its final score

use mn_retrieval::rrf::fuse;
use proptest::prelude::*;

fn list_strategy() -> impl Strategy<Value = Vec<u32>> {
    proptest::collection::vec(0u32..100, 1..50).prop_map(|mut v| {
        v.sort_unstable();
        v.dedup();
        v
    })
}

proptest! {
    #[test]
    fn deterministic(lists in proptest::collection::vec(list_strategy(), 0..5)) {
        let refs: Vec<&[u32]> = lists.iter().map(Vec::as_slice).collect();
        let a = fuse(&refs);
        let b = fuse(&refs);
        prop_assert_eq!(a, b);
    }

    #[test]
    fn list_order_invariant(lists in proptest::collection::vec(list_strategy(), 1..5)) {
        let refs: Vec<&[u32]> = lists.iter().map(Vec::as_slice).collect();
        let mut reversed = refs.clone();
        reversed.reverse();
        let a = fuse(&refs);
        let b = fuse(&reversed);

        let a_map: std::collections::HashMap<_, _> = a.into_iter().collect();
        let b_map: std::collections::HashMap<_, _> = b.into_iter().collect();
        prop_assert_eq!(a_map.len(), b_map.len());
        for (k, score) in &a_map {
            let other = b_map.get(k).copied().unwrap_or(f64::NAN);
            prop_assert!(
                (score - other).abs() < 1e-12,
                "score for {} differs: {} vs {}",
                k,
                score,
                other
            );
        }
    }

    #[test]
    fn monotonic_in_rank(list in list_strategy()) {
        // If we PROMOTE the last element to the front (rank n -> rank 1), the
        // promoted element's score must be >= its previous score.
        prop_assume!(list.len() >= 2);
        let last = *list.last().unwrap();
        let baseline: Vec<&[u32]> = vec![&list];
        let scores_a = fuse(&baseline);
        let score_a = scores_a
            .iter()
            .find(|(t, _)| *t == last)
            .map(|(_, s)| *s)
            .unwrap();

        let mut promoted = list;
        let pos = promoted.iter().position(|&x| x == last).unwrap();
        let v = promoted.remove(pos);
        promoted.insert(0, v);
        let scores_b = fuse(&[&promoted]);
        let score_b = scores_b
            .iter()
            .find(|(t, _)| *t == last)
            .map(|(_, s)| *s)
            .unwrap();

        prop_assert!(score_b >= score_a, "promotion must not decrease score (was {score_a}, now {score_b})");
    }
}

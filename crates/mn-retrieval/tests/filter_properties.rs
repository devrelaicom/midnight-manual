//! Property tests for the filter model: serde round-trip identity and
//! validation stability.
use mn_retrieval::filters::{SearchFilters, SetMatch};
use proptest::prelude::*;

fn arb_string_set() -> impl Strategy<Value = SetMatch<String>> {
    (
        prop::collection::vec("[a-z_]{1,8}", 0..4),
        prop::collection::vec("[a-z_]{1,8}", 0..4),
    )
        .prop_map(|(any_of, none_of)| SetMatch { any_of, none_of })
}

proptest! {
    /// Any filter built from arbitrary open-set string members round-trips
    /// through JSON unchanged.
    #[test]
    fn open_set_filters_round_trip(language in arb_string_set(), tags in arb_string_set()) {
        let f = SearchFilters { language, tags, ..Default::default() };
        let back: SearchFilters = serde_json::from_value(serde_json::to_value(&f).unwrap()).unwrap();
        prop_assert_eq!(f, back);
    }

    /// validate() never panics on arbitrary open-set input (open sets accept
    /// any string), and only ever returns Err via the typed FilterError path.
    #[test]
    fn validate_is_total_on_open_sets(language in arb_string_set()) {
        let f = SearchFilters { language, ..Default::default() };
        let _ = f.validate(); // must not panic
    }
}

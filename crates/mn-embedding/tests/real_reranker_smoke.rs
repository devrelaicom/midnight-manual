//! Opt-in smoke test for the real `bge-reranker-base` cross-encoder.
//!
//! Loads the actual fastembed ONNX bundle (~270 MB) into the standard model
//! cache and asserts that the reranker assigns a higher logit to a
//! directly-relevant doc than to an irrelevant one for the same query.
//!
//! The corpus embedder is now VoyageAI (client-side BYOK or the server's
//! `/v1/embeddings` proxy), so there is no local embedder left to smoke-test;
//! only the reranker still rides on fastembed.
//!
//! The test is `#[ignore]`-gated so the default `cargo test` run does not
//! download ~270 MB of weights on every CI lap. Run with:
//!
//! ```text
//! cargo test -p mn-embedding --test real_reranker_smoke -- --ignored
//! ```
//!
//! The CI workflow `.github/workflows/reranker-smoke.yml` runs this suite on a
//! daily schedule and can be triggered manually via the `workflow_dispatch`
//! button on the Actions tab.

#![allow(clippy::doc_markdown)]

use mn_embedding::cache::{resolve, StdEnv};
use mn_embedding::reranker::Reranker;

/// Resolve and create the on-disk model cache directory, falling back to a
/// fresh `target/test-model-cache` under the repo when neither
/// `MIDNIGHT_MANUAL_MODEL_CACHE_DIR`, `XDG_DATA_HOME`, nor `HOME` is set.
fn cache_dir() -> std::path::PathBuf {
    let dir = resolve(&StdEnv).unwrap_or_else(|| {
        std::env::current_dir()
            .expect("cwd")
            .join("target")
            .join("test-model-cache")
    });
    std::fs::create_dir_all(&dir).expect("create model cache dir");
    dir
}

#[test]
#[ignore = "real-reranker smoke: downloads ~270 MB; run with --ignored"]
fn reranker_ranks_relevant_doc_above_irrelevant() {
    let dir = cache_dir();
    let reranker = Reranker::try_new(dir).expect("load real reranker");

    let query = "how do I compile a Compact smart contract";
    let docs: Vec<String> = vec![
        // index 0: irrelevant
        "the weather in San Francisco is foggy today".to_owned(),
        // index 1: directly relevant
        "to compile a Compact contract, run `compactc src.compact` and inspect the produced artifact"
            .to_owned(),
        // index 2: tangentially related (same domain, different question)
        "Midnight uses zero-knowledge proofs to keep transaction contents private".to_owned(),
    ];

    let scores = reranker.rerank(query, &docs, None).expect("rerank");
    assert_eq!(scores.len(), 3);
    let score_for = |idx: usize| -> f32 {
        scores
            .iter()
            .find(|r| r.index == idx)
            .unwrap_or_else(|| panic!("missing rerank score for index {idx}"))
            .score
    };
    let s_irrelevant = score_for(0);
    let s_relevant = score_for(1);
    let s_tangential = score_for(2);

    assert!(
        s_relevant > s_irrelevant,
        "reranker regression: relevant logit {s_relevant} must beat irrelevant {s_irrelevant}"
    );
    assert!(
        s_relevant > s_tangential,
        "reranker regression: directly-relevant logit {s_relevant} must beat tangential {s_tangential}"
    );
}

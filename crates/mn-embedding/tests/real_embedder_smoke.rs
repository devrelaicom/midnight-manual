//! Opt-in smoke test for the real `bge-base-en-v1.5` embedder and the
//! `bge-reranker-base` cross-encoder.
//!
//! Loads the actual fastembed ONNX bundles (~370 MB for the embedder,
//! ~270 MB for the reranker) into the standard model cache and asserts
//! that:
//!
//! 1. The embedder returns 768-dim vectors with non-zero norms.
//! 2. The cosine similarity between near-paraphrases beats the cosine
//!    between clearly unrelated sentences by a comfortable margin — a
//!    sanity check that the model loaded actually behaves like an
//!    English embedder.
//! 3. The reranker assigns a higher logit to a directly-relevant doc
//!    than to an irrelevant one for the same query.
//!
//! Both tests are `#[ignore]`-gated so the default `cargo test` run does
//! not download ~640 MB of weights on every CI lap. Run with:
//!
//! ```text
//! cargo test -p mn-embedding --test real_embedder_smoke -- --ignored
//! ```
//!
//! The CI workflow `.github/workflows/embedder-smoke.yml` runs this
//! suite on a daily schedule and can be triggered manually via the
//! `workflow_dispatch` button on the Actions tab.

use mn_embedding::cache::{resolve, StdEnv};
use mn_embedding::embedder::{Embedder, BGE_BASE_DIM};
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

/// Cosine similarity of two equal-length f32 vectors.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "cosine: dimension mismatch");
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|y| y * y).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[test]
#[ignore = "real-embedder smoke: downloads ~370 MB; run with --ignored"]
fn embedder_produces_768d_vectors_and_orders_paraphrases() {
    let dir = cache_dir();
    let embedder = Embedder::try_new(dir).expect("load real embedder");

    let docs: Vec<String> = vec![
        // anchor
        "how do I compile a Compact smart contract".to_owned(),
        // near-paraphrase of the anchor
        "what is the build step for a Compact contract source file".to_owned(),
        // clearly unrelated
        "the weather in San Francisco is foggy today".to_owned(),
    ];

    let vectors = embedder.embed(&docs, None).expect("embed three sentences");
    assert_eq!(vectors.len(), 3);
    for (i, v) in vectors.iter().enumerate() {
        assert_eq!(
            v.len(),
            BGE_BASE_DIM,
            "doc {i} embedding has {} dims, expected {BGE_BASE_DIM}",
            v.len()
        );
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(norm > 0.0, "doc {i} has a zero-norm embedding");
    }

    let sim_paraphrase = cosine(&vectors[0], &vectors[1]);
    let sim_unrelated = cosine(&vectors[0], &vectors[2]);
    // The bge-base-en-v1.5 model returns paraphrase cosines in the 0.7-0.9
    // range and clearly-unrelated cosines around 0.2-0.5. A 0.10 gap is a
    // very forgiving sanity floor that should hold across point releases.
    assert!(
        sim_paraphrase > sim_unrelated + 0.10,
        "embedder regression: paraphrase cosine {sim_paraphrase:.4} should beat \
         unrelated cosine {sim_unrelated:.4} by at least 0.10"
    );
}

#[test]
#[ignore = "real-embedder smoke: downloads ~270 MB; run with --ignored"]
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

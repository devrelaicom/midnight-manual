//! Task 9.3: `LoadedReranker` resolves + loads a reranker from a `RerankerSpec`.
//!
//! Runnable (non-`#[ignore]`) tests cover the paths that need no network:
//!   * the `Voyage` arm's key handling (no HTTP — construction only), and
//!   * the `CustomPath` arm's error on a directory missing the ONNX/tokenizer
//!     files.
//!
//! The model-download paths (`Native`, `UserOnnx`) are `#[ignore]`-gated,
//! mirroring `tests/real_reranker_smoke.rs`, so the default `cargo test` lap
//! never pulls hundreds of MB of weights. They still compile, which is the
//! point of keeping them here.
//!
//! The full Voyage rerank request/response mapping is covered against a
//! `wiremock` mock in `tests/voyage_rerank.rs` (Task 9.1). `LoadedReranker::load`
//! builds the `VoyageReranker` internally with the hard-coded production base
//! URL, so there is no seam to inject a mock URL through `load`; rather than
//! widen the public API for a test, we assert the `Voyage` arm's *construction*
//! contract here (variant + key handling) and lean on `voyage_rerank.rs` for the
//! wire mapping.

#![allow(clippy::doc_markdown)]

use fastembed::RerankerModel;
use mn_embedding::error::EmbeddingError;
use mn_embedding::reranker::LoadedReranker;
use mn_embedding::reranker_catalog::RerankerSpec;

/// A throwaway, guaranteed-empty cache directory for tests that must not touch
/// the shared model cache.
fn tmp_cache() -> tempfile::TempDir {
    tempfile::tempdir().expect("create temp cache dir")
}

#[tokio::test]
async fn voyage_spec_with_key_loads_to_voyage_variant() {
    let dir = tmp_cache();
    let loaded = LoadedReranker::load(
        RerankerSpec::Voyage("rerank-2.5-lite".to_owned()),
        dir.path().to_path_buf(),
        Some("test-key"),
    )
    .await
    .expect("voyage spec with a key must load");
    assert!(
        matches!(loaded, LoadedReranker::Voyage(_)),
        "a Voyage spec must resolve to the Voyage variant"
    );
}

#[tokio::test]
async fn voyage_spec_without_key_errors_mentioning_env_var() {
    let dir = tmp_cache();
    let err = LoadedReranker::load(
        RerankerSpec::Voyage("rerank-2.5-lite".to_owned()),
        dir.path().to_path_buf(),
        None,
    )
    .await
    .expect_err("a Voyage spec without a key must fail");
    match err {
        EmbeddingError::Init { message, .. } => assert!(
            message.contains("VOYAGE_API_KEY"),
            "the missing-key error must point at VOYAGE_API_KEY; got: {message}"
        ),
        other => panic!("expected EmbeddingError::Init, got: {other:?}"),
    }
}

#[tokio::test]
async fn custom_path_with_missing_files_errors() {
    let cache = tmp_cache();
    // An empty directory: no model.onnx, no tokenizer files — the load must
    // surface an Init error rather than panic or hang.
    let model_dir = tmp_cache();
    let err = LoadedReranker::load(
        RerankerSpec::CustomPath(model_dir.path().to_path_buf()),
        cache.path().to_path_buf(),
        None,
    )
    .await
    .expect_err("a custom path with no model files must fail");
    assert!(
        matches!(err, EmbeddingError::Init { .. }),
        "missing custom model files must map to EmbeddingError::Init; got: {err:?}"
    );
}

#[tokio::test]
async fn custom_path_that_does_not_exist_errors() {
    let cache = tmp_cache();
    let err = LoadedReranker::load(
        RerankerSpec::CustomPath("/nonexistent/reranker/dir".into()),
        cache.path().to_path_buf(),
        None,
    )
    .await
    .expect_err("a custom path pointing at a missing dir must fail");
    assert!(
        matches!(err, EmbeddingError::Init { .. }),
        "a missing custom path must map to EmbeddingError::Init; got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "downloads ~270 MB of bge-reranker-base weights; run with --ignored"]
async fn native_spec_loads_and_reranks() {
    let dir = tmp_cache();
    let loaded = LoadedReranker::load(
        RerankerSpec::Native(RerankerModel::BGERerankerBase),
        dir.path().to_path_buf(),
        None,
    )
    .await
    .expect("native reranker must load");
    assert!(matches!(loaded, LoadedReranker::Local(_)));

    let results = loaded
        .rerank(
            "how do I compile a Compact contract".to_owned(),
            vec![
                "the weather in San Francisco is foggy today".to_owned(),
                "run `compactc src.compact` to compile a Compact contract".to_owned(),
            ],
        )
        .await
        .expect("rerank");
    assert_eq!(results.len(), 2);
    let score_for = |idx: usize| results.iter().find(|r| r.index == idx).unwrap().score;
    assert!(score_for(1) > score_for(0), "the relevant doc must outrank the irrelevant one");
}

#[tokio::test]
#[ignore = "downloads a user-defined ONNX reranker from Hugging Face; run with --ignored"]
async fn user_onnx_spec_loads_and_reranks() {
    let dir = tmp_cache();
    let loaded = LoadedReranker::load(
        RerankerSpec::UserOnnx {
            repo: "Xenova/ms-marco-MiniLM-L-6-v2",
            model_file: "onnx/model.onnx",
        },
        dir.path().to_path_buf(),
        None,
    )
    .await
    .expect("user-defined onnx reranker must load");
    assert!(matches!(loaded, LoadedReranker::Local(_)));

    let results = loaded
        .rerank(
            "how do I compile a Compact contract".to_owned(),
            vec![
                "the weather in San Francisco is foggy today".to_owned(),
                "run `compactc src.compact` to compile a Compact contract".to_owned(),
            ],
        )
        .await
        .expect("rerank");
    assert_eq!(results.len(), 2);
}

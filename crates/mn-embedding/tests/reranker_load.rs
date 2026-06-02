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
//! `wiremock` mock in `tests/voyage_rerank.rs` (Task 9.1). As of Task 9.4
//! `LoadedReranker::load` takes an optional `voyage_base_url` override, so the
//! Voyage arm *can* be pointed at a mock; the
//! [`voyage_spec_with_base_url_override_reranks_via_mock`] test below exercises
//! that seam end-to-end. The construction-contract tests (variant + key
//! handling) still pass `None` for the override.

#![allow(clippy::doc_markdown)]

use fastembed::RerankerModel;
use mn_embedding::error::EmbeddingError;
use mn_embedding::reranker::LoadedReranker;
use mn_embedding::reranker_catalog::RerankerSpec;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
        None,
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

/// Task 9.4 PART C: the `voyage_base_url` override threads through `load` into
/// the `VoyageReranker`, so a Voyage reranker can be pointed at a wiremock
/// `/v1/rerank`. This makes the Voyage *load + rerank* path testable without
/// touching `api.voyageai.com` (and without any env mutation).
#[tokio::test]
async fn voyage_spec_with_base_url_override_reranks_via_mock() {
    let server = MockServer::start().await;
    // The mock returns the second document as more relevant (higher score),
    // out of input order, to prove `index` is honoured by the caller.
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [
                { "relevance_score": 0.95_f32, "index": 1 },
                { "relevance_score": 0.10_f32, "index": 0 },
            ],
            "model": "rerank-2.5-lite",
            "usage": { "total_tokens": 12 },
        })))
        .mount(&server)
        .await;

    let dir = tmp_cache();
    let loaded = LoadedReranker::load(
        RerankerSpec::Voyage("rerank-2.5-lite".to_owned()),
        dir.path().to_path_buf(),
        Some("test-key"),
        Some(&server.uri()),
    )
    .await
    .expect("voyage spec with a base-url override must load");
    assert!(matches!(loaded, LoadedReranker::Voyage(_)));

    let results = loaded
        .rerank(
            "how do I compile a Compact contract".to_owned(),
            vec![
                "the weather in San Francisco is foggy today".to_owned(),
                "run `compactc src.compact` to compile a Compact contract".to_owned(),
            ],
        )
        .await
        .expect("rerank via the mocked Voyage endpoint");

    assert_eq!(results.len(), 2);
    let top = results
        .iter()
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .expect("non-empty results");
    assert_eq!(top.index, 1, "the relevant doc (index 1) must score highest");
}

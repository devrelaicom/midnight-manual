//! Task 9.4 PART A: the MCP `search` tool reranks via the *configured*
//! reranker catalog id (native / onnx / custom / Voyage) through
//! `LoadedReranker`, instead of the hardcoded local `bge` singleton.
//!
//! ## Test strategy (we chose **(b)**, the selection-contract + isolated-load
//! unit, over **(a)** full-wiremock-through-`run_search`)
//!
//! `run_search` resolves its config via `mn_core::config::Config::discover(None,
//! &StdEnv)` and `resolve_voyage_api_key(None, .., &StdEnv)` — both read the
//! *real* process environment. Driving the full Voyage rerank path through
//! `run_search` would therefore require setting `MIDNIGHT_MANUAL_RERANKER`,
//! `VOYAGE_API_KEY`, and `MIDNIGHT_MANUAL_VOYAGE_BASE_URL` as real env vars,
//! which is racy under the parallel test harness (and `std::env::set_var` is
//! `unsafe`). So instead of mutating global env we:
//!
//!   1. assert the *selection contract* — the catalog id that `run_search`
//!      resolves maps to the right `RerankerSpec` (default config → local
//!      `bge`; a Voyage config id → the Voyage spec); and
//!   2. drive the *exact* MCP load-and-rerank unit `run_search` calls
//!      (`mn_mcp::tools::load_configured_reranker`) with a Voyage id pointed at
//!      a wiremock `/v1/rerank` via the env-free base-url override, proving the
//!      configured Voyage reranker loads + reranks **without** loading the local
//!      bge model.
//!
//! The Voyage *HTTP wire mapping* itself is covered by Task 9.1's
//! `mn-embedding/tests/voyage_rerank.rs`; the `load` base-url seam by Task 9.4's
//! `mn-embedding/tests/reranker_load.rs`.

#![allow(missing_docs)]

use mn_embedding::reranker::LoadedReranker;
use mn_embedding::reranker_catalog::{resolve, RerankerSpec};
use mn_mcp::tools::load_configured_reranker;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Selection contract: the default config reranker id resolves to a *local*
/// fastembed-native spec (the prior `bge` behaviour), so omitting any
/// reranker config keeps the historical default.
#[test]
fn default_config_selects_local_bge() {
    let default_id = mn_core::config::ModelsConfig::default().reranker;
    assert_eq!(default_id, "bge-reranker-base");
    let spec = resolve(&default_id, None).expect("default id resolves");
    assert!(
        matches!(spec, RerankerSpec::Native(_)),
        "the default reranker must resolve to a local fastembed-native spec, got {spec:?}"
    );
}

/// Selection contract: a Voyage config reranker id resolves to the Voyage spec,
/// so configuring `reranker = "voyage-rerank-2.5-lite"` selects the remote
/// reranker rather than the local bge singleton.
#[test]
fn voyage_config_selects_voyage_spec() {
    let spec = resolve("voyage-rerank-2.5-lite", None).expect("voyage id resolves");
    assert!(
        matches!(spec, RerankerSpec::Voyage(_)),
        "a voyage-* reranker id must resolve to the Voyage spec, got {spec:?}"
    );
}

/// End-to-end through the MCP load unit: `load_configured_reranker` with a
/// Voyage id + key + base-url override loads the Voyage variant and reranks via
/// a wiremock `/v1/rerank`. This is the *same* unit `run_search` calls, so it
/// proves the search tool reranks through the configured Voyage reranker — and
/// crucially never touches the local bge model (no weights downloaded, no
/// network to Hugging Face).
#[tokio::test]
async fn load_configured_voyage_reranker_reranks_via_mock() {
    let server = MockServer::start().await;
    // Return doc index 1 as most relevant, out of input order, to prove the
    // caller maps results by `index` (not positionally).
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [
                { "relevance_score": 0.95_f32, "index": 1 },
                { "relevance_score": 0.10_f32, "index": 0 },
            ],
            "model": "rerank-2.5-lite",
            "usage": { "total_tokens": 9 },
        })))
        .mount(&server)
        .await;

    let cache = tempfile::tempdir().unwrap();
    let loaded = load_configured_reranker(
        "voyage-rerank-2.5-lite",
        None,
        Some("test-key"),
        Some(&server.uri()),
        cache.path(),
    )
    .await
    .expect("the configured Voyage reranker must load");
    assert!(
        matches!(loaded.as_ref(), LoadedReranker::Voyage(_)),
        "the configured voyage id must load the Voyage variant"
    );

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
        .expect("non-empty");
    assert_eq!(top.index, 1, "the relevant doc (index 1) must score highest");
}

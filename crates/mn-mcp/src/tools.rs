//! MCP tool registry and per-tool handlers.
//!
//! Twelve tools (until `get_chunk_siblings` is removed at the end of this
//! plan), three categories:
//!
//! - `status` / `pull_models` — local-only; talk to the embedder/reranker
//!   model cache. No cloud round-trip.
//! - `search` — embed locally, post to the cloud `/v1/search`, optionally
//!   rerank with the local cross-encoder.
//! - All other tools (`get_chunk` / `get_chunk_next` / `get_chunk_prev` /
//!   `get_chunk_siblings` / `get_chunk_parents` / `get_document` /
//!   `get_document_full` / `get_document_chunks` / `list_sources`) —
//!   pass-through to the cloud's read entry points, returning the response
//!   JSON verbatim.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use mn_core::scoring::normalize_rerank;
use mn_core::scoring_policy::ScoringPolicy;
use mn_embedding::{embedder, reranker};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::cloud_client::{CloudClient, CloudError, QueryPair, SearchRequest};
use crate::protocol::{ToolDescription, ToolsListResult};
use crate::server::ServerConfig;

/// Build the static tool manifest sent in response to `tools/list`.
///
/// All twelve tools declared in spec.md US5 / contracts/mcp-tools.json.
/// Schemas here are kept in sync with the canonical document by way of the
/// contract tests in `tests/`.
#[must_use]
pub fn list() -> ToolsListResult {
    ToolsListResult {
        tools: vec![
            ToolDescription {
                name: "search",
                description:
                    "Hybrid (FTS + vector) retrieval over the Midnight corpus, with optional cross-encoder reranking and trust-aware confidence scoring. Provide the single-query `{query, vector}` form, or a `queries` array of 1-10 `{text, vector}` pairs that RRF fuses across. The caller embeds each text. Rate-limit cost is max(1, distinct queries) tokens (D25).\n\nPatterns (full worked examples in docs/cookbook/query-enhancement.md):\n- hyde: send the question plus a hypothetical answer as a second query, e.g. queries=[\"<the user's question>\", \"<a 1-2 sentence hypothetical answer the agent drafts>\"]. Lifts recall when the question is short or jargon-light.\n- multi_query: send 2-3 paraphrases varying vocabulary and breadth, e.g. queries=[\"compile a contract\", \"build source into a deployable artifact\", \"smart-contract build step\"]. Helps when synonyms matter.\n- step_back: send the question plus a more abstract framing, e.g. queries=[\"why did this specific call fail?\", \"how does the platform validate calls?\"]. Helps when the question is over-specific.",
                input_schema: search_input_schema(),
            },
            ToolDescription {
                name: "get_chunk",
                description:
                    "Fetch one chunk by id. Returns the chunk row (id, content, chunk_index, total_chunks, content_hash, embedding_model_id, heading_path, symbol_path, start_byte, end_byte, token_count, status, created_at, document_id, source_version_id, node_id) plus a small `document` sub-object (id, source_path, published_url, source_url, language, kind, provenance) and a `source` sub-object (slug). For the chunk's parent chain call get_chunk_parents; for adjacent chunks call get_chunk_next/get_chunk_prev.",
                input_schema: id_only_schema(),
            },
            ToolDescription {
                name: "get_chunk_next",
                description:
                    "Fetch up to `count` chunks immediately following the given chunk in chunk_index order, scoped to the same document. Returns `{chunks: ChunkWithContext[]}` sorted ascending. Returns `{chunks: []}` (not 404) when called on the last chunk. `embed_failed` chunks are skipped, so the returned chunk_index sequence may have gaps. count defaults to 5 and must be in [1, 100]; out-of-range values are rejected as InvalidParams before the call reaches the cloud.",
                input_schema: chunk_nav_schema(),
            },
            ToolDescription {
                name: "get_chunk_prev",
                description:
                    "Fetch up to `count` chunks immediately preceding the given chunk in chunk_index order, scoped to the same document. Returns `{chunks: ChunkWithContext[]}` sorted ascending (reading order). Returns `{chunks: []}` (not 404) when called on the first chunk. `embed_failed` chunks are skipped, so the returned chunk_index sequence may have gaps. count defaults to 5 and must be in [1, 100]; out-of-range values are rejected as InvalidParams before the call reaches the cloud.",
                input_schema: chunk_nav_schema(),
            },
            ToolDescription {
                name: "get_chunk_siblings",
                description:
                    "Fetch every chunk from the same document as the given chunk, ordered by chunk_index. Useful for reconstructing a full page from any starting point.",
                input_schema: id_only_schema(),
            },
            ToolDescription {
                name: "get_chunk_parents",
                description:
                    "Walk the parent chain from a chunk up to its source-version root. Returns nodes from immediate parent to root.",
                input_schema: id_only_schema(),
            },
            ToolDescription {
                name: "get_document",
                description:
                    "Document overview: metadata (id, source_version_id, node_id, source_path, published_url, source_url, language, kind, content_hash, char_count, token_count, source_modified_at, created_at, frontmatter, provenance, package_id), the source `{slug}`, and an ordered `chunk_ids` array of every ready chunk. No chunk bodies. Use get_document_full for inline bodies or get_document_chunks for a windowed slice.",
                input_schema: id_only_schema(),
            },
            ToolDescription {
                name: "get_document_full",
                description:
                    "Complete document: every overview field except chunk_ids, plus a `chunks` array with each chunk's `{chunk_id, chunk_index, content, heading_path, token_count}` inline (no per-chunk document/source sub-objects). Capped at 500 ready chunks. For documents over the cap the call fails with a structured `too_many_chunks` error (see error.data.next_tool); fall back to get_document_chunks.",
                input_schema: id_only_schema(),
            },
            ToolDescription {
                name: "get_document_chunks",
                description:
                    "Position-windowed chunk slice of a document. Returns `{chunks: ChunkBody[], from, limit, total_chunks}`. from defaults to 0 (must be >= 0); limit defaults to 20 and must be in [1, 100]. Out-of-range values are rejected as InvalidParams before the call reaches the cloud. `from` past the end returns `chunks: []` with accurate `total_chunks` (not 404). Use to page through documents larger than get_document_full's 500-chunk cap or to read a known offset.",
                input_schema: document_chunks_schema(),
            },
            ToolDescription {
                name: "list_sources",
                description:
                    "Enumerate the corpus's available sources so an agent can narrow filters.",
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
            },
            ToolDescription {
                name: "pull_models",
                description:
                    "Download / load the embedder (bge-base-en-v1.5) and reranker (bge-reranker-base) into the local model cache. Required on first use and after a corpus-side model migration (signalled by an `embedding_model_mismatch` error from search).",
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
            },
            ToolDescription {
                name: "status",
                description:
                    "Report health and model state. Works without models loaded.",
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
            },
        ],
    }
}

fn id_only_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
        },
        "additionalProperties": false,
    })
}

fn chunk_nav_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 5 },
        },
        "additionalProperties": false,
    })
}

fn document_chunks_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "from": { "type": "integer", "minimum": 0, "default": 0 },
            "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
        },
        "additionalProperties": false,
    })
}

fn search_input_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "minLength": 1,
                "description": "Single-query convenience form (mutually exclusive with queries).",
            },
            "queries": {
                "type": "array",
                "minItems": 1,
                "maxItems": 50,
                "items": { "type": "string", "minLength": 1 },
                "description": "Multi-query input for HyDE / expansion / step-back patterns.",
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 50,
                "default": 10,
                "description": "Max results returned to the caller. Capped at 50 (FR-088).",
            },
            "rerank": {
                "type": "boolean",
                "default": true,
                "description": "Apply local cross-encoder reranking. Disable for ultra-low-latency callers.",
            },
            "filters": {
                "type": "object",
                "description": "Filter spec forwarded verbatim to the cloud /v1/search endpoint.",
            },
        },
        "oneOf": [
            { "required": ["query"] },
            { "required": ["queries"] },
        ],
    })
}

// ---------------------------------------------------------------------------
// status (local)
// ---------------------------------------------------------------------------

/// `status` tool response payload.
#[derive(Debug, Serialize)]
pub struct StatusOutput {
    /// mn-mcp crate version.
    pub server_version: &'static str,
    /// Embedder model identifier.
    pub embedder: &'static str,
    /// Reranker model identifier.
    pub reranker: &'static str,
    /// Current model state.
    pub model_state: ModelState,
    /// Resolved on-disk model cache directory, if any.
    pub cache_dir: Option<String>,
}

/// Coarse model-state values reported by `status`.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelState {
    /// Models not yet loaded for this process.
    Missing,
    /// Models loaded and ready to use.
    Ready,
}

/// Dispatch the `status` tool.
#[must_use]
pub fn run_status(cache_dir: Option<&PathBuf>) -> StatusOutput {
    StatusOutput {
        server_version: crate::VERSION,
        embedder: mn_embedding::EMBEDDER_MODEL_NAME,
        reranker: mn_embedding::RERANKER_MODEL_NAME,
        model_state: if embedder_loaded() && reranker_loaded() {
            ModelState::Ready
        } else {
            ModelState::Missing
        },
        cache_dir: cache_dir.map(|p| p.display().to_string()),
    }
}

fn embedder_loaded() -> bool {
    LOADED_MARKERS.load_relaxed_embedder()
}

fn reranker_loaded() -> bool {
    LOADED_MARKERS.load_relaxed_reranker()
}

mod markers {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Process-wide markers tracking whether `pull_models` has completed.
    pub struct LoadedMarkers {
        embedder: AtomicBool,
        reranker: AtomicBool,
    }

    impl LoadedMarkers {
        pub const fn new() -> Self {
            Self {
                embedder: AtomicBool::new(false),
                reranker: AtomicBool::new(false),
            }
        }

        pub fn mark_embedder(&self) {
            self.embedder.store(true, Ordering::Release);
        }

        pub fn mark_reranker(&self) {
            self.reranker.store(true, Ordering::Release);
        }

        pub fn load_relaxed_embedder(&self) -> bool {
            self.embedder.load(Ordering::Acquire)
        }

        pub fn load_relaxed_reranker(&self) -> bool {
            self.reranker.load(Ordering::Acquire)
        }
    }
}

use markers::LoadedMarkers;

pub(crate) static LOADED_MARKERS: LoadedMarkers = LoadedMarkers::new();

// ---------------------------------------------------------------------------
// pull_models (local)
// ---------------------------------------------------------------------------

/// `pull_models` response payload.
#[derive(Debug, Serialize)]
pub struct PullModelsOutput {
    /// Embedder model identifier.
    pub embedder: &'static str,
    /// Reranker model identifier.
    pub reranker: &'static str,
    /// Whether the embedder was loaded by this call (false = cached).
    pub embedder_loaded: bool,
    /// Whether the reranker was loaded by this call (false = cached).
    pub reranker_loaded: bool,
    /// Total milliseconds spent in this call.
    pub took_ms: u128,
}

/// Dispatch the `pull_models` tool. Returns once both `OnceCell`s are filled.
///
/// # Errors
///
/// Returns a string error message if either model fails to initialize.
pub async fn run_pull_models(cache_dir: PathBuf) -> Result<PullModelsOutput, String> {
    let t0 = Instant::now();
    let embedder_was_loaded = LOADED_MARKERS.load_relaxed_embedder();
    let reranker_was_loaded = LOADED_MARKERS.load_relaxed_reranker();

    embedder::global(cache_dir.clone())
        .await
        .map_err(|e| format!("embedder init failed: {e}"))?;
    LOADED_MARKERS.mark_embedder();

    reranker::global(cache_dir)
        .await
        .map_err(|e| format!("reranker init failed: {e}"))?;
    LOADED_MARKERS.mark_reranker();

    Ok(PullModelsOutput {
        embedder: mn_embedding::EMBEDDER_MODEL_NAME,
        reranker: mn_embedding::RERANKER_MODEL_NAME,
        embedder_loaded: !embedder_was_loaded,
        reranker_loaded: !reranker_was_loaded,
        took_ms: t0.elapsed().as_millis(),
    })
}

// ---------------------------------------------------------------------------
// search (cloud + local embed + optional local rerank)
// ---------------------------------------------------------------------------

/// Errors `run_search` can produce. Distinguished so the server layer can map
/// them to the right MCP error code.
#[derive(Debug)]
pub enum SearchError {
    /// Caller-supplied arguments are malformed (oneOf violation, type
    /// mismatch, etc.).
    InvalidInput(String),
    /// Cloud returned an embedding-model mismatch — the JSON-RPC error layer
    /// turns this into a typed response with `data.next_tool = "pull_models"`.
    Mismatch {
        /// Corpus's active `{name}@{revision}`.
        corpus_model: String,
        /// What the client sent.
        client_model: String,
        /// Operator-facing message.
        message: String,
        /// Concrete next step.
        remediation: String,
    },
    /// Catchall cloud / decode / transport failure.
    Cloud(String),
}

const DEFAULT_LIMIT: u32 = 10;
const MAX_LIMIT: u32 = 50;
/// When rerank is on, we always fetch up to this many candidates from the
/// cloud so the cross-encoder has signal to work with.
const RERANK_FETCH: u32 = 50;

/// Dispatch the `search` tool.
///
/// Embeds each query locally, posts the resulting `{text, vector}` pairs to
/// the cloud's `/v1/search`, optionally reranks the returned chunks against
/// the first query, and truncates to the caller's `limit`. When rerank is on
/// each returned result gains a `rerank_score` field.
///
/// # Errors
///
/// See [`SearchError`].
pub async fn run_search(
    args: &serde_json::Value,
    cfg: &ServerConfig,
    cloud: &Arc<CloudClient>,
) -> Result<serde_json::Value, SearchError> {
    let parsed = parse_search_args(args).map_err(SearchError::InvalidInput)?;

    // Embed all queries against the local model.
    let embedder = embedder::global(cfg.cache_dir.clone())
        .await
        .map_err(|e| SearchError::Cloud(format!("embedder init failed: {e}")))?;
    LOADED_MARKERS.mark_embedder();
    let vectors = embedder
        .embed_blocking(parsed.queries.clone(), None)
        .await
        .map_err(|e| SearchError::Cloud(format!("embed failed: {e}")))?;
    let pairs: Vec<QueryPair> = parsed
        .queries
        .iter()
        .zip(vectors.into_iter())
        .map(|(text, vector)| QueryPair { text: text.clone(), vector })
        .collect();

    // Send to cloud. If rerank is on, ask for a fixed top-K so the reranker
    // has a useful candidate pool independent of the caller's limit.
    let cloud_limit = if parsed.rerank {
        RERANK_FETCH
    } else {
        parsed.limit
    };
    let req = SearchRequest {
        queries: pairs,
        client_embedding_model: cfg.client_embedding_model.clone(),
        limit: cloud_limit,
        filters: parsed.filters.clone(),
        // When reranking, fetch the candidate pool in RRF/relevance order so the
        // cross-encoder sees the most relevant chunks (not the cloud's
        // confidence-first default, which could drop relevant-but-low-trust
        // candidates before we rerank). Pass-through (rerank=false) keeps the
        // cloud's confidence ordering.
        sort_by: if parsed.rerank { Some("score") } else { None },
    };
    let cloud_resp = match cloud.search(&req).await {
        Ok(v) => v,
        Err(CloudError::EmbeddingModelMismatch {
            corpus_model,
            client_model,
            message,
            remediation,
        }) => {
            return Err(SearchError::Mismatch {
                corpus_model,
                client_model,
                message,
                remediation,
            });
        }
        Err(e) => return Err(SearchError::Cloud(e.to_string())),
    };

    // Decompose cloud response. `results` is the only field we touch — every
    // other field is passed through verbatim so the response stays additive
    // as the cloud surface evolves.
    let mut envelope = cloud_resp;
    let results = envelope
        .get_mut("results")
        .and_then(|r| r.as_array_mut())
        .map(std::mem::take)
        .unwrap_or_default();

    let final_results = if parsed.rerank && !results.is_empty() {
        rerank_results(&parsed.queries, results, &cfg.cache_dir, parsed.limit).await?
    } else {
        let mut r = results;
        r.truncate(parsed.limit as usize);
        r
    };

    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("results".to_owned(), serde_json::Value::Array(final_results));
        obj.insert(
            "corpus_embedding_model".to_owned(),
            serde_json::Value::String(cfg.client_embedding_model.clone()),
        );
    }
    Ok(envelope)
}

struct ParsedSearchArgs {
    queries: Vec<String>,
    limit: u32,
    rerank: bool,
    filters: Option<serde_json::Value>,
}

fn parse_search_args(v: &serde_json::Value) -> Result<ParsedSearchArgs, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "arguments must be a JSON object".to_owned())?;

    let queries: Vec<String> = match (obj.get("query"), obj.get("queries")) {
        (Some(_), Some(_)) => {
            return Err("`query` and `queries` are mutually exclusive".to_owned());
        }
        (Some(serde_json::Value::String(s)), None) => {
            if s.is_empty() {
                return Err("`query` must not be empty".to_owned());
            }
            vec![s.clone()]
        }
        (None, Some(serde_json::Value::Array(arr))) => {
            if arr.is_empty() {
                return Err("`queries` must not be empty".to_owned());
            }
            if arr.len() > MAX_LIMIT as usize {
                return Err(format!("`queries` length must be <= {MAX_LIMIT}"));
            }
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let Some(s) = item.as_str() else {
                    return Err(format!("`queries[{i}]` must be a string"));
                };
                if s.is_empty() {
                    return Err(format!("`queries[{i}]` must not be empty"));
                }
                out.push(s.to_owned());
            }
            out
        }
        _ => return Err("supply either `query` (string) or `queries` (array)".to_owned()),
    };

    // Honour omitted `limit` as the default; reject any present-but-not-integer
    // value rather than quietly defaulting (silent-default would let callers
    // ship a typo like `limit: "five"` and never notice).
    let limit = match obj.get("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err("`limit` must be an integer".to_owned());
            };
            if !(1..=i64::from(MAX_LIMIT)).contains(&n) {
                return Err(format!("`limit` must be 1..={MAX_LIMIT}"));
            }
            u32::try_from(n).expect("validated above")
        }
    };

    let rerank = obj
        .get("rerank")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let filters = obj.get("filters").cloned();

    Ok(ParsedSearchArgs {
        queries,
        limit,
        rerank,
        filters,
    })
}

async fn rerank_results(
    queries: &[String],
    results: Vec<serde_json::Value>,
    cache_dir: &Path,
    limit: u32,
) -> Result<Vec<serde_json::Value>, SearchError> {
    let reranker = reranker::global(cache_dir.to_path_buf())
        .await
        .map_err(|e| SearchError::Cloud(format!("reranker init failed: {e}")))?;
    LOADED_MARKERS.mark_reranker();

    // Use the first query as the rerank pivot. Multi-query / HyDE typically
    // wants the most "user-facing" question to anchor the rerank.
    let pivot = queries.first().map_or(String::new(), String::clone);
    let docs: Vec<String> = results
        .iter()
        .map(|r| {
            r.get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        })
        .collect();
    let scores = reranker
        .rerank_blocking(pivot, docs, None)
        .await
        .map_err(|e| SearchError::Cloud(format!("rerank failed: {e}")))?;

    Ok(rerank_postprocess(results, &scores, limit))
}

/// Attach `rerank_score`, recompute trust-aware `confidence` from the
/// sigmoid-normalized reranker logit, re-sort, and truncate (US6 #8/#12).
///
/// For each reranked result we substitute the normalized cross-encoder score
/// for the relevance term, blend it with the cloud's `trust_score` using the
/// compiled-in default policy weights, and record the substitution in
/// `confidence_factors.relevance_source = "rerank"`. Results are then ordered
/// by the recomputed confidence (descending). Results that carry no cloud
/// `trust_score` (e.g. an older cloud, or `include_scores=false`) keep their
/// confidence and fall back to ordering by the reranker score, so the function
/// degrades gracefully. Pure (no model/IO) so it is unit-testable.
fn rerank_postprocess(
    mut results: Vec<serde_json::Value>,
    scores: &[reranker::RerankResult],
    limit: u32,
) -> Vec<serde_json::Value> {
    let policy = ScoringPolicy::default();
    // Dedupe by index in case the model ever returns the same source index
    // twice (defensive — fastembed shouldn't, but a future swap could).
    let mut seen = std::collections::HashSet::new();
    let mut indexed: Vec<(f64, serde_json::Value)> = scores
        .iter()
        .filter_map(|s| {
            let idx = s.index;
            if idx >= results.len() || !seen.insert(idx) {
                return None;
            }
            let relevance = normalize_rerank(f64::from(s.score));
            let mut taken = std::mem::take(&mut results[idx]);
            let sort_key = recompute_confidence(&mut taken, &policy, f64::from(s.score), relevance);
            Some((sort_key, taken))
        })
        .collect();
    // total_cmp gives a strict total order even with NaN inputs (a NaN would
    // otherwise collapse to Ordering::Equal and produce a non-deterministic
    // sort).
    indexed.sort_by(|a, b| b.0.total_cmp(&a.0));
    indexed.truncate(limit as usize);
    indexed.into_iter().map(|(_, v)| v).collect()
}

/// Patch one result in place: attach the raw `rerank_score` logit, and when the
/// cloud supplied a `scores.trust_score`, recompute `scores.confidence` from
/// `relevance` and stamp `relevance_source`/`relevance_multiplier` into
/// `confidence_factors`. Returns the value to sort by (the recomputed
/// confidence, else the normalized relevance when no trust is available).
fn recompute_confidence(
    result: &mut serde_json::Value,
    policy: &ScoringPolicy,
    raw_logit: f64,
    relevance: f64,
) -> f64 {
    let Some(obj) = result.as_object_mut() else {
        return relevance;
    };
    obj.insert("rerank_score".to_owned(), serde_json::Value::from(raw_logit));

    let trust = obj
        .get("scores")
        .and_then(|s| s.get("trust_score"))
        .and_then(serde_json::Value::as_f64);
    let Some(trust) = trust else {
        // No cloud trust to blend with — leave confidence untouched and order
        // by relevance (monotonic in the reranker logit).
        return relevance;
    };
    let confidence = policy.confidence(trust, relevance);
    if let Some(scores) = obj
        .get_mut("scores")
        .and_then(serde_json::Value::as_object_mut)
    {
        scores.insert("confidence".to_owned(), serde_json::Value::from(confidence));
        if let Some(factors) = scores
            .get_mut("confidence_factors")
            .and_then(serde_json::Value::as_object_mut)
        {
            factors.insert("relevance_source".to_owned(), serde_json::Value::from("rerank"));
            factors.insert("relevance_multiplier".to_owned(), serde_json::Value::from(relevance));
        }
    }
    confidence
}

// ---------------------------------------------------------------------------
// pass-through tools (cloud GET only)
// ---------------------------------------------------------------------------

/// Which cloud endpoint a pass-through tool should hit.
#[derive(Debug, Clone, Copy)]
pub enum PassthroughKind {
    /// `/v1/chunks/:id`
    Chunk,
    /// `/v1/chunks/:id/siblings` (DEPRECATED — removed in a later task).
    Siblings,
    /// `/v1/chunks/:id/parents`
    Parents,
    /// `/v1/documents/:id`
    Document,
    /// `/v1/documents/:id/full` (may return [`PassthroughError::TooManyChunks`]).
    DocumentFull,
}

/// Errors for the chunk pass-through tools.
#[derive(Debug)]
pub enum PassthroughError {
    /// `id` arg missing or malformed.
    InvalidInput(String),
    /// Cloud returned 404.
    NotFound(String),
    /// Cloud returned `412 too_many_chunks` (document-full only).
    TooManyChunks {
        /// Reported ready-chunk count for the document.
        chunk_count: u32,
        /// Server's configured cap.
        cap: u32,
        /// Operator-facing hint from the cloud.
        hint: String,
    },
    /// Cloud / transport / decode failure.
    Cloud(String),
}

/// Direction for `run_chunk_nav` — selects `/next` or `/prev`.
#[derive(Debug, Clone, Copy)]
pub enum ChunkNavDirection {
    /// `/v1/chunks/:id/next`
    Next,
    /// `/v1/chunks/:id/prev`
    Prev,
}

const CHUNK_NAV_DEFAULT_COUNT: u32 = 5;
const CHUNK_NAV_MAX_COUNT: u32 = 100;

/// Dispatch `get_chunk_next` / `get_chunk_prev`. Parses `{id, count?}` and
/// rejects out-of-range or non-integer `count` as `InvalidInput` before the
/// wire call.
///
/// # Errors
///
/// See [`PassthroughError`].
pub async fn run_chunk_nav(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
    dir: ChunkNavDirection,
) -> Result<serde_json::Value, PassthroughError> {
    let obj = args.as_object().ok_or_else(|| {
        PassthroughError::InvalidInput("arguments must be a JSON object".to_owned())
    })?;
    let id_str = obj
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PassthroughError::InvalidInput("`id` (string) is required".to_owned()))?;
    Uuid::parse_str(id_str)
        .map_err(|e| PassthroughError::InvalidInput(format!("`id` is not a valid UUID: {e}")))?;

    let count = match obj.get("count") {
        None => CHUNK_NAV_DEFAULT_COUNT,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(PassthroughError::InvalidInput(
                    "`count` must be an integer".to_owned(),
                ));
            };
            if !(1..=i64::from(CHUNK_NAV_MAX_COUNT)).contains(&n) {
                return Err(PassthroughError::InvalidInput(format!(
                    "`count` must be 1..={CHUNK_NAV_MAX_COUNT}"
                )));
            }
            u32::try_from(n).expect("validated above")
        }
    };

    let r = match dir {
        ChunkNavDirection::Next => cloud.get_chunk_next(id_str, count).await,
        ChunkNavDirection::Prev => cloud.get_chunk_prev(id_str, count).await,
    };
    r.map_err(|e| match e {
        CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
        other => PassthroughError::Cloud(other.to_string()),
    })
}

const DOCUMENT_CHUNKS_DEFAULT_FROM: u32 = 0;
const DOCUMENT_CHUNKS_DEFAULT_LIMIT: u32 = 20;
const DOCUMENT_CHUNKS_MAX_LIMIT: u32 = 100;

/// Dispatch `get_document_chunks`. Parses `{id, from?, limit?}`. `from`
/// must be `>= 0`; `limit` must be in `[1, 100]`. Out-of-range or wrong-type
/// values are rejected as `InvalidInput` before the wire call.
///
/// # Errors
///
/// See [`PassthroughError`].
pub async fn run_document_chunks(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
) -> Result<serde_json::Value, PassthroughError> {
    let obj = args.as_object().ok_or_else(|| {
        PassthroughError::InvalidInput("arguments must be a JSON object".to_owned())
    })?;
    let id_str = obj
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PassthroughError::InvalidInput("`id` (string) is required".to_owned()))?;
    Uuid::parse_str(id_str)
        .map_err(|e| PassthroughError::InvalidInput(format!("`id` is not a valid UUID: {e}")))?;

    let from = match obj.get("from") {
        None => DOCUMENT_CHUNKS_DEFAULT_FROM,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(PassthroughError::InvalidInput("`from` must be an integer".to_owned()));
            };
            if n < 0 {
                return Err(PassthroughError::InvalidInput("`from` must be >= 0".to_owned()));
            }
            u32::try_from(n).map_err(|_| {
                PassthroughError::InvalidInput("`from` exceeds 32-bit range".to_owned())
            })?
        }
    };

    let limit = match obj.get("limit") {
        None => DOCUMENT_CHUNKS_DEFAULT_LIMIT,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(PassthroughError::InvalidInput(
                    "`limit` must be an integer".to_owned(),
                ));
            };
            if !(1..=i64::from(DOCUMENT_CHUNKS_MAX_LIMIT)).contains(&n) {
                return Err(PassthroughError::InvalidInput(format!(
                    "`limit` must be 1..={DOCUMENT_CHUNKS_MAX_LIMIT}"
                )));
            }
            u32::try_from(n).expect("validated above")
        }
    };

    cloud
        .get_document_chunks(id_str, from, limit)
        .await
        .map_err(|e| match e {
            CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
            other => PassthroughError::Cloud(other.to_string()),
        })
}

/// Dispatch any of the `get_chunk*` tools. Returns the cloud's JSON verbatim.
///
/// # Errors
///
/// See [`PassthroughError`].
pub async fn run_passthrough_id(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
    kind: PassthroughKind,
) -> Result<serde_json::Value, PassthroughError> {
    let id_str = args
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PassthroughError::InvalidInput("`id` (string) is required".to_owned()))?;
    Uuid::parse_str(id_str)
        .map_err(|e| PassthroughError::InvalidInput(format!("`id` is not a valid UUID: {e}")))?;
    let r = match kind {
        PassthroughKind::Chunk => cloud.get_chunk(id_str).await,
        PassthroughKind::Siblings => cloud.get_chunk_siblings(id_str).await,
        PassthroughKind::Parents => cloud.get_chunk_parents(id_str).await,
        PassthroughKind::Document => cloud.get_document(id_str).await,
        PassthroughKind::DocumentFull => cloud.get_document_full(id_str).await,
    };
    r.map_err(|e| match e {
        CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
        CloudError::TooManyChunks { chunk_count, cap, hint } => {
            PassthroughError::TooManyChunks { chunk_count, cap, hint }
        }
        other => PassthroughError::Cloud(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_has_all_twelve_tools_pre_siblings_removal() {
        // After the 5 new navigation tools are added and before
        // `get_chunk_siblings` is removed, the manifest carries 12 tools.
        let m = list();
        let names: Vec<_> = m.tools.iter().map(|t| t.name).collect();
        for expected in [
            "search",
            "get_chunk",
            "get_chunk_next",
            "get_chunk_prev",
            "get_chunk_siblings",
            "get_chunk_parents",
            "get_document",
            "get_document_full",
            "get_document_chunks",
            "list_sources",
            "pull_models",
            "status",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
        assert_eq!(names.len(), 12, "expected 12 tools, got {}", names.len());
    }

    #[test]
    fn new_navigation_tools_have_object_schemas() {
        let m = list();
        for name in [
            "get_chunk_next",
            "get_chunk_prev",
            "get_document",
            "get_document_full",
            "get_document_chunks",
        ] {
            let tool = m
                .tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("missing tool: {name}"));
            assert_eq!(tool.input_schema["type"], "object", "{name} schema must be object-typed");
            assert_eq!(
                tool.input_schema["additionalProperties"], false,
                "{name} schema must reject additional properties"
            );
        }
    }

    #[test]
    fn parse_search_args_accepts_single_query() {
        let v = json!({ "query": "hello", "limit": 5, "rerank": false });
        let p = parse_search_args(&v).unwrap();
        assert_eq!(p.queries, vec!["hello".to_owned()]);
        assert_eq!(p.limit, 5);
        assert!(!p.rerank);
    }

    #[test]
    fn parse_search_args_accepts_multi_query() {
        let v = json!({ "queries": ["a", "b", "c"] });
        let p = parse_search_args(&v).unwrap();
        assert_eq!(p.queries.len(), 3);
        assert_eq!(p.limit, DEFAULT_LIMIT);
        assert!(p.rerank);
    }

    #[test]
    fn parse_search_args_rejects_both_forms() {
        let v = json!({ "query": "a", "queries": ["b"] });
        assert!(parse_search_args(&v).is_err());
    }

    #[test]
    fn parse_search_args_rejects_empty() {
        assert!(parse_search_args(&json!({})).is_err());
        assert!(parse_search_args(&json!({"query": ""})).is_err());
        assert!(parse_search_args(&json!({"queries": []})).is_err());
    }

    #[test]
    fn parse_search_args_clamps_limit() {
        let v = json!({ "query": "x", "limit": 0 });
        assert!(parse_search_args(&v).is_err());
        let v = json!({ "query": "x", "limit": 51 });
        assert!(parse_search_args(&v).is_err());
    }

    #[test]
    fn search_input_schema_is_object_with_oneof() {
        let s = search_input_schema();
        assert_eq!(s["type"], "object");
        assert!(s.get("oneOf").is_some());
    }

    fn result_with_trust(chunk: &str, trust: f64) -> serde_json::Value {
        json!({
            "chunk_id": chunk,
            "content": format!("content for {chunk}"),
            "scores": {
                "rrf_score": 0.5,
                "trust_score": trust,
                "confidence": 0.4,
                "confidence_factors": { "relevance_source": "rrf", "relevance_multiplier": 0.4 },
            },
        })
    }

    #[test]
    fn rerank_recomputes_confidence_and_marks_source() {
        // #8/#12: confidence is recomputed from the sigmoid-normalized logit and
        // the substitution is recorded.
        let results = vec![result_with_trust("a", 0.9)];
        let scores = vec![reranker::RerankResult { index: 0, score: 2.0 }];
        let out = rerank_postprocess(results, &scores, 10);
        assert_eq!(out.len(), 1);
        let s = &out[0]["scores"];
        assert_eq!(s["confidence_factors"]["relevance_source"], "rerank");
        // relevance_multiplier == sigmoid(2.0) ≈ 0.8808.
        let rel = s["confidence_factors"]["relevance_multiplier"]
            .as_f64()
            .unwrap();
        assert!((rel - 0.880_797).abs() < 1e-4, "relevance was {rel}");
        // confidence recomputed away from the cloud's 0.4 placeholder.
        let conf = s["confidence"].as_f64().unwrap();
        assert!((conf - 0.4).abs() > 1e-6 && (0.0..=1.0).contains(&conf));
        assert!(out[0]["rerank_score"].as_f64().unwrap() > 1.99);
    }

    #[test]
    fn rerank_orders_by_recomputed_confidence_not_logit() {
        // High-trust chunk with a slightly lower logit should still outrank a
        // low-trust chunk with a slightly higher logit, because confidence
        // blends trust in.
        let results = vec![
            result_with_trust("low_trust", 0.05),
            result_with_trust("high_trust", 0.99),
        ];
        let scores = vec![
            reranker::RerankResult { index: 0, score: 1.2 }, // low trust, higher logit
            reranker::RerankResult { index: 1, score: 1.0 }, // high trust, lower logit
        ];
        let out = rerank_postprocess(results, &scores, 10);
        assert_eq!(out[0]["chunk_id"], "high_trust", "trust must lift the top result");
        assert_eq!(out[1]["chunk_id"], "low_trust");
    }

    #[test]
    fn rerank_passes_through_without_trust() {
        // A result missing scores.trust_score keeps its confidence and just
        // gains a rerank_score (graceful degradation).
        let results = vec![json!({ "chunk_id": "x", "content": "c" })];
        let scores = vec![reranker::RerankResult { index: 0, score: 0.5 }];
        let out = rerank_postprocess(results, &scores, 10);
        assert_eq!(out.len(), 1);
        assert!(out[0].get("scores").is_none());
        assert!(out[0]["rerank_score"].is_number());
    }

    #[test]
    fn rerank_truncates_to_limit() {
        let results = vec![
            result_with_trust("a", 0.8),
            result_with_trust("b", 0.7),
            result_with_trust("c", 0.6),
        ];
        let scores = vec![
            reranker::RerankResult { index: 0, score: 0.1 },
            reranker::RerankResult { index: 1, score: 0.9 },
            reranker::RerankResult { index: 2, score: 0.5 },
        ];
        let out = rerank_postprocess(results, &scores, 2);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn status_reports_models() {
        let s = run_status(None);
        assert_eq!(s.embedder, "bge-base-en-v1.5");
        assert_eq!(s.reranker, "bge-reranker-base");
        assert!(matches!(s.model_state, ModelState::Missing | ModelState::Ready));
    }
}

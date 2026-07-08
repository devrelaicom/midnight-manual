//! `POST /v1/search` — hybrid FTS + pgvector retrieval, then inline rerank.
//!
//! For each distinct query pair the handler runs a pgvector cosine search, a
//! Postgres full-text search, and — when the effective `code_mode` is not
//! `off` — a second cosine search over the partial voyage-code-3
//! `code_embedding` index, then fuses every ranked list — across all modes and
//! all query pairs — in a single Reciprocal Rank Fusion pass (k=60). When
//! `rerank != "none"` it then reranks the top candidate pool inline via Voyage
//! (server key, charged to the caller's token budget), degrading to RRF order
//! on any failure (spec §1). Confidence scoring blends trust × relevance.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Extension, State};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mnm_core::error::{Error as CoreError, ErrorCode};
use mnm_core::provenance::Provenance;
use mnm_core::scoring::{self, ConfidenceFactors, RelevanceSource, ScoreResult};
use mnm_retrieval::filters::SearchFilters;
use pgvector::Vector;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, QueryBuilder, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::rate_limit::RateLimitContext;
use crate::middleware::request_id::RequestId;
use crate::observability;
use crate::ratelimit::Decision;

/// Mount the search route.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/search", post(search))
}

/// Request body shape for `POST /v1/search`.
///
/// Two mutually-exclusive input forms are accepted: the canonical `queries`
/// array (multi-query, RRF across pairs) or the single-query convenience form
/// `{query, vector}` (D3 / acceptance #6). They are normalized to the same
/// internal query list by `normalize_queries`.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchRequest {
    /// Zero or more query pairs. Multi-query (>1) RRFs across pairs. Mutually
    /// exclusive with the convenience `query`/`vector` fields.
    #[serde(default)]
    pub queries: Vec<QueryPair>,
    /// Single-query convenience form: the query text. Pairs with `vector` and
    /// is mutually exclusive with `queries`.
    #[serde(default)]
    pub query: Option<String>,
    /// Single-query convenience form: the pre-computed embedding for `query`.
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
    /// The embedding model identifier the client used. REQUIRED for `hybrid`
    /// and `vector` modes; optional/ignored for `fts` mode.
    #[serde(default)]
    pub client_embedding_model: Option<String>,
    /// Maximum number of results to return. Capped at 100 server-side.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Search filters (AND across keys, OR within each array).
    #[serde(default)]
    pub filters: SearchFilters,
    /// Result ordering key (US6 acceptance #9). Defaults to `confidence`.
    #[serde(default)]
    pub sort_by: SortBy,
    /// Which retrieval halves to run. Defaults to `hybrid`.
    #[serde(default)]
    pub mode: SearchMode,
    /// Drop results whose `confidence` is below this floor before applying
    /// `limit` (US6 acceptance #10). Defaults to 0.0 (no filtering).
    #[serde(default)]
    pub min_confidence: f64,
    /// When `false`, omit the per-result `scores` object from the response.
    /// Defaults to `true`.
    #[serde(default = "default_true")]
    pub include_scores: bool,
    /// Code-vector fusion mode. Defaults to `on` for hybrid/vector, forced
    /// `off` for fts (where `on`/`exclusive` is a 400).
    #[serde(default)]
    pub code_mode: Option<CodeMode>,
    /// The code-embedding model wire id the client used for `code_vector`s.
    /// REQUIRED when the effective code_mode != off.
    #[serde(default)]
    pub client_code_embedding_model: Option<String>,
    /// Single-query convenience form: the voyage-code-3 embedding for `query`.
    #[serde(default)]
    pub code_vector: Option<Vec<f32>>,
    /// Server-side rerank model, or `"none"` to skip (spec §1). Omitted ⇒
    /// `rerank-2.5`. Clients that rerank locally always send `"none"`.
    #[serde(default)]
    pub rerank: Option<mnm_core::rerank::RerankParam>,
    /// Optional natural-language rerank instruction (≤400 chars; replaces the
    /// derived default wholesale, D4). Ignored when rerank is `"none"`.
    #[serde(default)]
    pub rerank_instructions: Option<String>,
    /// Version-matching mode for the semver-bearing facets (spec §3): strict
    /// hard-filters; permissive (default) biases, dropping only breaking
    /// mismatches among version-declaring content.
    #[serde(default)]
    pub version_match: mnm_retrieval::filters::VersionMatchMode,
}

/// Whether the code-vector ranked list joins the RRF pool (D5/D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeMode {
    /// Fuse the code-vector list alongside the general lists. Default for
    /// hybrid/vector modes.
    On,
    /// General retrieval only (pre-dual-embeddings behavior). Forced for fts.
    Off,
    /// Code-vector list replaces the general vector list.
    Exclusive,
}

/// How to order the result set (US6 acceptance #9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    /// Blended trust × relevance confidence, descending. The default.
    #[default]
    Confidence,
    /// Content `trust_score`, descending.
    Trust,
    /// The relevance term used (normalized RRF here), descending.
    Relevance,
    /// The underlying RRF score, descending (the pre-US6 Story 4 default).
    Score,
}

/// Which retrieval halves to run (per-request).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Run both pgvector and FTS, fuse via RRF. The default.
    #[default]
    Hybrid,
    /// pgvector only. Requires `vector` + `client_embedding_model`.
    Vector,
    /// FTS only. `vector` / `client_embedding_model` are optional and ignored.
    Fts,
}

/// One {text, vector} pair.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct QueryPair {
    /// The query text, used for the full-text-search half of retrieval.
    pub text: String,
    /// The pre-computed embedding; its dimension must match the active corpus
    /// model. Optional so `fts`-mode callers can omit it.
    #[serde(default)]
    pub vector: Vec<f32>,
    /// The pre-computed code-model embedding; required iff code_mode != off.
    #[serde(default)]
    pub code_vector: Vec<f32>,
}

const fn default_limit() -> u32 {
    20
}

const fn default_true() -> bool {
    true
}

const fn max_limit() -> u32 {
    100
}

/// Whether a `min_confidence` value is an acceptable confidence floor (#165).
/// Valid floors lie within the closed unit interval; `RangeInclusive::contains`
/// also rejects `NaN` and `±inf` (each compares `false` against the bounds), so
/// an out-of-range value is a `400` rather than being silently clamped up to
/// `1.0` — which would make the downstream `retain(confidence >= floor)` drop
/// nearly every candidate and return an empty page with no error signal.
fn min_confidence_valid(v: f64) -> bool {
    (0.0..=1.0).contains(&v)
}

/// Whether result-set overlap dedup runs. Default on; set `MNM_SEARCH_DEDUP=0`
/// (or `false`) to disable as an escape hatch.
fn dedup_enabled() -> bool {
    !matches!(std::env::var("MNM_SEARCH_DEDUP").as_deref(), Ok("0" | "false"))
}

/// Response body shape.
#[derive(Debug, Serialize)]
pub struct SearchResponse {
    /// Ordered list of matching chunks.
    pub results: Vec<SearchResult>,
    /// Per-query diagnostics + global counters.
    pub search_metadata: SearchMetadata,
}

/// One result (chunk + light metadata; full metadata lands in a follow-up).
#[derive(Debug, Serialize)]
pub struct SearchResult {
    /// Chunk identifier.
    pub chunk_id: Uuid,
    /// The chunk's text content.
    pub content: String,
    /// Document identifier.
    pub document_id: Uuid,
    /// Source version identifier (active version at the time of the query).
    pub source_version_id: Uuid,
    /// 0-indexed position within its document.
    pub chunk_index: i32,
    /// Total chunks in the parent document.
    pub total_chunks: i32,
    /// Created-at timestamp.
    pub created_at: OffsetDateTime,
    /// URL-safe source handle.
    pub source_slug: String,
    /// Human-readable source name.
    pub source_display_name: String,
    /// Source-relative path of the parent document, e.g. `docs/intro.md`.
    pub source_path: String,
    /// Canonical published URL, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_url: Option<String>,
    /// Original source URL, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// Markdown heading breadcrumb for this chunk.
    pub heading_path: Vec<String>,
    /// Code symbol breadcrumb for this chunk.
    pub symbol_path: Vec<String>,
    /// Per-result scores. Omitted when the request sets `include_scores=false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scores: Option<ScoreBreakdown>,
}

/// Score breakdown — additive; new fields are appended in later phases.
#[derive(Debug, Serialize)]
pub struct ScoreBreakdown {
    /// Reciprocal Rank Fusion score (k=60) used to order results.
    pub rrf_score: f64,
    /// Best pgvector cosine similarity across the queries that vector-matched
    /// this chunk, normalized to 0..=1 (`1 - distance`); 0.0 if the chunk was
    /// found only via FTS.
    pub vector_similarity: f64,
    /// 0-based indices of the distinct input queries that contributed at least
    /// one FTS or vector rank to this result.
    pub matched_queries: Vec<usize>,
    /// Content trust in `[0, 1]`, from provenance (US6, D24).
    pub trust_score: f64,
    /// Blended trust × relevance confidence in `[0, 1]` (US6, D24).
    pub confidence: f64,
    /// Per-factor breakdown explaining the trust + confidence values.
    pub confidence_factors: ConfidenceFactors,
    /// Voyage relevance score in [0, 1]; present only when the server reranked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank_score: Option<f64>,
}

/// Per-query timings and counters.
#[derive(Debug, Serialize)]
pub struct SearchMetadata {
    /// Per-query records, one per distinct input query.
    pub per_query: Vec<PerQueryRecord>,
    /// Total candidates considered before the limit cap.
    pub total_candidates: usize,
    /// How many input queries were dropped as duplicates before retrieval
    /// (EC-90). Duplicates do not inflate the rate-limit cost.
    pub deduplicated_count: usize,
    /// How many results were dropped as fully-overlapping duplicates of a
    /// higher-ranked chunk from the same document (rolling-window dedup).
    pub overlap_dropped_count: usize,
    /// How many results had overlapping text trimmed out (with an `…` elision
    /// marker) against a higher-ranked same-document chunk (rolling-window dedup).
    pub overlap_trimmed_count: usize,
    /// How many candidates were dropped for falling below `min_confidence`
    /// before the limit was applied (US6 acceptance #10).
    pub filtered_by_confidence: usize,
    /// The ordering key actually applied (echoes the request, default
    /// `confidence`), so callers can confirm the resolved sort.
    pub sort_by: SortBy,
    /// The effective code_mode applied (request value or mode-derived default).
    pub code_mode: CodeMode,
    /// The version-matching mode applied (echoes the request; default permissive).
    pub version_match: mnm_retrieval::filters::VersionMatchMode,
    /// Outcome of the inline rerank stage (spec §1), reported on every response.
    pub rerank: RerankMetadata,
}

/// Outcome of the rerank stage, reported on every response (spec §1).
#[derive(Debug, Serialize)]
pub struct RerankMetadata {
    /// Whether a Voyage rerank was applied to this result set.
    pub applied: bool,
    /// The model attempted/applied; absent when rerank was `"none"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'static str>,
    /// Why rerank was not applied: `not_requested` | `token_budget_exhausted`
    /// | `provider_error` | `disabled`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

/// Server-side rerank candidate-pool floor (mirrors the clients' RERANK_FETCH).
const RERANK_POOL: u32 = 50;

/// Remediation for the code-vector DIMENSION guard. Shared between the route
/// (below) and `mismatch_envelope_tests` so the string is guarded against
/// drifting back to a non-actionable form: reverting the route to an inline
/// string leaves this `const` unused, which `-D warnings` clippy rejects, and
/// the envelope test pins its wording. It mirrors its three sibling query-time
/// mismatch remediations by naming `mnm models active` — the command that
/// prints exactly the code `dim` this guard rejects on (#140).
const CODE_VECTOR_DIM_REMEDIATION: &str =
    "re-embed code queries with the corpus's active code model (see `mnm models active`)";

/// Pool size: at least [`RERANK_POOL`], never below the caller's `limit`.
const fn rerank_pool_size(limit: u32) -> u32 {
    if limit > RERANK_POOL {
        limit
    } else {
        RERANK_POOL
    }
}

/// Pre-gate estimate of a rerank's token cost, in Voyage's formula
/// `(query_tokens × num_documents) + sum(document_tokens)`, using the same
/// ~4-bytes/token heuristic as the embeddings route. The reservation is
/// settled against Voyage's reported count, so slack here only affects
/// in-flight gating, never the final balance.
fn rerank_token_estimate(query: &str, docs: &[String]) -> u64 {
    if docs.is_empty() {
        return 0;
    }
    let est = |s: &str| (s.len() as u64).div_ceil(4).max(1);
    est(query) * docs.len() as u64 + docs.iter().map(|d| est(d)).sum::<u64>()
}

/// One per-query record.
#[derive(Debug, Serialize)]
pub struct PerQueryRecord {
    /// 0-indexed distinct-query position.
    pub query_index: usize,
    /// FTS-mode candidate count for this query.
    pub fts_candidates: usize,
    /// FTS-mode latency in milliseconds.
    pub fts_latency_ms: f64,
    /// Vector-mode candidate count for this query.
    pub vector_candidates: usize,
    /// Vector-mode latency in milliseconds.
    pub vector_latency_ms: f64,
    /// Code-vector candidate count for this query.
    pub code_vector_candidates: usize,
    /// Code-vector latency in milliseconds.
    pub code_vector_latency_ms: f64,
}

/// Resolve the effective code mode for a request (D6 defaults; spec §10.2).
/// `Err(())` = fts with an explicit on/exclusive (400).
const fn effective_code_mode(
    mode: SearchMode,
    requested: Option<CodeMode>,
) -> Result<CodeMode, ()> {
    match (mode, requested) {
        (SearchMode::Fts, None | Some(CodeMode::Off)) => Ok(CodeMode::Off),
        (SearchMode::Fts, Some(_)) => Err(()),
        (_, Some(m)) => Ok(m),
        (_, None) => Ok(CodeMode::On),
    }
}

#[allow(clippy::too_many_lines)]
async fn search(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    rl: Option<Extension<RateLimitContext>>,
    headers: axum::http::HeaderMap,
    auth: Option<Extension<crate::middleware::bearer::AuthContext>>,
    // NOTE: reads only the connect-info Extension set by
    // `into_make_service_with_connect_info` (production); it does NOT observe
    // axum's `MockConnectInfo` test helper — inject a peer addr in tests via
    // `.layer(Extension(ConnectInfo(addr)))`, not `MockConnectInfo`.
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(req): Json<SearchRequest>,
) -> Response {
    let rid = req_id.as_str();
    let rl_ctx = rl.as_ref().map(|Extension(c)| c);
    // Socket peer IP, used to key the token limiter when the trusted proxy
    // header is absent (issue #176 L15). Threaded into `rerank_stage`.
    let peer_ip = peer.map(|Extension(ConnectInfo(sa))| sa.ip());

    // Which retrieval halves this mode runs (loop-invariant).
    let run_vector = matches!(req.mode, SearchMode::Hybrid | SearchMode::Vector);
    let run_fts = matches!(req.mode, SearchMode::Hybrid | SearchMode::Fts);

    // Resolve the effective code mode (D6 defaults): `on` for hybrid/vector,
    // forced `off` for fts — where an explicit on/exclusive is a 400.
    let Ok(code_mode) = effective_code_mode(req.mode, req.code_mode) else {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message("code_mode on/exclusive is incompatible with mode=fts")
                .remediation("drop code_mode, or use mode=hybrid/vector")
                .build(),
            rid,
        );
    };
    // Exclusive replaces the general vector list with the code-vector list.
    let run_general_vector = run_vector && !matches!(code_mode, CodeMode::Exclusive);
    let run_code_vector = run_vector && !matches!(code_mode, CodeMode::Off);

    // Pseudonymous identity + latency clock for observability (Task 8). Set
    // identity here — before any of the error early-returns below — so a
    // rejected request still carries the caller's identity on its Sentry
    // events. Metadata only: no query content is attached in this task.
    observability::set_request_identity(
        auth.as_deref(),
        state.cfg.sentry.identity_secret.as_deref(),
    );
    let started = std::time::Instant::now();

    // Normalize the single-query convenience form `{query, vector}` (#6) into
    // the canonical query list. Ambiguous/incomplete requests are rejected.
    let queries = match normalize_queries(&req, code_mode) {
        Ok(queries) => queries,
        Err((message, remediation)) => {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message(message)
                    .remediation(remediation)
                    .build(),
                rid,
            );
        }
    };

    // Cap check (EC-88) — before any work or rate-limit consumption. The
    // effective cap is the configured value clamped to the hard ceiling of 50.
    let cap = state.cfg.max_queries_per_request.min(50);
    if queries.len() > cap as usize {
        // Refund the single token the middleware already charged so an
        // over-cap request truly costs nothing.
        if let (Some(limiter), Some(ctx)) = (state.rate_limiter.as_ref(), rl_ctx) {
            limiter.refund(&ctx.key, ctx.limit, 1);
        }
        return error::into_response(
            CoreError::builder(ErrorCode::MultiQueryLimitExceeded)
                .message(format!(
                    "queries.length {} exceeds the per-request cap of {cap}",
                    queries.len()
                ))
                .remediation(format!(
                    "reduce queries.length; the configured cap is {cap} and the hard ceiling is 50"
                ))
                .build(),
            rid,
        );
    }

    // Validate request shape.
    if queries.is_empty() {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message("queries must contain at least one entry")
                .remediation("supply one or more `{text, vector}` pairs in the `queries` array")
                .build(),
            rid,
        );
    }

    // Reject when every query has empty/whitespace-only text (#7): such a
    // request can never drive the FTS half of retrieval and signals a malformed
    // caller. Only applies when FTS actually runs (hybrid or fts mode); a
    // vector-only request has no use for query text.
    if run_fts && queries.iter().all(|q| q.text.trim().is_empty()) {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message("every query has empty `text`")
                .remediation("supply non-empty query text so full-text search can run")
                .build(),
            rid,
        );
    }
    let limit = req.limit.min(max_limit()).max(1);

    // Reject an out-of-range `min_confidence` rather than clamping it (#165).
    // Silently clamping e.g. `5.0` or `1e400` (parses to +inf) up to `1.0` makes
    // the later `retain(confidence >= floor)` drop nearly everything and returns
    // an empty result set with no signal that the parameter was invalid.
    if !min_confidence_valid(req.min_confidence) {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!(
                    "min_confidence {} is out of range; must be within [0.0, 1.0]",
                    req.min_confidence
                ))
                .remediation("supply a min_confidence between 0.0 and 1.0 (inclusive)")
                .build(),
            rid,
        );
    }

    // Validate the filter object at the boundary (registry-backed closed-set
    // checks, range ordering, semver parseability). A bad filter is a 400.
    if let Err(e) = req.filters.validate() {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!("invalid filter `{}`: {}", e.facet, e.message))
                .remediation("see GET /v1/facets for valid facets and values")
                .context("facet", e.facet)
                .build(),
            rid,
        );
    }

    // Instruction cap (spec §1): reject, never truncate silently.
    if let Some(instr) = req.rerank_instructions.as_deref() {
        if let Err(msg) = mnm_core::rerank::validate_instruction(instr) {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message(msg)
                    .remediation("shorten rerank_instructions to 400 characters or fewer")
                    .build(),
                rid,
            );
        }
    }

    // Model-mismatch guard. The active corpus model is resolved at boot (and
    // re-resolved after each ingest finalize) into the AppState RwLock; if it's
    // somehow None here the server is mis-configured and we 503 rather than
    // silently compare against a hardcoded literal (which used to cause spec
    // drift if migration 0006 ever seeded a different revision).
    // Snapshot the model behind the lock and drop the guard immediately, so the
    // read lock isn't held across the rest of the handler.
    let snapshot = state
        .corpus_model
        .read()
        .expect("corpus_model lock poisoned")
        .clone();
    let Some(cm) = snapshot else {
        return error::service_unavailable(
            "server has no resolved corpus_model; check boot logs",
            rid,
        );
    };

    // The general vector half (and its model/dim guards) only runs in
    // hybrid/vector mode with a non-exclusive code_mode. fts mode skips
    // embedding entirely, and exclusive code mode replaces the general list,
    // so `client_embedding_model` and `vector` are optional and ignored.
    if run_general_vector {
        let Some(client_model) = req.client_embedding_model.as_deref() else {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message("client_embedding_model is required for hybrid/vector mode")
                    .remediation("supply client_embedding_model, or use mode=fts to skip embedding")
                    .build(),
                rid,
            );
        };
        if client_model != cm.wire {
            return error::into_response(
                CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                    .message(format!(
                        "client_embedding_model `{client_model}` does not match corpus model `{}`",
                        cm.wire,
                    ))
                    .remediation("run `mnm models active` to see the corpus's active model, then re-embed the query with it")
                    .context("corpus_model", cm.wire.clone())
                    .context("client_model", client_model.to_owned())
                    .build(),
                rid,
            );
        }
        for (i, q) in queries.iter().enumerate() {
            if q.vector.len() != cm.dim {
                return error::into_response(
                    CoreError::builder(ErrorCode::InvalidRequest)
                        .message(format!(
                            "queries[{i}].vector has {} dimensions; expected {}",
                            q.vector.len(),
                            cm.dim,
                        ))
                        .remediation("re-embed the query with the corpus's active model (see `mnm models active`)")
                        .build(),
                    rid,
                );
            }
        }
    }

    // Code-model guards (mirror the general ones): require the client's code
    // model wire id, match it against the config-pinned code model, and check
    // every query's `code_vector` dimension.
    let code_model_id: Option<Uuid> = if run_code_vector {
        let code_snapshot = state
            .code_model
            .read()
            .expect("code_model lock poisoned")
            .clone();
        let Some(km) = code_snapshot else {
            return error::service_unavailable(
                "server has no resolved code model; check boot logs",
                rid,
            );
        };
        let Some(client_model) = req.client_code_embedding_model.as_deref() else {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message("client_code_embedding_model is required when code_mode != off")
                    .remediation("supply client_code_embedding_model, or set code_mode=off")
                    .build(),
                rid,
            );
        };
        if client_model != km.wire {
            return error::into_response(
                CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                    .message(format!(
                        "client_code_embedding_model `{client_model}` does not match code model `{}`",
                        km.wire,
                    ))
                    .remediation("run `mnm models active` to see the corpus's active code-embedding model, then re-embed code queries with it")
                    // Emit under `corpus_model` (not `code_model`) so this 409
                    // matches the general-model guard above and the MCP client's
                    // `parse_mismatch`, which reads only `corpus_model`/`client_model`.
                    // The code model IS the corpus's active code model.
                    .context("corpus_model", km.wire.clone())
                    .context("client_model", client_model.to_owned())
                    .build(),
                rid,
            );
        }
        for (i, q) in queries.iter().enumerate() {
            if q.code_vector.len() != km.dim {
                return error::into_response(
                    CoreError::builder(ErrorCode::InvalidRequest)
                        .message(format!(
                            "queries[{i}].code_vector has {} dimensions; expected {}",
                            q.code_vector.len(),
                            km.dim,
                        ))
                        .remediation(CODE_VECTOR_DIM_REMEDIATION)
                        .build(),
                    rid,
                );
            }
        }
        Some(km.id)
    } else {
        None
    };

    // Deduplicate identical {text, vector} pairs (EC-90) so duplicates don't
    // inflate the rate-limit cost. First-occurrence order is preserved.
    let mut seen = std::collections::HashSet::new();
    let mut distinct: Vec<&QueryPair> = Vec::new();
    for q in &queries {
        if seen.insert(query_hash(q)) {
            distinct.push(q);
        }
    }
    let deduplicated_count = queries.len() - distinct.len();

    // Charge the multi-query premium (D25): total cost is `max(1, distinct)`.
    // The middleware already charged 1, so charge the remainder against the
    // same bucket. EC-92: insufficient budget returns 429 naming the cost;
    // the `X-RateLimit-*` headers (set by the middleware on the way out)
    // reflect the post-charge balance.
    if let (Some(limiter), Some(ctx)) = (state.rate_limiter.as_ref(), rl_ctx) {
        let extra = u32::try_from(distinct.len().saturating_sub(1)).unwrap_or(u32::MAX);
        if extra > 0 {
            if let Decision::Rejected { .. } = limiter.charge(&ctx.key, ctx.limit, extra) {
                let remaining = match limiter.charge(&ctx.key, ctx.limit, 0) {
                    Decision::Allowed { remaining, .. } => remaining,
                    Decision::Rejected { .. } => 0,
                };
                return error::into_response(
                    CoreError::builder(ErrorCode::RateLimited)
                        .message(format!(
                            "rate-limit budget insufficient for {} distinct queries \
                             (cost {} tokens); {remaining} remaining",
                            distinct.len(),
                            distinct.len()
                        ))
                        .remediation("reduce queries.length or request a higher rate-limit tier")
                        .build(),
                    rid,
                );
            }
        }
    }

    // Hybrid retrieval: for each distinct query run BOTH pgvector and FTS,
    // collecting one ranked candidate list per (query, mode). RRF (k=60) then
    // fuses every list — across modes and across queries — in a single pass.
    // Candidates are restricted to chunks on the corpus model's source_versions
    // so off-model rows never surface. Copied into a local `Uuid` so the loop
    // body doesn't borrow `cm`.
    let corpus_model_id = cm.id;
    let mut per_query = Vec::with_capacity(distinct.len());
    let mut ranked_lists: Vec<Vec<Uuid>> = Vec::with_capacity(distinct.len() * 3);
    // Per chunk: which distinct queries contributed at least one rank, and the
    // best vector similarity seen (for reporting; FTS-only chunks stay at 0.0).
    let mut matched: std::collections::HashMap<Uuid, std::collections::BTreeSet<usize>> =
        std::collections::HashMap::new();
    let mut best_similarity: std::collections::HashMap<Uuid, f64> =
        std::collections::HashMap::new();

    for (i, q) in distinct.iter().enumerate() {
        // Per-mode gating uses the hoisted, loop-invariant flags: hybrid runs
        // both halves; vector/fts run one each. The code-vector list is a
        // third half gated by the effective code_mode (exclusive swaps it in
        // for the general list).
        let (vector_hits, vector_latency_ms): (Vec<(Uuid, f64)>, f64) = if run_general_vector {
            let t0 = std::time::Instant::now();
            let hits = match vector_search(
                &state.pool,
                &q.vector,
                &req.filters,
                req.version_match,
                corpus_model_id,
            )
            .await
            {
                Ok(hits) => hits,
                Err(e) => {
                    tracing::warn!(request_id = rid, error = %e, query_index = i, "vector search failed");
                    return error::service_unavailable(
                        format!("vector search failed for query {i}"),
                        rid,
                    );
                }
            };
            (hits, t0.elapsed().as_secs_f64() * 1000.0)
        } else {
            (Vec::new(), 0.0)
        };

        let (code_hits, code_vector_latency_ms): (Vec<(Uuid, f64)>, f64) = if run_code_vector {
            let t = std::time::Instant::now();
            let id = code_model_id.expect("validated above");
            let hits = match code_vector_search(
                &state.pool,
                &q.code_vector,
                &req.filters,
                req.version_match,
                id,
            )
            .await
            {
                Ok(hits) => hits,
                Err(e) => {
                    tracing::warn!(request_id = rid, error = %e, query_index = i, "code vector search failed");
                    return error::service_unavailable(
                        format!("code vector search failed for query {i}"),
                        rid,
                    );
                }
            };
            (hits, t.elapsed().as_secs_f64() * 1000.0)
        } else {
            (Vec::new(), 0.0)
        };

        let (fts_hits, fts_latency_ms): (Vec<Uuid>, f64) = if run_fts {
            let t1 = std::time::Instant::now();
            let hits = match fts_search(
                &state.pool,
                &q.text,
                &req.filters,
                req.version_match,
                corpus_model_id,
            )
            .await
            {
                Ok(hits) => hits,
                Err(e) => {
                    tracing::warn!(request_id = rid, error = %e, query_index = i, "fts search failed");
                    return error::service_unavailable(
                        format!("fts search failed for query {i}"),
                        rid,
                    );
                }
            };
            (hits, t1.elapsed().as_secs_f64() * 1000.0)
        } else {
            (Vec::new(), 0.0)
        };

        per_query.push(PerQueryRecord {
            query_index: i,
            fts_candidates: fts_hits.len(),
            fts_latency_ms,
            vector_candidates: vector_hits.len(),
            vector_latency_ms,
            code_vector_candidates: code_hits.len(),
            code_vector_latency_ms,
        });

        let mut vector_ids = Vec::with_capacity(vector_hits.len());
        for (id, sim) in vector_hits {
            matched.entry(id).or_default().insert(i);
            best_similarity
                .entry(id)
                .and_modify(|s| {
                    if sim > *s {
                        *s = sim;
                    }
                })
                .or_insert(sim);
            vector_ids.push(id);
        }
        let mut code_ids = Vec::with_capacity(code_hits.len());
        for (id, sim) in code_hits {
            matched.entry(id).or_default().insert(i);
            best_similarity
                .entry(id)
                .and_modify(|s| {
                    if sim > *s {
                        *s = sim;
                    }
                })
                .or_insert(sim);
            code_ids.push(id);
        }
        for id in &fts_hits {
            matched.entry(*id).or_default().insert(i);
        }

        // Only push the lists for halves that actually ran, so RRF doesn't fuse
        // empty placeholder lists (which would otherwise be harmless no-ops).
        if run_general_vector {
            ranked_lists.push(vector_ids);
        }
        if run_code_vector {
            ranked_lists.push(code_ids);
        }
        if run_fts {
            ranked_lists.push(fts_hits);
        }
    }

    // Single RRF pass across all (query, mode) lists. We score the FULL fused
    // candidate set (not just the top `limit`) so confidence filtering and the
    // sort_by reorder operate over every candidate before truncation (#9/#10).
    let list_refs: Vec<&[Uuid]> = ranked_lists.iter().map(Vec::as_slice).collect();
    let fused = mnm_retrieval::rrf::fuse(&list_refs);
    let total_candidates = fused.len();

    // Batch-fetch every fused candidate joined with its document (provenance +
    // freshness) and source_version (ingest timestamp).
    let fused_ids: Vec<Uuid> = fused.iter().map(|(id, _)| *id).collect();
    let mut rows = match fetch_scoring_rows(&state.pool, &fused_ids).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "scoring-row fetch failed");
            return error::service_unavailable("result fetch failed", rid);
        }
    };

    let mode = req.version_match;
    let now = OffsetDateTime::now_utc();

    // Score each candidate in fused order. Rows missing (deleted since the
    // candidate fetch) are skipped. We `remove` (rather than `get`) each row so
    // its `content` and metadata are *moved* into the `ScoredCandidate` below
    // instead of cloned — `fused` keys are unique (RRF fuses on a HashMap), so
    // no candidate is consumed twice, and `rows` never co-holds a second copy of
    // the full candidate text (#165: avoids a ~2× resident hold of the pool).
    let mut scored: Vec<ScoredCandidate> = Vec::with_capacity(fused.len());
    for (chunk_id, rrf_score) in fused {
        let Some(row) = rows.remove(&chunk_id) else {
            continue;
        };
        // Version-bearing facets (FR-033, spec §3): classify, then drop per
        // mode — strict drops everything not Satisfies; permissive drops only
        // Breaking. Scalar facets were already enforced in SQL.
        let outcomes = req.filters.version_outcomes(&row.provenance);
        let version_input =
            match version_decision(&req.filters, outcomes, mode, &state.scoring_policy) {
                VersionDecision::Drop => continue,
                VersionDecision::Score(v) => v,
            };
        // BTreeSet iterates ascending, so matched_queries is sorted.
        let matched_queries: Vec<usize> = matched
            .get(&chunk_id)
            .into_iter()
            .flatten()
            .copied()
            .collect();
        let vector_similarity = best_similarity.get(&chunk_id).copied().unwrap_or(0.0);
        let relevance = scoring::normalize_rrf(rrf_score);
        let age_days = age_in_days(now, row.source_modified_at, row.ingested_at);
        let score = state.scoring_policy.score(
            &row.provenance,
            version_input.as_ref(),
            age_days,
            relevance,
            RelevanceSource::Rrf,
        );
        scored.push(ScoredCandidate {
            chunk_id,
            content: row.content,
            document_id: row.document_id,
            source_version_id: row.source_version_id,
            chunk_index: row.chunk_index,
            total_chunks: row.total_chunks,
            start_byte: row.start_byte,
            end_byte: row.end_byte,
            created_at: row.created_at,
            rrf_score,
            vector_similarity,
            matched_queries,
            relevance,
            score,
            rerank_score: None,
            source_slug: row.source_slug,
            source_display_name: row.source_display_name,
            source_path: row.source_path,
            published_url: row.published_url,
            source_url: row.source_url,
            heading_path: row.heading_path,
            symbol_path: row.symbol_path,
        });
    }

    // ---- Rerank stage (spec §1). Degrades, never fails search. ----
    // Pool by relevance (RRF score) order, dedup overlaps first (don't pay
    // Voyage for dupes), then keep the top max(limit, 50). The Voyage relevance
    // scores then drive recomputed confidences for min_confidence/sort/truncate.
    let rerank_param = req.rerank.unwrap_or_default();
    let rerank_meta = if let Some(model) = rerank_param.model_name() {
        sort_candidates(&mut scored, SortBy::Score);
        let dedup_stats_early;
        (scored, dedup_stats_early) = if dedup_enabled() {
            mnm_retrieval::dedup::trim_overlaps(scored)
        } else {
            (scored, mnm_retrieval::dedup::DedupStats::default())
        };
        scored.truncate(rerank_pool_size(limit) as usize);
        rerank_stage(
            &state,
            &req,
            &queries,
            &headers,
            auth.as_ref().map(|Extension(c)| c),
            peer_ip,
            model,
            rerank_param,
            &mut scored,
            rid,
        )
        .await
        // dedup already ran for this path; remember its stats
        .with_dedup(dedup_stats_early)
    } else {
        RerankOutcome::not_requested()
    };

    // Drop candidates below the confidence floor before applying `limit` (#10).
    // `min_confidence` was validated to be within [0.0, 1.0] at the boundary
    // (out-of-range is a 400), so no clamp is needed here (#165).
    let min_confidence = req.min_confidence;
    let before = scored.len();
    scored.retain(|c| c.score.confidence >= min_confidence);
    let filtered_by_confidence = before - scored.len();

    // Sort by the requested key (#9), then dedup overlapping same-document
    // windows over the FULL candidate set, then truncate — so dropping a
    // duplicate does not shrink the result page below `limit`. Dedup keeps the
    // first-seen (best-ranked) chunk's bytes, so it assumes a quality-descending
    // sort; every current `SortBy` variant is "higher = better". A future
    // recency/ascending sort would need to dedup before sorting instead. When
    // the rerank path already deduped pre-Voyage, reuse its stats and skip a
    // second pass.
    sort_candidates(&mut scored, req.sort_by);
    let dedup_stats = match rerank_meta.dedup {
        // The rerank path already deduped pre-Voyage; reuse those stats.
        Some(early) => early,
        None if dedup_enabled() => {
            let (deduped, overlap_stats) = mnm_retrieval::dedup::trim_overlaps(scored);
            scored = deduped;
            overlap_stats
        }
        None => mnm_retrieval::dedup::DedupStats::default(),
    };
    scored.truncate(limit as usize);

    let results: Vec<SearchResult> = scored
        .into_iter()
        .map(|c| c.into_result(req.include_scores))
        .collect();

    // Topic is a placeholder until the Task 11 tagger lands; only the success
    // path is instrumented here (error-path metrics are out of scope).
    observability::record_search_metrics(
        "ok",
        started.elapsed().as_secs_f64() * 1000.0,
        "unknown",
        !matches!(code_mode, CodeMode::Off),
    );

    Json(SearchResponse {
        results,
        search_metadata: SearchMetadata {
            per_query,
            total_candidates,
            deduplicated_count,
            overlap_dropped_count: dedup_stats.dropped,
            overlap_trimmed_count: dedup_stats.trimmed,
            filtered_by_confidence,
            sort_by: req.sort_by,
            code_mode,
            version_match: req.version_match,
            rerank: rerank_meta.meta,
        },
    })
    .into_response()
}

/// Everything the rerank stage produced: the metadata for the response plus
/// (for the rerank path) the dedup stats already accounted.
struct RerankOutcome {
    meta: RerankMetadata,
    /// `Some` when the rerank path ran dedup early (so the main flow must skip
    /// its own dedup pass); `None` on the not-requested path.
    dedup: Option<mnm_retrieval::dedup::DedupStats>,
}

impl RerankOutcome {
    const fn not_requested() -> Self {
        Self {
            meta: RerankMetadata {
                applied: false,
                model: None,
                reason: Some("not_requested"),
            },
            dedup: None,
        }
    }
    const fn with_dedup(mut self, stats: mnm_retrieval::dedup::DedupStats) -> Self {
        self.dedup = Some(stats);
        self
    }
}

/// Run the Voyage rerank over the pooled candidates, charging billed-equivalent
/// tokens to the caller's windows + the global cap. Mutates `scored` in place
/// (relevance, confidence, factors, rerank_score). Every failure path degrades
/// to RRF order with a `reason` — a flaky upstream or an empty budget never
/// fails the search (spec D3).
#[allow(clippy::too_many_arguments)]
async fn rerank_stage(
    state: &AppState,
    req: &SearchRequest,
    queries: &[QueryPair],
    headers: &axum::http::HeaderMap,
    auth: Option<&crate::middleware::bearer::AuthContext>,
    peer: Option<std::net::IpAddr>,
    model: &'static str,
    param: mnm_core::rerank::RerankParam,
    scored: &mut [ScoredCandidate],
    rid: &str,
) -> RerankOutcome {
    let degraded = |reason: &'static str| RerankOutcome {
        meta: RerankMetadata {
            applied: false,
            model: Some(model),
            reason: Some(reason),
        },
        dedup: None,
    };

    // Kill switch / no platform key -> disabled.
    if !state.cfg.server_rerank_enabled {
        return degraded("disabled");
    }
    let Some(key) = state.cfg.voyage_api_key.as_deref() else {
        return degraded("disabled");
    };
    if scored.is_empty() {
        // Nothing to rerank; report applied (a no-op rerank is not a failure).
        return RerankOutcome {
            meta: RerankMetadata {
                applied: true,
                model: Some(model),
                reason: None,
            },
            dedup: None,
        };
    }

    // Compose the rerank query: first query text + (agent instruction, else
    // derived default per spec §3). The derived default is only materialized
    // when the agent supplied none.
    let pivot = queries.first().map(|q| q.text.as_str()).unwrap_or_default();
    let agent_instruction = req.rerank_instructions.as_deref();
    let derived: Option<String> = if agent_instruction.is_some() {
        None
    } else {
        let code_exclusive = matches!(req.code_mode, Some(CodeMode::Exclusive));
        let version = req
            .filters
            .language_target
            .any_of
            .iter()
            .find(|lt| lt.version_satisfies.is_some())
            .and_then(|lt| {
                lt.version_satisfies
                    .as_deref()
                    .map(|v| (lt.name.as_str(), v))
            });
        mnm_core::rerank::default_instruction(code_exclusive, version)
    };
    let instruction = agent_instruction.or(derived.as_deref());
    let composed = mnm_core::rerank::compose_rerank_query(pivot, instruction);
    let docs: Vec<String> = scored.iter().map(|c| c.content.clone()).collect();

    // Gate-then-charge against the shared Voyage token budget (spec §2).
    let client_ip = crate::middleware::rate_limit::client_ip(
        headers,
        &state.cfg.rate_limit_client_ip_header,
        peer,
    );
    let (subject, _tier, limits) = state.token_limiter.resolve(&client_ip, auth);
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let estimate = param.billed_tokens(rerank_token_estimate(&composed, &docs));
    let Ok(reservation) = state
        .token_limiter
        .reserve(&subject, limits, estimate, now, false)
    else {
        return degraded("token_budget_exhausted");
    };

    // Call Voyage. Errors release the reservation and degrade.
    let mut reranker = mnm_embedding::voyage::VoyageReranker::new(key, model);
    if let Some(base) = state.cfg.voyage_base_url.as_deref() {
        reranker = reranker.with_base_url(base);
    }
    let out = match reranker.rerank(composed, docs, None).await {
        Ok(o) => o,
        Err(e) => {
            state.token_limiter.release(&subject, reservation, false);
            tracing::warn!(request_id = rid, error = %e, "voyage rerank failed; degrading");
            return degraded("provider_error");
        }
    };
    let billed = param.billed_tokens(out.total_tokens);
    state
        .token_limiter
        .settle(&subject, reservation, billed, now, false);

    // Rescore in place: Voyage relevance_score is already in [0, 1] — used
    // directly, no sigmoid. Indices refer into the pool (== `scored`) order.
    for s in &out.results {
        let Some(c) = scored.get_mut(s.index) else {
            continue;
        };
        let relevance = f64::from(s.score).clamp(0.0, 1.0);
        c.relevance = relevance;
        c.rerank_score = Some(f64::from(s.score));
        c.score.confidence = state
            .scoring_policy
            .confidence(c.score.trust_score, relevance);
        c.score.factors.relevance_source = RelevanceSource::Rerank;
        c.score.factors.relevance_multiplier = relevance;
    }
    RerankOutcome {
        meta: RerankMetadata {
            applied: true,
            model: Some(model),
            reason: None,
        },
        dedup: None,
    }
}

/// A fused candidate with its computed scores, awaiting filter/sort/truncate.
struct ScoredCandidate {
    chunk_id: Uuid,
    content: String,
    document_id: Uuid,
    source_version_id: Uuid,
    chunk_index: i32,
    total_chunks: i32,
    start_byte: i32,
    end_byte: i32,
    created_at: OffsetDateTime,
    rrf_score: f64,
    vector_similarity: f64,
    matched_queries: Vec<usize>,
    /// Normalized RRF relevance term (used when `sort_by = relevance`).
    relevance: f64,
    score: ScoreResult,
    /// Voyage relevance score in [0, 1]; `Some` only after the server reranked.
    rerank_score: Option<f64>,
    source_slug: String,
    source_display_name: String,
    source_path: String,
    published_url: Option<String>,
    source_url: Option<String>,
    heading_path: Vec<String>,
    symbol_path: Vec<String>,
}

impl ScoredCandidate {
    /// Convert into the wire `SearchResult`, attaching the `scores` object only
    /// when `include_scores` is set.
    fn into_result(self, include_scores: bool) -> SearchResult {
        let scores = if include_scores {
            Some(ScoreBreakdown {
                rrf_score: self.rrf_score,
                vector_similarity: self.vector_similarity,
                matched_queries: self.matched_queries,
                trust_score: self.score.trust_score,
                confidence: self.score.confidence,
                confidence_factors: self.score.factors,
                rerank_score: self.rerank_score,
            })
        } else {
            None
        };
        SearchResult {
            chunk_id: self.chunk_id,
            content: self.content,
            document_id: self.document_id,
            source_version_id: self.source_version_id,
            chunk_index: self.chunk_index,
            total_chunks: self.total_chunks,
            created_at: self.created_at,
            source_slug: self.source_slug,
            source_display_name: self.source_display_name,
            source_path: self.source_path,
            published_url: self.published_url,
            source_url: self.source_url,
            heading_path: self.heading_path,
            symbol_path: self.symbol_path,
            scores,
        }
    }
}

impl mnm_retrieval::dedup::OverlapItem for ScoredCandidate {
    type Key = Uuid;
    fn document_key(&self) -> Uuid {
        self.document_id
    }
    fn byte_range(&self) -> (usize, usize) {
        let s = usize::try_from(self.start_byte).unwrap_or(0);
        let e = usize::try_from(self.end_byte).unwrap_or(0);
        (s, e.max(s))
    }
    fn content(&self) -> &str {
        &self.content
    }
    fn set_content(&mut self, content: String) {
        self.content = content;
    }
}

/// Sort candidates in place by the requested key, descending. `total_cmp`
/// gives a strict total order even with NaN; the sort is stable so equal keys
/// keep their fused (RRF) order.
fn sort_candidates(scored: &mut [ScoredCandidate], sort_by: SortBy) {
    scored.sort_by(|a, b| {
        let (ka, kb) = match sort_by {
            SortBy::Confidence => (a.score.confidence, b.score.confidence),
            SortBy::Trust => (a.score.trust_score, b.score.trust_score),
            SortBy::Relevance => (a.relevance, b.relevance),
            SortBy::Score => (a.rrf_score, b.rrf_score),
        };
        kb.total_cmp(&ka)
    });
}

/// Age of a document in whole days: from `source_modified_at` when present,
/// else the source-version `ingested_at` (the policy's freshness fallback).
fn age_in_days(
    now: OffsetDateTime,
    source_modified_at: Option<OffsetDateTime>,
    ingested_at: OffsetDateTime,
) -> i64 {
    let ts = source_modified_at.unwrap_or(ingested_at);
    (now - ts).whole_days()
}

/// A chunk row joined with the provenance + freshness it needs for scoring.
struct ScoringRow {
    content: String,
    document_id: Uuid,
    source_version_id: Uuid,
    chunk_index: i32,
    total_chunks: i32,
    start_byte: i32,
    end_byte: i32,
    created_at: OffsetDateTime,
    provenance: Provenance,
    source_modified_at: Option<OffsetDateTime>,
    ingested_at: OffsetDateTime,
    source_slug: String,
    source_display_name: String,
    source_path: String,
    published_url: Option<String>,
    source_url: Option<String>,
    heading_path: Vec<String>,
    symbol_path: Vec<String>,
}

/// Batch-fetch the scoring rows for a set of chunk ids, keyed by chunk id.
/// Invalid/forward-incompatible provenance JSON degrades to the default
/// (lowest-trust) `Provenance` rather than dropping the row.
///
/// # Errors
///
/// Propagates any `sqlx` error from the query or column decode.
async fn fetch_scoring_rows(
    pool: &sqlx::PgPool,
    ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, ScoringRow>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows = sqlx::query(
        "SELECT chunk.id, chunk.document_id, chunk.source_version_id, chunk.chunk_index, \
                chunk.total_chunks, chunk.start_byte, chunk.end_byte, chunk.content, chunk.created_at, \
                chunk.heading_path AS heading_path, chunk.symbol_path AS symbol_path, \
                d.provenance AS provenance, d.source_modified_at AS source_modified_at, \
                d.source_path AS source_path, d.published_url AS published_url, d.source_url AS source_url, \
                s.slug AS source_slug, s.display_name AS source_display_name, \
                sv.ingested_at AS ingested_at \
         FROM chunk \
         JOIN document d ON d.id = chunk.document_id \
         JOIN source_version sv ON sv.id = chunk.source_version_id \
         JOIN source s ON s.id = sv.source_id \
         WHERE chunk.id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;

    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for r in &rows {
        let id: Uuid = r.try_get("id")?;
        let provenance_json: serde_json::Value = r.try_get("provenance")?;
        let provenance: Provenance = serde_json::from_value(provenance_json).unwrap_or_default();
        // symbol_path is JSONB [{kind,name}] (unlike heading_path's native text[]); the wire breadcrumb only needs the name strings, so kind is intentionally dropped.
        let symbol_json: serde_json::Value =
            r.try_get("symbol_path").unwrap_or(serde_json::Value::Null);
        let symbol_path: Vec<String> = symbol_json
            .as_array()
            .map(|segs| {
                segs.iter()
                    .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        map.insert(
            id,
            ScoringRow {
                content: r.try_get("content")?,
                document_id: r.try_get("document_id")?,
                source_version_id: r.try_get("source_version_id")?,
                chunk_index: r.try_get("chunk_index")?,
                total_chunks: r.try_get("total_chunks")?,
                start_byte: r.try_get("start_byte")?,
                end_byte: r.try_get("end_byte")?,
                created_at: r.try_get("created_at")?,
                provenance,
                source_modified_at: r.try_get("source_modified_at")?,
                ingested_at: r.try_get("ingested_at")?,
                source_slug: r.try_get("source_slug")?,
                source_display_name: r.try_get("source_display_name")?,
                source_path: r.try_get("source_path")?,
                published_url: r.try_get("published_url")?,
                source_url: r.try_get("source_url")?,
                heading_path: r.try_get("heading_path")?,
                symbol_path,
            },
        );
    }
    Ok(map)
}

/// Resolve the effective query list from either the canonical `queries` array
/// or the single-query convenience form `{query, vector, code_vector}`
/// (acceptance #6).
///
/// The two forms are mutually exclusive. Returns `Err((message, remediation))`
/// for an ambiguous (both forms) or incomplete (only one of `query`/`vector`)
/// request. The general `vector` requirement is keyed on the GENERAL list:
/// fts mode skips embedding and exclusive `code_mode` replaces the general
/// list, so both accept a vector-less single query. An entirely empty request
/// yields `Ok(vec![])` so the caller's empty-queries guard produces the
/// canonical error.
fn normalize_queries(
    req: &SearchRequest,
    code_mode: CodeMode,
) -> Result<Vec<QueryPair>, (String, String)> {
    let has_convenience = req.query.is_some() || req.vector.is_some();
    if !req.queries.is_empty() {
        if has_convenience {
            return Err((
                "provide either `queries` or the single-query `{query, vector}` form, not both"
                    .to_owned(),
                "drop `query`/`vector` when using `queries`, or send a single `{query, vector}`"
                    .to_owned(),
            ));
        }
        return Ok(req.queries.clone());
    }
    match (req.query.as_ref(), req.vector.as_ref()) {
        (Some(text), Some(vector)) => Ok(vec![QueryPair {
            text: text.clone(),
            vector: vector.clone(),
            code_vector: req.code_vector.clone().unwrap_or_default(),
        }]),
        // fts mode skips embedding, and exclusive code mode replaces the
        // general vector list, so a general-vector-less single query is valid.
        (Some(text), None)
            if req.mode == SearchMode::Fts || code_mode == CodeMode::Exclusive =>
        {
            Ok(vec![QueryPair {
                text: text.clone(),
                vector: Vec::new(),
                code_vector: req.code_vector.clone().unwrap_or_default(),
            }])
        }
        (Some(_), None) | (None, Some(_)) => Err((
            "the single-query form requires both `query` and `vector`".to_owned(),
            "include the embedding `vector` (dimension must match the corpus model) alongside `query`, or use the `queries` array".to_owned(),
        )),
        (None, None) => Ok(Vec::new()),
    }
}

/// Stable content hash of a query pair (`text` + the raw bits of each vector
/// component, general then code) used to detect duplicate queries for EC-90
/// dedup. The vectors are length-delimited so `{vector: [a], code_vector: []}`
/// never collides with `{vector: [], code_vector: [a]}`.
fn query_hash(q: &QueryPair) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    q.text.hash(&mut h);
    q.vector.len().hash(&mut h);
    for f in &q.vector {
        f.to_bits().hash(&mut h);
    }
    q.code_vector.len().hash(&mut h);
    for f in &q.code_vector {
        f.to_bits().hash(&mut h);
    }
    h.finish()
}

/// Run the pgvector half of retrieval for one query.
///
/// Returns up to 100 `(chunk_id, similarity)` pairs ordered by descending
/// cosine similarity over active, ready chunks with a non-null embedding, after
/// applying the SQL-expressible filter dimensions (#11).
///
/// # Errors
///
/// Propagates any `sqlx` error from the query.
async fn vector_search(
    pool: &sqlx::PgPool,
    vector: &[f32],
    filters: &SearchFilters,
    mode: mnm_retrieval::filters::VersionMatchMode,
    corpus_model_id: Uuid,
) -> Result<Vec<(Uuid, f64)>, sqlx::Error> {
    // pgvector cosine distance: 0 = identical, 2 = opposite. Top 100 per query.
    let mut qb: QueryBuilder<Postgres> =
        QueryBuilder::new("SELECT chunk.id, 1 - (chunk.embedding <=> ");
    qb.push_bind(Vector::from(vector.to_vec()));
    qb.push(") AS similarity FROM chunk JOIN source_version sv ON sv.id = chunk.source_version_id");
    push_filter_joins(&mut qb, filters, mode);
    qb.push(
        " WHERE chunk.embedding IS NOT NULL AND chunk.status = 'ready' AND sv.is_active = true",
    );
    // Restrict to chunks encoded with the corpus model so off-model rows
    // (e.g. a mid-migration source_version on a different embedder) never leak
    // into candidates — their vectors aren't comparable to the query's.
    qb.push(" AND sv.embedding_model_id = ");
    qb.push_bind(corpus_model_id);
    push_filter_predicates(&mut qb, filters, mode);
    qb.push(" ORDER BY chunk.embedding <=> ");
    qb.push_bind(Vector::from(vector.to_vec()));
    qb.push(" LIMIT 100");

    let rows = qb.build().fetch_all(pool).await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let id: Uuid = r.try_get("id").ok()?;
            let sim: f64 = r.try_get("similarity").ok()?;
            Some((id, sim))
        })
        .collect())
}

/// Run the code-vector half (voyage-code-3 over the partial HNSW). Restricted
/// to chunks whose source_version declares the active code model — opt-out
/// versions and NULL code_embeddings can never appear in this list.
///
/// # Errors
///
/// Propagates any `sqlx` error from the query.
async fn code_vector_search(
    pool: &sqlx::PgPool,
    vector: &[f32],
    filters: &SearchFilters,
    mode: mnm_retrieval::filters::VersionMatchMode,
    code_model_id: Uuid,
) -> Result<Vec<(Uuid, f64)>, sqlx::Error> {
    let mut qb: QueryBuilder<Postgres> =
        QueryBuilder::new("SELECT chunk.id, 1 - (chunk.code_embedding <=> ");
    qb.push_bind(Vector::from(vector.to_vec()));
    qb.push(") AS similarity FROM chunk JOIN source_version sv ON sv.id = chunk.source_version_id");
    push_filter_joins(&mut qb, filters, mode);
    qb.push(
        " WHERE chunk.code_embedding IS NOT NULL AND chunk.status = 'ready' AND sv.is_active = true",
    );
    qb.push(" AND sv.code_embedding_model_id = ");
    qb.push_bind(code_model_id);
    push_filter_predicates(&mut qb, filters, mode);
    qb.push(" ORDER BY chunk.code_embedding <=> ");
    qb.push_bind(Vector::from(vector.to_vec()));
    qb.push(" LIMIT 100");

    let rows = qb.build().fetch_all(pool).await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let id: Uuid = r.try_get("id").ok()?;
            let sim: f64 = r.try_get("similarity").ok()?;
            Some((id, sim))
        })
        .collect())
}

/// Run the full-text-search half of retrieval for one query.
///
/// Returns up to 100 chunk ids ordered by descending `ts_rank` over active,
/// ready chunks, after applying the SQL-expressible filter dimensions (#11).
/// Empty or stopword-only text yields no rows, since `websearch_to_tsquery`
/// produces a query that matches nothing.
///
/// # Errors
///
/// Propagates any `sqlx` error from the query.
async fn fts_search(
    pool: &sqlx::PgPool,
    text: &str,
    filters: &SearchFilters,
    mode: mnm_retrieval::filters::VersionMatchMode,
    corpus_model_id: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT chunk.id FROM chunk JOIN source_version sv ON sv.id = chunk.source_version_id",
    );
    push_filter_joins(&mut qb, filters, mode);
    qb.push(" WHERE chunk.tsvector @@ websearch_to_tsquery('english', ");
    qb.push_bind(text.to_owned());
    qb.push(") AND chunk.status = 'ready' AND sv.is_active = true");
    // Restrict to chunks encoded with the corpus model (see `vector_search`):
    // an off-model active source_version must not contribute FTS candidates.
    qb.push(" AND sv.embedding_model_id = ");
    qb.push_bind(corpus_model_id);
    push_filter_predicates(&mut qb, filters, mode);
    qb.push(" ORDER BY ts_rank(chunk.tsvector, websearch_to_tsquery('english', ");
    qb.push_bind(text.to_owned());
    qb.push(")) DESC LIMIT 100");

    let rows = qb.build().fetch_all(pool).await?;
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid, _>("id").ok())
        .collect())
}

/// Whether any filter needs the `document` table joined. `document` carries
/// provenance JSON (attribution, content_type, verified, deprecated, tags,
/// language_targets, sdk_dependencies), the `kind`/`language` columns, the
/// `package_id` FK, and `source_modified_at`.
//
// Not `const`: `SetMatch::is_empty` is deliberately a non-const method in
// `mnm-retrieval`, so this can't be a `const fn` (unlike the old all-`Vec`
// `SearchFilters`, whose `Vec::is_empty` was const).
fn needs_document_join(f: &SearchFilters, mode: mnm_retrieval::filters::VersionMatchMode) -> bool {
    use mnm_retrieval::filters::VersionMatchMode;
    !f.attribution.is_empty()
        || f.verified.is_some()
        || f.deprecated.is_some()
        || !f.content_type.is_empty()
        || !f.kind.is_empty()
        || !f.language.is_empty()
        || !f.tags.is_empty()
        || !f.package.is_empty()
        // language_target / sdk_dependency only gate in SQL under strict mode;
        // permissive fetches provenance post-RRF, so they must not force the join.
        || (mode == VersionMatchMode::Strict
            && (!f.language_target.is_empty() || !f.sdk_dependency.is_empty()))
        || f.source_modified_at.is_some()
}

/// Append the JOINs required by the active SQL filter facets. Both candidate
/// queries call this immediately after the `source_version` join so the alias
/// set (`chunk`, `sv`, `d`, `s`, `p`) is consistent.
fn push_filter_joins(
    qb: &mut QueryBuilder<'_, Postgres>,
    f: &SearchFilters,
    mode: mnm_retrieval::filters::VersionMatchMode,
) {
    if needs_document_join(f, mode) {
        qb.push(" JOIN document d ON d.id = chunk.document_id");
    }
    if !f.source_slug.is_empty() || !f.source_kind.is_empty() {
        qb.push(" JOIN source s ON s.id = sv.source_id");
    }
    if !f.package.is_empty() {
        qb.push(" LEFT JOIN package p ON p.id = d.package_id");
    }
}

/// Append the SQL-expressible filter predicates as ` AND (...)` clauses, one
/// per constrained facet. AND across facets; OR within each facet's `any_of`;
/// exclude each facet's `none_of`. Covers every v1 facet that Postgres can
/// express: the column-backed string sets (`kind`, `language`, `source_slug`,
/// `source_kind`), the provenance-backed enums (`attribution`, `content_type`),
/// the bools (`verified`, `deprecated`), the array/JSONB sets (`tags`,
/// `heading_path`, `symbol`), `package` tuples, and the temporal/numeric ranges
/// (`ingested_at`, `source_modified_at`, `token_count`). The
/// `language_target`/`sdk_dependency` *name* membership is gated to `mode ==
/// Strict`: in permissive mode those facets are a pure ranking signal
/// (classified post-RRF, spec §3.3), not a SQL hard filter. The semver
/// refinements (version constraints) can't be expressed in SQL and are applied
/// post-fetch by [`SearchFilters::version_outcomes`].
fn push_filter_predicates(
    qb: &mut QueryBuilder<'_, Postgres>,
    f: &SearchFilters,
    mode: mnm_retrieval::filters::VersionMatchMode,
) {
    // -- enum / open-set string facets backed by a column --
    push_text_set(qb, "d.kind", &f.kind);
    push_text_set(qb, "d.language", &f.language);
    push_text_set(qb, "s.slug", &f.source_slug);
    push_text_set(qb, "s.kind", &f.source_kind);
    // provenance-backed enums (default-coalesced to match the old behaviour)
    push_prov_set(qb, "attribution", "unknown", &f.attribution);
    push_prov_set(qb, "content_type", "other", &f.content_type);

    // -- bools --
    if let Some(v) = f.verified {
        qb.push(" AND COALESCE((d.provenance->>'verified')::boolean, false) = ");
        qb.push_bind(v);
    }
    if let Some(v) = f.deprecated {
        qb.push(
            " AND COALESCE((d.provenance->'deprecation'->>'is_deprecated')::boolean, false) = ",
        );
        qb.push_bind(v);
    }

    // -- tags: JSONB array overlap (any_of) / NOT overlap (none_of) --
    if !f.tags.any_of.is_empty() {
        qb.push(" AND d.provenance->'tags' ?| ");
        qb.push_bind(f.tags.any_of.clone());
    }
    if !f.tags.none_of.is_empty() {
        // COALESCE the tags array so untagged documents survive the exclusion.
        // `Provenance.tags` is `skip_serializing_if = "Vec::is_empty"`, so an
        // untagged document has NO `tags` key and `d.provenance->'tags'` is SQL
        // NULL. `NOT (NULL ?| $none_of)` is NULL under three-valued logic, which
        // would silently drop *every* untagged document from a `tags.none_of`
        // filter (#162). Default the missing key to an empty JSONB array — which
        // overlaps nothing, so `NOT (... ?| ...)` is TRUE and the row is kept.
        qb.push(" AND NOT (COALESCE(d.provenance->'tags', '[]'::jsonb) ?| ");
        qb.push_bind(f.tags.none_of.clone());
        qb.push(")");
    }

    // -- heading_path: text[] overlap --
    if !f.heading_path.any_of.is_empty() {
        qb.push(" AND chunk.heading_path && ");
        qb.push_bind(f.heading_path.any_of.clone());
    }
    if !f.heading_path.none_of.is_empty() {
        qb.push(" AND NOT (chunk.heading_path && ");
        qb.push_bind(f.heading_path.none_of.clone());
        qb.push(")");
    }

    // -- symbol: JSONB containment per element (OR within any_of) --
    push_symbol(qb, &f.symbol);

    // -- package: (kind,name) tuples (OR within any_of) --
    push_package(qb, &f.package);

    // -- language_target / sdk_dependency: name membership in SQL (strict only;
    //    permissive is a pure ranking signal, spec §3.3) --
    if mode == mnm_retrieval::filters::VersionMatchMode::Strict {
        push_language_target_names(qb, &f.language_target);
        push_sdk_dependency_names(qb, &f.sdk_dependency);
    }

    // -- ranges --
    if let Some(r) = &f.ingested_at {
        if let Some(a) = r.after {
            qb.push(" AND sv.ingested_at >= ");
            qb.push_bind(date_to_dt(a));
        }
        if let Some(b) = r.before {
            qb.push(" AND sv.ingested_at <= ");
            qb.push_bind(date_to_dt(b));
        }
    }
    if let Some(r) = &f.source_modified_at {
        if let Some(a) = r.after {
            qb.push(" AND d.source_modified_at >= ");
            qb.push_bind(date_to_dt(a));
        }
        if let Some(b) = r.before {
            qb.push(" AND d.source_modified_at <= ");
            qb.push_bind(date_to_dt(b));
        }
    }
    if let Some(r) = &f.token_count {
        // Saturating to i32::MAX is intentional: chunk token counts fit
        // comfortably in i32, and validation already rejects `min > max`.
        if let Some(min) = r.min {
            qb.push(" AND chunk.token_count >= ");
            qb.push_bind(i32::try_from(min).unwrap_or(i32::MAX));
        }
        if let Some(max) = r.max {
            qb.push(" AND chunk.token_count <= ");
            qb.push_bind(i32::try_from(max).unwrap_or(i32::MAX));
        }
    }
}

/// Emit ` AND {col} = ANY(any_of)` / ` AND {col} <> ALL(none_of)` for a string
/// set facet backed directly by a column.
fn push_text_set(
    qb: &mut QueryBuilder<'_, Postgres>,
    col: &str,
    set: &mnm_retrieval::filters::SetMatch<String>,
) {
    if !set.any_of.is_empty() {
        qb.push(format!(" AND {col} = ANY("));
        qb.push_bind(set.any_of.clone());
        qb.push(")");
    }
    if !set.none_of.is_empty() {
        // COALESCE the column so a NULL value survives the exclusion. Without it,
        // `NULL <> ALL($none_of)` evaluates to NULL under SQL three-valued logic
        // and Postgres drops the row — so an "exclude TypeScript" filter over the
        // nullable `document.language` column would silently drop *every*
        // NULL-language document (all prose/markdown), not just TypeScript ones
        // (#162). The `''` sentinel can never appear in `none_of` for a real
        // value, so coalesced-NULL rows always pass. Mirrors `push_prov_set`.
        qb.push(format!(" AND COALESCE({col}, '') <> ALL("));
        qb.push_bind(set.none_of.clone());
        qb.push(")");
    }
}

/// Like [`push_text_set`] but reads the value out of `document.provenance`,
/// coalescing absent values to `default` so missing provenance keys still match
/// the historical "treat absent as the catch-all bucket" behaviour.
fn push_prov_set(
    qb: &mut QueryBuilder<'_, Postgres>,
    key: &str,
    default: &str,
    set: &mnm_retrieval::filters::SetMatch<String>,
) {
    if !set.any_of.is_empty() {
        qb.push(format!(" AND COALESCE(d.provenance->>'{key}', '{default}') = ANY("));
        qb.push_bind(set.any_of.clone());
        qb.push(")");
    }
    if !set.none_of.is_empty() {
        qb.push(format!(" AND COALESCE(d.provenance->>'{key}', '{default}') <> ALL("));
        qb.push_bind(set.none_of.clone());
        qb.push(")");
    }
}

/// Emit JSONB-containment predicates for the `symbol` facet: OR across
/// `any_of` elements, exclude each `none_of` element.
fn push_symbol(
    qb: &mut QueryBuilder<'_, Postgres>,
    set: &mnm_retrieval::filters::SetMatch<mnm_retrieval::filters::SymbolMatch>,
) {
    if !set.any_of.is_empty() {
        qb.push(" AND (");
        for (i, s) in set.any_of.iter().enumerate() {
            if i > 0 {
                qb.push(" OR ");
            }
            qb.push("chunk.symbol_path @> ");
            qb.push_bind(symbol_json(s));
        }
        qb.push(")");
    }
    for s in &set.none_of {
        qb.push(" AND NOT (chunk.symbol_path @> ");
        qb.push_bind(symbol_json(s));
        qb.push(")");
    }
}

/// Build a one-element JSONB containment doc, e.g. `[{"kind":"circuit"}]`.
fn symbol_json(s: &mnm_retrieval::filters::SymbolMatch) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    if let Some(k) = &s.kind {
        obj.insert("kind".into(), serde_json::Value::String(k.clone()));
    }
    if let Some(n) = &s.name {
        obj.insert("name".into(), serde_json::Value::String(n.clone()));
    }
    serde_json::Value::Array(vec![serde_json::Value::Object(obj)])
}

/// Emit `(p.kind = .. AND p.name = ..)` tuple predicates for the `package`
/// facet: OR across `any_of` elements, exclude each `none_of` element.
fn push_package(
    qb: &mut QueryBuilder<'_, Postgres>,
    set: &mnm_retrieval::filters::SetMatch<mnm_retrieval::filters::PackageMatch>,
) {
    if !set.any_of.is_empty() {
        qb.push(" AND (");
        for (i, p) in set.any_of.iter().enumerate() {
            if i > 0 {
                qb.push(" OR ");
            }
            qb.push("(p.kind = ");
            qb.push_bind(p.kind.clone());
            qb.push(" AND p.name = ");
            qb.push_bind(p.name.clone());
            qb.push(")");
        }
        qb.push(")");
    }
    for p in &set.none_of {
        // `p.*` is NULL for documents with a NULL `package_id` (the LEFT JOIN
        // found no package). `NOT (NULL = $ AND NULL = $)` is NULL under SQL
        // three-valued logic, so Postgres would silently drop *every* packageless
        // document — all non-code content — from a `none_of` filter (#162). A row
        // with no package can never match an excluded (kind, name), so guard the
        // exclusion with `p.id IS NULL OR ...` to retain those rows; only rows
        // that actually have a package are tested against the exclusion.
        qb.push(" AND (p.id IS NULL OR NOT (p.kind = ");
        qb.push_bind(p.kind.clone());
        qb.push(" AND p.name = ");
        qb.push_bind(p.name.clone());
        qb.push("))");
    }
}

/// Emit a JSONB `EXISTS` over `provenance.language_targets` matching any
/// requested `name`. The semver refinement is applied post-fetch.
fn push_language_target_names(
    qb: &mut QueryBuilder<'_, Postgres>,
    set: &mnm_retrieval::filters::SetMatch<mnm_retrieval::filters::LanguageTargetMatch>,
) {
    if set.any_of.is_empty() {
        return;
    }
    let names: Vec<String> = set.any_of.iter().map(|t| t.name.clone()).collect();
    qb.push(" AND EXISTS (SELECT 1 FROM jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt WHERE lt->>'name' = ANY(");
    qb.push_bind(names);
    qb.push("))");
}

/// Emit a JSONB `EXISTS` over `provenance.sdk_dependencies` matching any
/// requested `name`. The semver refinement is applied post-fetch.
fn push_sdk_dependency_names(
    qb: &mut QueryBuilder<'_, Postgres>,
    set: &mnm_retrieval::filters::SetMatch<mnm_retrieval::filters::SdkDependencyMatch>,
) {
    if set.any_of.is_empty() {
        return;
    }
    let names: Vec<String> = set.any_of.iter().map(|d| d.name.clone()).collect();
    qb.push(" AND EXISTS (SELECT 1 FROM jsonb_array_elements(COALESCE(d.provenance->'sdk_dependencies','[]'::jsonb)) dep WHERE dep->>'name' = ANY(");
    qb.push_bind(names);
    qb.push("))");
}

/// Lift an ISO `Date` to a UTC midnight `OffsetDateTime` for binding against a
/// `timestamptz` column.
fn date_to_dt(d: time::Date) -> time::OffsetDateTime {
    d.with_hms(0, 0, 0)
        .expect("00:00:00 is always a valid time")
        .assume_utc()
}

/// What the version facets decided for one candidate.
enum VersionDecision {
    /// Candidate is removed (strict non-satisfies, or permissive Breaking).
    Drop,
    /// Candidate is scored with this input (`None` = no version filter).
    Score(Option<mnm_core::scoring::VersionScoreInput>),
}

/// Combine per-facet outcomes into a drop/score decision (spec §3.2/§3.3).
/// Combined multiplier = min across constrained facets (worst offender).
fn version_decision(
    filters: &SearchFilters,
    outcomes: mnm_retrieval::filters::VersionOutcomes,
    mode: mnm_retrieval::filters::VersionMatchMode,
    policy: &mnm_core::scoring_policy::ScoringPolicy,
) -> VersionDecision {
    use mnm_core::scoring::{LanguageTargetQueryFactor, VersionScoreInput};
    use mnm_core::version_match::MatchClass;
    use mnm_retrieval::filters::{FacetVersionOutcome, VersionMatchMode};

    let constrained: Vec<FacetVersionOutcome> = [outcomes.language_target, outcomes.sdk_dependency]
        .into_iter()
        .flatten()
        .collect();
    if constrained.is_empty() {
        return VersionDecision::Score(None);
    }
    match mode {
        VersionMatchMode::Strict => {
            // Anything not Satisfies (incl. Silent/Unknown) drops — unchanged
            // hard-filter semantics.
            let all_satisfy = constrained.iter().all(|o| {
                matches!(
                    o,
                    FacetVersionOutcome::Classified {
                        class: MatchClass::Satisfies,
                        ..
                    }
                )
            });
            if !all_satisfy {
                return VersionDecision::Drop;
            }
        }
        VersionMatchMode::Permissive => {
            if constrained.iter().any(|o| {
                matches!(
                    o,
                    FacetVersionOutcome::Classified {
                        class: MatchClass::Breaking,
                        ..
                    }
                )
            }) {
                return VersionDecision::Drop;
            }
        }
    }
    // Per-facet multiplier; Silent → neutral. Track the worst (min) facet.
    let facet_eval = |o: &FacetVersionOutcome| -> (f64, &'static str, Option<u32>) {
        match o {
            FacetVersionOutcome::Silent => (policy.version_match.neutral, "silent", None),
            FacetVersionOutcome::Classified { class, .. } => {
                let m = policy.version_multiplier(class);
                let (label, dist) = match class {
                    MatchClass::Satisfies => ("satisfies", None),
                    MatchClass::NearMissPatch(d) | MatchClass::NearMissMinor(d) => {
                        ("near_miss", Some(*d))
                    }
                    MatchClass::Unknown => ("unknown", None),
                    MatchClass::Breaking => ("near_miss", None), // dropped above; unreachable
                };
                (m, label, dist)
            }
        }
    };
    let (multiplier, class, distance) = constrained
        .iter()
        .map(facet_eval)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .expect("constrained is non-empty");
    // Echo the language element that won (when the language facet is constrained).
    let query = match outcomes.language_target {
        Some(FacetVersionOutcome::Classified { element, .. }) => filters
            .language_target
            .any_of
            .get(element)
            .map(|lt| LanguageTargetQueryFactor {
                name: lt.name.clone(),
                version_constraint_satisfies: lt.version_satisfies.clone(),
            }),
        _ => filters
            .language_target
            .any_of
            .first()
            .map(|lt| LanguageTargetQueryFactor {
                name: lt.name.clone(),
                version_constraint_satisfies: lt.version_satisfies.clone(),
            }),
    };
    VersionDecision::Score(Some(VersionScoreInput {
        multiplier,
        class,
        distance,
        query,
    }))
}

#[cfg(test)]
mod code_mode_tests {
    use super::{effective_code_mode, CodeMode, SearchMode};

    #[test]
    fn matrix() {
        use CodeMode::{Exclusive, Off, On};
        use SearchMode::{Fts, Hybrid, Vector};
        assert_eq!(effective_code_mode(Hybrid, None), Ok(On));
        assert_eq!(effective_code_mode(Vector, None), Ok(On));
        assert_eq!(effective_code_mode(Fts, None), Ok(Off));
        assert_eq!(effective_code_mode(Hybrid, Some(Off)), Ok(Off));
        assert_eq!(effective_code_mode(Vector, Some(Exclusive)), Ok(Exclusive));
        assert_eq!(effective_code_mode(Fts, Some(Off)), Ok(Off));
        assert_eq!(effective_code_mode(Fts, Some(On)), Err(()));
        assert_eq!(effective_code_mode(Fts, Some(Exclusive)), Err(()));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        min_confidence_valid, normalize_queries, CodeMode, QueryPair, ScoredCandidate, SearchMode,
        SearchRequest, SortBy,
    };
    use mnm_core::provenance::Attribution;
    use mnm_core::scoring::{ConfidenceFactors, RelevanceSource, ScoreResult};
    use mnm_retrieval::filters::{NumericRange, SearchFilters, SetMatch};
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn req(
        queries: Vec<QueryPair>,
        query: Option<String>,
        vector: Option<Vec<f32>>,
    ) -> SearchRequest {
        SearchRequest {
            queries,
            query,
            vector,
            client_embedding_model: Some("bge-base-en-v1.5@1".to_owned()),
            limit: 20,
            filters: SearchFilters::default(),
            sort_by: SortBy::default(),
            mode: SearchMode::default(),
            min_confidence: 0.0,
            include_scores: true,
            code_mode: None,
            client_code_embedding_model: None,
            code_vector: None,
            rerank: None,
            rerank_instructions: None,
            version_match: mnm_retrieval::filters::VersionMatchMode::default(),
        }
    }

    #[test]
    fn convenience_form_normalizes_identically_to_canonical() {
        let v = vec![0.5_f32; 768];
        let convenience = req(Vec::new(), Some("hello world".to_owned()), Some(v.clone()));
        let canonical = req(
            vec![QueryPair {
                text: "hello world".to_owned(),
                vector: v,
                code_vector: Vec::new(),
            }],
            None,
            None,
        );
        // The convenience form is pure sugar: both shapes resolve to the exact
        // same internal query list, so downstream processing is byte-identical.
        assert_eq!(
            normalize_queries(&convenience, CodeMode::On).unwrap(),
            normalize_queries(&canonical, CodeMode::On).unwrap()
        );
    }

    #[test]
    fn rejects_both_forms_at_once() {
        let v = vec![0.5_f32; 768];
        let both = req(
            vec![QueryPair {
                text: "a".to_owned(),
                vector: v.clone(),
                code_vector: Vec::new(),
            }],
            Some("b".to_owned()),
            Some(v),
        );
        assert!(normalize_queries(&both, CodeMode::On).is_err());
    }

    #[test]
    fn rejects_incomplete_convenience_form() {
        let only_query = req(Vec::new(), Some("a".to_owned()), None);
        let only_vector = req(Vec::new(), None, Some(vec![0.5_f32; 768]));
        assert!(normalize_queries(&only_query, CodeMode::On).is_err());
        assert!(normalize_queries(&only_vector, CodeMode::On).is_err());
    }

    #[test]
    fn empty_request_yields_empty_list() {
        assert!(normalize_queries(&req(Vec::new(), None, None), CodeMode::On)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn min_confidence_accepts_the_closed_unit_interval() {
        // Boundaries and interior are valid floors.
        assert!(min_confidence_valid(0.0));
        assert!(min_confidence_valid(1.0));
        assert!(min_confidence_valid(0.5));
        // The historic default (serde `#[serde(default)]` f64) stays valid.
        assert!(min_confidence_valid(f64::default()));
    }

    #[test]
    fn min_confidence_rejects_out_of_range_values() {
        // The prior code silently clamped these to 1.0 and returned an empty
        // page; now they are a 400 (#165). NaN and ±inf all compare false
        // against the bounds, so `contains` rejects them too — `1e400` from the
        // wire parses to +inf and must not be accepted.
        assert!(!min_confidence_valid(5.0));
        assert!(!min_confidence_valid(1.000_001));
        assert!(!min_confidence_valid(-0.000_001));
        assert!(!min_confidence_valid(-1.0));
        assert!(!min_confidence_valid(f64::INFINITY));
        assert!(!min_confidence_valid(f64::NEG_INFINITY));
        assert!(!min_confidence_valid(f64::NAN));
    }

    #[test]
    fn fts_mode_allows_vectorless_single_query() {
        let mut r = req(Vec::new(), Some("hello".to_owned()), None);
        r.mode = SearchMode::Fts;
        let qs = normalize_queries(&r, CodeMode::Off).unwrap();
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].text, "hello");
        assert!(qs[0].vector.is_empty());

        // Non-fts mode still requires the vector for the convenience form.
        let mut r2 = req(Vec::new(), Some("hello".to_owned()), None);
        r2.mode = SearchMode::Hybrid;
        assert!(normalize_queries(&r2, CodeMode::On).is_err());
    }

    #[test]
    fn exclusive_mode_allows_general_vectorless_single_query() {
        // In exclusive code mode the general vector list is replaced by the
        // code-vector list, so the convenience form may omit `vector` as long
        // as `code_vector` carries the code-model embedding.
        let mut r = req(Vec::new(), Some("hello".to_owned()), None);
        r.code_mode = Some(CodeMode::Exclusive);
        r.code_vector = Some(vec![0.5_f32; 4]);
        let qs = normalize_queries(&r, CodeMode::Exclusive).unwrap();
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].text, "hello");
        assert!(qs[0].vector.is_empty());
        assert_eq!(qs[0].code_vector.len(), 4);

        // Outside exclusive (and fts) the general vector is still required.
        assert!(normalize_queries(&r, CodeMode::On).is_err());
    }

    #[test]
    fn code_mode_parses_and_defaults_to_none() {
        let body = serde_json::json!({
            "queries": [{ "text": "x", "vector": [0.0] }],
            "client_embedding_model": "voyage-context-3@1"
        });
        let req: SearchRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.code_mode, None);
        assert!(req.queries[0].code_vector.is_empty());

        let body2 = serde_json::json!({
            "queries": [{ "text": "x", "vector": [0.0], "code_vector": [0.1, 0.2] }],
            "client_embedding_model": "voyage-context-3@1",
            "client_code_embedding_model": "voyage-code-3@1",
            "code_mode": "exclusive"
        });
        let req2: SearchRequest = serde_json::from_value(body2).unwrap();
        assert_eq!(req2.code_mode, Some(CodeMode::Exclusive));
        assert_eq!(req2.queries[0].code_vector, vec![0.1_f32, 0.2_f32]);
    }

    #[test]
    fn mode_defaults_to_hybrid_and_parses() {
        let body = serde_json::json!({
            "queries": [{ "text": "x", "vector": [0.0] }],
            "client_embedding_model": "voyage-code-3@1"
        });
        let req: SearchRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.mode, SearchMode::Hybrid);

        let body2 = serde_json::json!({
            "query": "x", "vector": [0.0],
            "client_embedding_model": "voyage-code-3@1", "mode": "fts"
        });
        let req2: SearchRequest = serde_json::from_value(body2).unwrap();
        assert_eq!(req2.mode, SearchMode::Fts);
    }

    fn built_sql(filters: &SearchFilters) -> String {
        // Strict mode so the helper still exercises the language_target /
        // sdk_dependency name-gate path (permissive suppresses it in SQL).
        let mode = mnm_retrieval::filters::VersionMatchMode::Strict;
        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT chunk.id FROM chunk JOIN source_version sv ON sv.id = chunk.source_version_id",
        );
        super::push_filter_joins(&mut qb, filters, mode);
        qb.push(" WHERE true");
        super::push_filter_predicates(&mut qb, filters, mode);
        qb.sql().to_owned()
    }

    #[test]
    fn kind_emits_document_join_and_any_predicate() {
        let f = SearchFilters {
            kind: SetMatch {
                any_of: vec!["code".into()],
                none_of: vec![],
            },
            ..Default::default()
        };
        let sql = built_sql(&f);
        assert!(sql.contains("JOIN document d"), "kind needs the document join");
        assert!(sql.contains("d.kind = ANY("), "got: {sql}");
    }

    #[test]
    fn language_none_of_coalesces_so_null_rows_survive() {
        // `document.language` is nullable, so a bare `d.language <> ALL($none_of)`
        // is NULL (→ row dropped) for every prose/markdown document. The fix wraps
        // the column in COALESCE so NULL-language rows survive an exclusion (#162).
        let f = SearchFilters {
            language: SetMatch {
                any_of: vec![],
                none_of: vec!["typescript".into()],
            },
            ..Default::default()
        };
        let sql = built_sql(&f);
        assert!(
            sql.contains("COALESCE(d.language, '') <> ALL("),
            "language none_of must COALESCE the nullable column so NULL rows survive; got: {sql}"
        );
    }

    #[test]
    fn language_any_of_does_not_coalesce() {
        // The inclusion path (`any_of`) intentionally leaves NULL rows out — a
        // NULL language is not one of the requested values — so it must NOT be
        // coalesced. Only the exclusion path (`none_of`) needs the guard.
        let f = SearchFilters {
            language: SetMatch {
                any_of: vec!["rust".into()],
                none_of: vec![],
            },
            ..Default::default()
        };
        let sql = built_sql(&f);
        assert!(sql.contains("d.language = ANY("), "got: {sql}");
        assert!(
            !sql.contains("COALESCE(d.language"),
            "any_of must not coalesce (NULL rows should be excluded from an inclusion); got: {sql}"
        );
    }

    #[test]
    fn package_none_of_retains_null_package_rows() {
        // `document.package_id` is nullable and joined with a LEFT JOIN, so `p.*`
        // is NULL for packageless docs. A bare `NOT (p.kind = $ AND p.name = $)`
        // is NULL (→ row dropped) for every non-code document. The fix guards the
        // exclusion with `p.id IS NULL OR ...` so packageless rows survive (#162).
        use mnm_retrieval::filters::PackageMatch;
        let f = SearchFilters {
            package: SetMatch {
                any_of: vec![],
                none_of: vec![PackageMatch {
                    kind: "npm".into(),
                    name: "typescript".into(),
                }],
            },
            ..Default::default()
        };
        let sql = built_sql(&f);
        assert!(
            sql.contains("p.id IS NULL OR NOT (p.kind = "),
            "package none_of must retain NULL-package rows via `p.id IS NULL OR ...`; got: {sql}"
        );
    }

    #[test]
    fn tags_none_of_coalesces_so_untagged_rows_survive() {
        // `Provenance.tags` is `skip_serializing_if = "Vec::is_empty"`, so an
        // untagged document has no `tags` key and `d.provenance->'tags'` is NULL.
        // The fix defaults the missing key to `'[]'::jsonb` so untagged rows
        // survive a `tags.none_of` exclusion (#162).
        let f = SearchFilters {
            tags: SetMatch {
                any_of: vec![],
                none_of: vec!["deprecated".into()],
            },
            ..Default::default()
        };
        let sql = built_sql(&f);
        assert!(
            sql.contains("NOT (COALESCE(d.provenance->'tags', '[]'::jsonb) ?| "),
            "tags none_of must COALESCE the missing key so untagged rows survive; got: {sql}"
        );
    }

    #[test]
    fn tags_any_of_does_not_coalesce() {
        // The inclusion path leaves untagged (NULL-key) rows out — an untagged
        // doc has none of the requested tags — so it must NOT be coalesced.
        let f = SearchFilters {
            tags: SetMatch {
                any_of: vec!["tutorial".into()],
                none_of: vec![],
            },
            ..Default::default()
        };
        let sql = built_sql(&f);
        assert!(sql.contains("d.provenance->'tags' ?| "), "got: {sql}");
        assert!(
            !sql.contains("COALESCE(d.provenance->'tags'"),
            "any_of must not coalesce (untagged rows should be excluded from an inclusion); got: {sql}"
        );
    }

    #[test]
    fn token_count_min_emits_range() {
        let f = SearchFilters {
            token_count: Some(NumericRange { min: Some(50), max: None }),
            ..Default::default()
        };
        let sql = built_sql(&f);
        assert!(sql.contains("chunk.token_count >="), "got: {sql}");
    }

    #[test]
    fn into_result_carries_readable_identity() {
        let c = ScoredCandidate {
            chunk_id: Uuid::nil(),
            content: "x".into(),
            document_id: Uuid::nil(),
            source_version_id: Uuid::nil(),
            chunk_index: 0,
            total_chunks: 1,
            start_byte: 0,
            end_byte: 1,
            created_at: OffsetDateTime::UNIX_EPOCH,
            rrf_score: 0.0,
            vector_similarity: 0.0,
            matched_queries: vec![],
            relevance: 0.0,
            score: ScoreResult {
                trust_score: 0.0,
                confidence: 0.0,
                factors: ConfidenceFactors {
                    attribution: Attribution::Unknown,
                    attribution_multiplier: 0.0,
                    verified: false,
                    verified_by: None,
                    verification_multiplier: 1.0,
                    age_days: 0,
                    freshness_multiplier: 1.0,
                    deprecation: false,
                    deprecation_multiplier: 1.0,
                    language_target_query: None,
                    language_targets_chunk: vec![],
                    version_match_multiplier: 1.0,
                    version_match_class: None,
                    version_distance: None,
                    relevance_source: RelevanceSource::Rrf,
                    relevance_multiplier: 0.0,
                },
            },
            rerank_score: None,
            source_slug: "compact-docs".into(),
            source_display_name: "Compact Docs".into(),
            source_path: "docs/intro.md".into(),
            published_url: Some("https://x/intro".into()),
            source_url: None,
            heading_path: vec!["Compiling".into(), "Witnesses".into()],
            symbol_path: vec![],
        };
        let r = c.into_result(false);
        assert_eq!(r.source_path, "docs/intro.md");
        assert_eq!(r.source_display_name, "Compact Docs");
        assert_eq!(r.heading_path, vec!["Compiling".to_string(), "Witnesses".to_string()]);
    }

    #[test]
    fn symbol_path_name_extraction_skips_malformed() {
        let json = serde_json::json!([
            {"kind": "impl", "name": "Foo"},
            {"kind": "fn",   "name": "bar"},
            {},            // no "name"
            {"kind": "fn"} // "name" missing
        ]);
        let names: Vec<String> = json
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(str::to_owned))
            .collect();
        assert_eq!(names, vec!["Foo".to_string(), "bar".to_string()]);
    }
}

#[cfg(test)]
mod rerank_tests {
    use super::*;

    #[test]
    fn rerank_pool_is_max_of_limit_and_floor() {
        assert_eq!(rerank_pool_size(10), 50);
        assert_eq!(rerank_pool_size(50), 50);
        assert_eq!(rerank_pool_size(80), 80);
    }

    #[test]
    fn rerank_token_estimate_multiplies_query_by_docs() {
        // 8-char query ≈ 2 tokens; two 4-char docs ≈ 1 token each.
        // (2 × 2) + (1 + 1) = 6.
        let docs = vec!["aaaa".to_owned(), "bbbb".to_owned()];
        assert_eq!(rerank_token_estimate("qqqqqqqq", &docs), 6);
        // Empty docs -> 0 (no Voyage call would be made anyway).
        assert_eq!(rerank_token_estimate("qqqqqqqq", &[]), 0);
    }

    #[test]
    fn rerank_metadata_serializes_per_spec() {
        let applied = RerankMetadata {
            applied: true,
            model: Some("rerank-2.5"),
            reason: None,
        };
        let v = serde_json::to_value(&applied).unwrap();
        assert_eq!(v, serde_json::json!({"applied": true, "model": "rerank-2.5"}));

        let degraded = RerankMetadata {
            applied: false,
            model: Some("rerank-2.5-lite"),
            reason: Some("token_budget_exhausted"),
        };
        let v = serde_json::to_value(&degraded).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "applied": false, "model": "rerank-2.5-lite",
                "reason": "token_budget_exhausted"
            })
        );
    }
}

#[cfg(test)]
mod mismatch_envelope_tests {
    use super::*;
    use crate::error::ErrorBody;

    /// Both `embedding_model_mismatch` paths — the general-model guard and the
    /// code-model guard — must surface the corpus identifier under the SAME
    /// `corpus_model` context key. The MCP client's `parse_mismatch` reads only
    /// `corpus_model`/`client_model`; emitting the code path's identifier under a
    /// divergent `code_model` key (the prior bug) left `corpus_model` empty and
    /// degraded the agent-facing remediation. This mirrors the route builders at
    /// the two 409 sites exactly, then asserts the on-the-wire envelope shape.
    #[test]
    fn both_mismatch_paths_use_corpus_model_context_key() {
        // Mirrors the general-model guard (`run_general_vector` branch).
        let general = CoreError::builder(ErrorCode::EmbeddingModelMismatch)
            .message("client_embedding_model `m@1` does not match corpus model `m@2`")
            .remediation("run `mnm models active` to see the corpus's active model, then re-embed the query with it")
            .context("corpus_model", "m@2".to_owned())
            .context("client_model", "m@1".to_owned())
            .build();

        // Mirrors the code-model guard (`run_code_vector` branch).
        let code = CoreError::builder(ErrorCode::EmbeddingModelMismatch)
            .message("client_code_embedding_model `c@1` does not match code model `c@2`")
            .remediation("run `mnm models active` to see the corpus's active code-embedding model, then re-embed code queries with it")
            .context("corpus_model", "c@2".to_owned())
            .context("client_model", "c@1".to_owned())
            .build();

        for (label, err, corpus, client) in [
            ("general", general, "m@2", "m@1"),
            ("code", code, "c@2", "c@1"),
        ] {
            // Serialize the full `{ error: {...}, request_id }` envelope the MCP
            // client actually parses, not just the bare CoreError.
            let body = ErrorBody {
                error: err,
                request_id: "rid-test".to_owned(),
            };
            let v = serde_json::to_value(&body).unwrap();

            assert_eq!(v["error"]["code"], "embedding_model_mismatch", "{label}: code");
            assert_eq!(
                v["error"]["context"]["corpus_model"], corpus,
                "{label}: corpus identifier must live under `corpus_model`"
            );
            assert_eq!(
                v["error"]["context"]["client_model"], client,
                "{label}: client identifier must live under `client_model`"
            );
            // The legacy divergent key must never reappear on either path.
            assert!(
                v["error"]["context"].get("code_model").is_none(),
                "{label}: `code_model` context key must not be emitted"
            );
        }
    }

    /// The code-vector DIMENSION guard's remediation must name the same
    /// discovery command as its three sibling query-time mismatch guards
    /// (`mnm models active` prints the exact code `dim` this guard rejects on),
    /// so it can't drift back to the non-actionable "re-embed with the corpus
    /// code model" (#140). Pins the shared const the route emits — reverting the
    /// route to an inline string leaves the const dead (`-D warnings`).
    #[test]
    fn code_dimension_guard_remediation_names_models_active() {
        assert!(
            CODE_VECTOR_DIM_REMEDIATION.contains("mnm models active"),
            "the shared const must name the discovery command: {CODE_VECTOR_DIM_REMEDIATION}"
        );
        // Mirror the route builder to pin the on-the-wire envelope shape.
        let err = CoreError::builder(ErrorCode::InvalidRequest)
            .message("queries[0].code_vector has 512 dimensions; expected 256")
            .remediation(CODE_VECTOR_DIM_REMEDIATION)
            .build();
        let body = ErrorBody {
            error: err,
            request_id: "rid-test".to_owned(),
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["error"]["code"], "invalid_request");
        assert!(
            v["error"]["remediation"]
                .as_str()
                .is_some_and(|r| r.contains("mnm models active")),
            "dimension-guard remediation must name the discovery command: {v}"
        );
    }
}

//! `POST /v1/search` — hybrid FTS + pgvector retrieval.
//!
//! For each distinct query pair the handler runs both a pgvector cosine search
//! and a Postgres full-text search, then fuses every ranked list — across both
//! modes and across all query pairs — in a single Reciprocal Rank Fusion pass
//! (k=60). Reranking and confidence scoring land in later phases.

use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mn_core::error::{Error as CoreError, ErrorCode};
use mn_core::provenance::Provenance;
use mn_core::scoring::{self, ConfidenceFactors, RelevanceSource, ScoreResult, VersionQuery};
use mn_retrieval::filters::SearchFilters;
use pgvector::Vector;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, QueryBuilder, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::rate_limit::RateLimitContext;
use crate::middleware::request_id::RequestId;
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
    /// The embedding model identifier the client used to produce each
    /// `vector`. MUST match the corpus's active model identifier (D12 /
    /// FR-038); mismatch returns 409.
    pub client_embedding_model: String,
    /// Maximum number of results to return. Capped at 100 server-side.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Search filters (AND across keys, OR within each array).
    #[serde(default)]
    pub filters: SearchFilters,
    /// Result ordering key (US6 acceptance #9). Defaults to `confidence`.
    #[serde(default)]
    pub sort_by: SortBy,
    /// Drop results whose `confidence` is below this floor before applying
    /// `limit` (US6 acceptance #10). Defaults to 0.0 (no filtering).
    #[serde(default)]
    pub min_confidence: f64,
    /// When `false`, omit the per-result `scores` object from the response.
    /// Defaults to `true`.
    #[serde(default = "default_true")]
    pub include_scores: bool,
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

/// One {text, vector} pair.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct QueryPair {
    /// The query text, used for the full-text-search half of retrieval.
    pub text: String,
    /// The pre-computed embedding; its dimension must match the active corpus model.
    pub vector: Vec<f32>,
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
    /// How many candidates were dropped for falling below `min_confidence`
    /// before the limit was applied (US6 acceptance #10).
    pub filtered_by_confidence: usize,
    /// The ordering key actually applied (echoes the request, default
    /// `confidence`), so callers can confirm the resolved sort.
    pub sort_by: SortBy,
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
}

#[allow(clippy::too_many_lines)]
async fn search(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    rl: Option<Extension<RateLimitContext>>,
    Json(req): Json<SearchRequest>,
) -> Response {
    let rid = req_id.as_str();
    let rl_ctx = rl.as_ref().map(|Extension(c)| c);

    // Normalize the single-query convenience form `{query, vector}` (#6) into
    // the canonical query list. Ambiguous/incomplete requests are rejected.
    let queries = match normalize_queries(&req) {
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
    // request can never drive the FTS half of hybrid retrieval and signals a
    // malformed caller.
    if queries.iter().all(|q| q.text.trim().is_empty()) {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message("every query has empty `text`")
                .remediation("supply non-empty query text so full-text search can run")
                .build(),
            rid,
        );
    }
    let limit = req.limit.min(max_limit()).max(1);

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
    if req.client_embedding_model != cm.wire {
        return error::into_response(
            CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                .message(format!(
                    "client_embedding_model `{}` does not match corpus model `{}`",
                    req.client_embedding_model, cm.wire,
                ))
                .remediation("re-run `mnm models pull` to fetch the corpus model")
                .context("corpus_model", cm.wire.clone())
                .context("client_model", req.client_embedding_model.clone())
                .build(),
            rid,
        );
    }

    // Vector-dim guard: every query's vector must match the corpus model's dim.
    for (i, q) in queries.iter().enumerate() {
        if q.vector.len() != cm.dim {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message(format!(
                        "queries[{i}].vector has {} dimensions; expected {}",
                        q.vector.len(),
                        cm.dim,
                    ))
                    .remediation("re-embed with the corpus model (run `mnm models pull`)")
                    .build(),
                rid,
            );
        }
    }

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
    let mut ranked_lists: Vec<Vec<Uuid>> = Vec::with_capacity(distinct.len() * 2);
    // Per chunk: which distinct queries contributed at least one rank, and the
    // best vector similarity seen (for reporting; FTS-only chunks stay at 0.0).
    let mut matched: std::collections::HashMap<Uuid, std::collections::BTreeSet<usize>> =
        std::collections::HashMap::new();
    let mut best_similarity: std::collections::HashMap<Uuid, f64> =
        std::collections::HashMap::new();

    for (i, q) in distinct.iter().enumerate() {
        let t0 = std::time::Instant::now();
        let vector_hits = match vector_search(&state.pool, &q.vector, &req.filters, corpus_model_id)
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
        let vector_latency_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = std::time::Instant::now();
        let fts_hits = match fts_search(&state.pool, &q.text, &req.filters, corpus_model_id).await {
            Ok(hits) => hits,
            Err(e) => {
                tracing::warn!(request_id = rid, error = %e, query_index = i, "fts search failed");
                return error::service_unavailable(format!("fts search failed for query {i}"), rid);
            }
        };
        let fts_latency_ms = t1.elapsed().as_secs_f64() * 1000.0;

        per_query.push(PerQueryRecord {
            query_index: i,
            fts_candidates: fts_hits.len(),
            fts_latency_ms,
            vector_candidates: vector_hits.len(),
            vector_latency_ms,
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
        for id in &fts_hits {
            matched.entry(*id).or_default().insert(i);
        }

        ranked_lists.push(vector_ids);
        ranked_lists.push(fts_hits);
    }

    // Single RRF pass across all (query, mode) lists. We score the FULL fused
    // candidate set (not just the top `limit`) so confidence filtering and the
    // sort_by reorder operate over every candidate before truncation (#9/#10).
    let list_refs: Vec<&[Uuid]> = ranked_lists.iter().map(Vec::as_slice).collect();
    let fused = mn_retrieval::rrf::fuse(&list_refs);
    let total_candidates = fused.len();

    // Batch-fetch every fused candidate joined with its document (provenance +
    // freshness) and source_version (ingest timestamp).
    let fused_ids: Vec<Uuid> = fused.iter().map(|(id, _)| *id).collect();
    let rows = match fetch_scoring_rows(&state.pool, &fused_ids).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "scoring-row fetch failed");
            return error::service_unavailable("result fetch failed", rid);
        }
    };

    // Query-side version constraint (borrows from req.filters), built once.
    let version_query = req.filters.language_target.as_ref().map(|lt| VersionQuery {
        name: &lt.name,
        version_constraint_satisfies: lt.version_constraint_satisfies.as_deref(),
    });
    let now = OffsetDateTime::now_utc();

    // Score each candidate in fused order. Rows missing (deleted since the
    // candidate fetch) are skipped.
    let mut scored: Vec<ScoredCandidate> = Vec::with_capacity(fused.len());
    for (chunk_id, rrf_score) in fused {
        let Some(row) = rows.get(&chunk_id) else {
            continue;
        };
        // Apply the semver-bearing filter dimensions (language_target /
        // sdk_dependency, #11/FR-033) that SQL can't express. The scalar
        // dimensions were already enforced during candidate retrieval.
        if !req.filters.semver_post_match(&row.provenance) {
            continue;
        }
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
            version_query.as_ref(),
            age_days,
            relevance,
            RelevanceSource::Rrf,
        );
        scored.push(ScoredCandidate {
            chunk_id,
            content: row.content.clone(),
            document_id: row.document_id,
            source_version_id: row.source_version_id,
            chunk_index: row.chunk_index,
            total_chunks: row.total_chunks,
            created_at: row.created_at,
            rrf_score,
            vector_similarity,
            matched_queries,
            relevance,
            score,
        });
    }

    // Drop candidates below the confidence floor before applying `limit` (#10).
    let min_confidence = req.min_confidence.clamp(0.0, 1.0);
    let before = scored.len();
    scored.retain(|c| c.score.confidence >= min_confidence);
    let filtered_by_confidence = before - scored.len();

    // Sort by the requested key, then truncate (#9). The sort is stable, so
    // the fused (RRF) order breaks ties.
    sort_candidates(&mut scored, req.sort_by);
    scored.truncate(limit as usize);

    let results: Vec<SearchResult> = scored
        .into_iter()
        .map(|c| c.into_result(req.include_scores))
        .collect();

    Json(SearchResponse {
        results,
        search_metadata: SearchMetadata {
            per_query,
            total_candidates,
            deduplicated_count,
            filtered_by_confidence,
            sort_by: req.sort_by,
        },
    })
    .into_response()
}

/// A fused candidate with its computed scores, awaiting filter/sort/truncate.
struct ScoredCandidate {
    chunk_id: Uuid,
    content: String,
    document_id: Uuid,
    source_version_id: Uuid,
    chunk_index: i32,
    total_chunks: i32,
    created_at: OffsetDateTime,
    rrf_score: f64,
    vector_similarity: f64,
    matched_queries: Vec<usize>,
    /// Normalized RRF relevance term (used when `sort_by = relevance`).
    relevance: f64,
    score: ScoreResult,
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
            scores,
        }
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
    created_at: OffsetDateTime,
    provenance: Provenance,
    source_modified_at: Option<OffsetDateTime>,
    ingested_at: OffsetDateTime,
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
                chunk.total_chunks, chunk.content, chunk.created_at, \
                d.provenance AS provenance, d.source_modified_at AS source_modified_at, \
                sv.ingested_at AS ingested_at \
         FROM chunk \
         JOIN document d ON d.id = chunk.document_id \
         JOIN source_version sv ON sv.id = chunk.source_version_id \
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
        map.insert(
            id,
            ScoringRow {
                content: r.try_get("content")?,
                document_id: r.try_get("document_id")?,
                source_version_id: r.try_get("source_version_id")?,
                chunk_index: r.try_get("chunk_index")?,
                total_chunks: r.try_get("total_chunks")?,
                created_at: r.try_get("created_at")?,
                provenance,
                source_modified_at: r.try_get("source_modified_at")?,
                ingested_at: r.try_get("ingested_at")?,
            },
        );
    }
    Ok(map)
}

/// Resolve the effective query list from either the canonical `queries` array
/// or the single-query convenience form `{query, vector}` (acceptance #6).
///
/// The two forms are mutually exclusive. Returns `Err((message, remediation))`
/// for an ambiguous (both forms) or incomplete (only one of `query`/`vector`)
/// request. An entirely empty request yields `Ok(vec![])` so the caller's
/// empty-queries guard produces the canonical error.
fn normalize_queries(req: &SearchRequest) -> Result<Vec<QueryPair>, (String, String)> {
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
        }]),
        (Some(_), None) | (None, Some(_)) => Err((
            "the single-query form requires both `query` and `vector`".to_owned(),
            "include the embedding `vector` (dimension must match the corpus model) alongside `query`, or use the `queries` array".to_owned(),
        )),
        (None, None) => Ok(Vec::new()),
    }
}

/// Stable content hash of a query pair (`text` + the raw bits of each vector
/// component) used to detect duplicate queries for EC-90 dedup.
fn query_hash(q: &QueryPair) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    q.text.hash(&mut h);
    for f in &q.vector {
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
    corpus_model_id: Uuid,
) -> Result<Vec<(Uuid, f64)>, sqlx::Error> {
    // pgvector cosine distance: 0 = identical, 2 = opposite. Top 100 per query.
    let mut qb: QueryBuilder<Postgres> =
        QueryBuilder::new("SELECT chunk.id, 1 - (chunk.embedding <=> ");
    qb.push_bind(Vector::from(vector.to_vec()));
    qb.push(") AS similarity FROM chunk JOIN source_version sv ON sv.id = chunk.source_version_id");
    push_filter_joins(&mut qb, filters);
    qb.push(
        " WHERE chunk.embedding IS NOT NULL AND chunk.status = 'ready' AND sv.is_active = true",
    );
    // Restrict to chunks encoded with the corpus model so off-model rows
    // (e.g. a mid-migration source_version on a different embedder) never leak
    // into candidates — their vectors aren't comparable to the query's.
    qb.push(" AND sv.embedding_model_id = ");
    qb.push_bind(corpus_model_id);
    push_filter_predicates(&mut qb, filters);
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
    corpus_model_id: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT chunk.id FROM chunk JOIN source_version sv ON sv.id = chunk.source_version_id",
    );
    push_filter_joins(&mut qb, filters);
    qb.push(" WHERE chunk.tsvector @@ websearch_to_tsquery('english', ");
    qb.push_bind(text.to_owned());
    qb.push(") AND chunk.status = 'ready' AND sv.is_active = true");
    // Restrict to chunks encoded with the corpus model (see `vector_search`):
    // an off-model active source_version must not contribute FTS candidates.
    qb.push(" AND sv.embedding_model_id = ");
    qb.push_bind(corpus_model_id);
    push_filter_predicates(&mut qb, filters);
    qb.push(" ORDER BY ts_rank(chunk.tsvector, websearch_to_tsquery('english', ");
    qb.push_bind(text.to_owned());
    qb.push(")) DESC LIMIT 100");

    let rows = qb.build().fetch_all(pool).await?;
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid, _>("id").ok())
        .collect())
}

/// Whether any filter needs the `document` table joined (it carries provenance
/// and `package_id`).
const fn needs_document_join(filters: &SearchFilters) -> bool {
    !filters.attribution.is_empty()
        || filters.verified.is_some()
        || !filters.content_type.is_empty()
        || !filters.package.is_empty()
}

/// Append the JOINs required by the active SQL filter dimensions. Both
/// candidate queries call this immediately after the `source_version` join so
/// the alias set (`chunk`, `sv`, `d`, `s`, `p`) is consistent.
fn push_filter_joins(qb: &mut QueryBuilder<'_, Postgres>, filters: &SearchFilters) {
    if needs_document_join(filters) {
        qb.push(" JOIN document d ON d.id = chunk.document_id");
    }
    if !filters.source_slug.is_empty() {
        qb.push(" JOIN source s ON s.id = sv.source_id");
    }
    if !filters.package.is_empty() {
        qb.push(" LEFT JOIN package p ON p.id = d.package_id");
    }
}

/// Append the SQL-expressible filter predicates (`attribution`, `verified`,
/// `content_type`, `source_slug`, `package`) as ` AND (...)` clauses. AND
/// across keys; OR within each key's value array. The semver-bearing
/// `language_target`/`sdk_dependency` dimensions are handled in Rust by
/// [`SearchFilters::semver_post_match`].
fn push_filter_predicates(qb: &mut QueryBuilder<'_, Postgres>, filters: &SearchFilters) {
    if !filters.attribution.is_empty() {
        qb.push(" AND COALESCE(d.provenance->>'attribution', 'unknown') = ANY(");
        qb.push_bind(filters.attribution.clone());
        qb.push(")");
    }
    if let Some(verified) = filters.verified {
        qb.push(" AND COALESCE((d.provenance->>'verified')::boolean, false) = ");
        qb.push_bind(verified);
    }
    if !filters.content_type.is_empty() {
        qb.push(" AND COALESCE(d.provenance->>'content_type', 'other') = ANY(");
        qb.push_bind(filters.content_type.clone());
        qb.push(")");
    }
    if !filters.source_slug.is_empty() {
        qb.push(" AND s.slug = ANY(");
        qb.push_bind(filters.source_slug.clone());
        qb.push(")");
    }
    if !filters.package.is_empty() {
        qb.push(" AND (");
        for (i, pkg) in filters.package.iter().enumerate() {
            if i > 0 {
                qb.push(" OR ");
            }
            qb.push("(p.kind = ");
            qb.push_bind(pkg.kind.clone());
            qb.push(" AND p.name = ");
            qb.push_bind(pkg.name.clone());
            qb.push(")");
        }
        qb.push(")");
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_queries, QueryPair, SearchRequest, SortBy};
    use mn_retrieval::filters::SearchFilters;

    fn req(
        queries: Vec<QueryPair>,
        query: Option<String>,
        vector: Option<Vec<f32>>,
    ) -> SearchRequest {
        SearchRequest {
            queries,
            query,
            vector,
            client_embedding_model: "bge-base-en-v1.5@1".to_owned(),
            limit: 20,
            filters: SearchFilters::default(),
            sort_by: SortBy::default(),
            min_confidence: 0.0,
            include_scores: true,
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
            }],
            None,
            None,
        );
        // The convenience form is pure sugar: both shapes resolve to the exact
        // same internal query list, so downstream processing is byte-identical.
        assert_eq!(
            normalize_queries(&convenience).unwrap(),
            normalize_queries(&canonical).unwrap()
        );
    }

    #[test]
    fn rejects_both_forms_at_once() {
        let v = vec![0.5_f32; 768];
        let both = req(
            vec![QueryPair {
                text: "a".to_owned(),
                vector: v.clone(),
            }],
            Some("b".to_owned()),
            Some(v),
        );
        assert!(normalize_queries(&both).is_err());
    }

    #[test]
    fn rejects_incomplete_convenience_form() {
        let only_query = req(Vec::new(), Some("a".to_owned()), None);
        let only_vector = req(Vec::new(), None, Some(vec![0.5_f32; 768]));
        assert!(normalize_queries(&only_query).is_err());
        assert!(normalize_queries(&only_vector).is_err());
    }

    #[test]
    fn empty_request_yields_empty_list() {
        assert!(normalize_queries(&req(Vec::new(), None, None))
            .unwrap()
            .is_empty());
    }
}

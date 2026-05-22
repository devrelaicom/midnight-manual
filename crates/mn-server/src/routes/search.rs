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
use mn_retrieval::filters::SearchFilters;
use pgvector::Vector;
use serde::{Deserialize, Serialize};
use sqlx::Row;
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
}

/// One {text, vector} pair.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct QueryPair {
    /// The query text, used for the full-text-search half of retrieval.
    pub text: String,
    /// The pre-computed embedding (768 dimensions for bge-base-en-v1.5).
    pub vector: Vec<f32>,
}

const fn default_limit() -> u32 {
    20
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
    /// Per-result scores (RRF + reranker + confidence land in later phases).
    pub scores: ScoreBreakdown,
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

    // Model-mismatch guard. The active corpus model is resolved at boot and
    // stamped into ServerConfig; if it's somehow None here the server is
    // mis-configured and we 503 rather than silently compare against a
    // hardcoded literal (which used to cause spec drift if migration 0006
    // ever seeded a different revision).
    let Some(corpus_model_id) = state.cfg.corpus_model.clone() else {
        return error::service_unavailable(
            "server has no resolved corpus_model; check boot logs",
            rid,
        );
    };
    if req.client_embedding_model != corpus_model_id {
        return error::into_response(
            CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                .message(format!(
                    "client_embedding_model `{}` does not match corpus model `{corpus_model_id}`",
                    req.client_embedding_model,
                ))
                .remediation("re-run `mnm models pull` to fetch the corpus model")
                .context("corpus_model", corpus_model_id.clone())
                .context("client_model", req.client_embedding_model.clone())
                .build(),
            rid,
        );
    }

    // Vector-dim guard: every query's vector must be 768 dims (the seed model).
    for (i, q) in queries.iter().enumerate() {
        if q.vector.len() != 768 {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message(format!(
                        "queries[{i}].vector has {} dimensions; expected 768",
                        q.vector.len(),
                    ))
                    .remediation("re-embed with bge-base-en-v1.5 (768 dims)")
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
        let vector_hits = match vector_search(&state.pool, &q.vector).await {
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
        let fts_hits = match fts_search(&state.pool, &q.text).await {
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

    // Single RRF pass across all (query, mode) lists.
    let list_refs: Vec<&[Uuid]> = ranked_lists.iter().map(Vec::as_slice).collect();
    let mut fused = mn_retrieval::rrf::fuse(&list_refs);
    let total_candidates = fused.len();
    fused.truncate(limit as usize);

    // Fetch full chunk rows in fused order.
    let mut results = Vec::with_capacity(fused.len());
    for (chunk_id, rrf_score) in fused {
        let row = sqlx::query(
            "SELECT id, document_id, source_version_id, chunk_index, total_chunks, \
                    content, created_at \
             FROM chunk WHERE id = $1",
        )
        .bind(chunk_id)
        .fetch_optional(&state.pool)
        .await;
        match row {
            Ok(Some(r)) => {
                // BTreeSet iterates ascending, so matched_queries is sorted.
                let matched_queries: Vec<usize> = matched
                    .get(&chunk_id)
                    .into_iter()
                    .flatten()
                    .copied()
                    .collect();
                let similarity = best_similarity.get(&chunk_id).copied().unwrap_or(0.0);
                match decode_search_row(&r, rrf_score, similarity, matched_queries) {
                    Ok(result) => results.push(result),
                    Err(e) => {
                        // Schema drift — log and skip, rather than insert a row
                        // with Uuid::nil() + now() as silent placeholders.
                        tracing::warn!(
                            request_id = rid,
                            chunk_id = %chunk_id,
                            error = %e,
                            "decode chunk row failed; skipping",
                        );
                    }
                }
            }
            Ok(None) => {
                // Chunk was deleted between candidate fetch and full-row fetch
                // — skip silently and continue.
            }
            Err(e) => {
                tracing::warn!(request_id = rid, error = %e, chunk_id = %chunk_id, "fetch chunk failed");
            }
        }
    }

    Json(SearchResponse {
        results,
        search_metadata: SearchMetadata {
            per_query,
            total_candidates,
            deduplicated_count,
        },
    })
    .into_response()
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
            "include the 768-dim `vector` alongside `query`, or use the `queries` array".to_owned(),
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
/// cosine similarity over active, ready chunks with a non-null embedding.
///
/// # Errors
///
/// Propagates any `sqlx` error from the query.
async fn vector_search(
    pool: &sqlx::PgPool,
    vector: &[f32],
) -> Result<Vec<(Uuid, f64)>, sqlx::Error> {
    let vec = Vector::from(vector.to_vec());
    // pgvector cosine distance: 0 = identical, 2 = opposite. Top 100 per query.
    let rows = sqlx::query(
        "SELECT chunk.id, 1 - (chunk.embedding <=> $1) AS similarity \
         FROM chunk \
         JOIN source_version sv ON sv.id = chunk.source_version_id \
         WHERE chunk.embedding IS NOT NULL \
           AND chunk.status = 'ready' \
           AND sv.is_active = true \
         ORDER BY chunk.embedding <=> $1 \
         LIMIT 100",
    )
    .bind(&vec)
    .fetch_all(pool)
    .await?;
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
/// ready chunks. Empty or stopword-only text yields no rows, since
/// `websearch_to_tsquery` produces a query that matches nothing.
///
/// # Errors
///
/// Propagates any `sqlx` error from the query.
async fn fts_search(pool: &sqlx::PgPool, text: &str) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT chunk.id \
         FROM chunk \
         JOIN source_version sv ON sv.id = chunk.source_version_id \
         WHERE chunk.tsvector @@ websearch_to_tsquery('english', $1) \
           AND chunk.status = 'ready' \
           AND sv.is_active = true \
         ORDER BY ts_rank(chunk.tsvector, websearch_to_tsquery('english', $1)) DESC \
         LIMIT 100",
    )
    .bind(text)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid, _>("id").ok())
        .collect())
}

/// Decode a chunk row into a `SearchResult`. Every column is `try_get`'d so a
/// schema mismatch surfaces as an `Err` for the caller to skip, rather than
/// silently substituting `Uuid::nil()` / `OffsetDateTime::now_utc()`.
fn decode_search_row(
    r: &sqlx::postgres::PgRow,
    rrf_score: f64,
    vector_similarity: f64,
    matched_queries: Vec<usize>,
) -> Result<SearchResult, sqlx::Error> {
    Ok(SearchResult {
        chunk_id: r.try_get("id")?,
        content: r.try_get("content")?,
        document_id: r.try_get("document_id")?,
        source_version_id: r.try_get("source_version_id")?,
        chunk_index: r.try_get("chunk_index")?,
        total_chunks: r.try_get("total_chunks")?,
        created_at: r.try_get("created_at")?,
        scores: ScoreBreakdown {
            rrf_score,
            vector_similarity,
            matched_queries,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{normalize_queries, QueryPair, SearchRequest};
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

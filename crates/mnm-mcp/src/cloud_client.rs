//! HTTP client used by the MCP tools to call the cloud server's read API.
//!
//! The MCP server is a thin local proxy — it embeds queries with the in-process
//! `mnm_embedding` models, posts to `/v1/search` on the cloud, and pretty-prints
//! per-chunk lookups (`/v1/chunks/:id`, `/parents`,
//! `/v1/sources`). The wire shapes mirror what `midnight-manual-server` returns; rather
//! than couple to `midnight-manual-server`'s types we deserialize to `serde_json::Value`
//! and pass through, which keeps the MCP tool's response shape additive as
//! the cloud surface evolves.
//!
//! All methods translate non-success status codes into typed [`CloudError`]
//! variants. The most important one is [`CloudError::EmbeddingModelMismatch`],
//! which the cloud raises (HTTP 409) when the caller's embedder revision
//! doesn't match the corpus's active revision (D12 / FR-038). The MCP `search`
//! tool surfaces this as a typed MCP error carrying the cloud-provided
//! remediation.

use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use url::Url;

/// One `{text, vector, code_vector}` triple posted to the cloud `/v1/search`
/// endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct QueryPair {
    /// Original query text — kept for FTS / logging on the cloud side.
    pub text: String,
    /// Locally produced general-model embedding (1024 dims for
    /// voyage-context-3). Empty in `fts` mode and when `code_mode=exclusive`.
    pub vector: Vec<f32>,
    /// Locally produced code-model embedding (voyage-code-3); required by the
    /// cloud iff the effective `code_mode != off`. Empty vectors are omitted
    /// from the wire so pre-dual-embeddings request bodies are unchanged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub code_vector: Vec<f32>,
}

/// Request body for `POST /v1/search` matching `midnight-manual-server`'s shape.
#[derive(Debug, Clone, Serialize)]
pub struct SearchRequest {
    /// Query pairs (one or more).
    pub queries: Vec<QueryPair>,
    /// `{name}@{revision}` model identifier of the local embedder.
    pub client_embedding_model: String,
    /// Max results to return from the cloud (cloud caps at 100).
    pub limit: u32,
    /// Per-facet filter spec. The MCP tool validates this at the boundary
    /// (deserialize into `mnm_retrieval::filters::SearchFilters` + `.validate()`)
    /// before forwarding, so the cloud receives a registry-conformant object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<serde_json::Value>,
    /// Cloud-side ordering key (`confidence` | `trust` | `relevance` | `score`).
    /// When reranking locally, the MCP server forces `"score"` (RRF/relevance
    /// order) so the candidate pool the cross-encoder reranks isn't pre-filtered
    /// by the cloud's confidence-first default (US6) — the local rerank then
    /// re-orders the pool afterwards, overriding any caller `sort_by`. On the
    /// server/off paths this carries the caller's explicit choice (issue #137).
    /// `None` lets the cloud apply its default (`confidence`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<&'static str>,
    /// Confidence floor in `[0, 1]`: the cloud drops results whose blended
    /// `confidence` is below this before applying `limit`, and reports the count
    /// dropped as `search_metadata.filtered_by_confidence` (issue #137). `None`
    /// (omitted) lets the cloud apply its default of `0.0` (no filtering).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f64>,
    /// Query mode forwarded to the cloud (`hybrid` | `vector` | `fts`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<&'static str>,
    /// Code-vector fusion mode (`on` | `off` | `exclusive`). `None` lets the
    /// cloud derive its mode-dependent default (on for hybrid/vector, off for
    /// fts) — only an explicit caller choice is forwarded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_mode: Option<&'static str>,
    /// `{name}@{revision}` wire id of the code-model embedder used for the
    /// `code_vector`s. Required by the cloud iff the effective
    /// `code_mode != off`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_code_embedding_model: Option<String>,
    /// Server-side rerank parameter: the `VoyageAI` model name on the `Server`
    /// placement, `"none"` on the `Local` / `Off` placements (exactly one rerank
    /// pass — `Local` reranks client-side). `None` omits the key on the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank: Option<String>,
    /// Agent-supplied rerank instruction forwarded on the `Server` placement;
    /// `None` (and omitted) on `Local` / `Off`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank_instructions: Option<String>,
    /// Version-matching mode (`strict` | `permissive`); omitted = server default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_match: Option<String>,
}

/// One embedding model's identity decoded from `GET /v1/models/active`: the
/// `{name}@{revision}` wire id plus the `{name, dim, dtype}` an embedder is
/// built from. Carrying the full identity (not just the wire id) lets the MCP
/// search path build the embedder from the SAME source that labels the vectors
/// (cross-element drift fix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveModelEntry {
    /// `{name}@{revision}` wire id (e.g. `voyage-context-3@1`).
    pub wire: String,
    /// Bare model name (e.g. `voyage-context-3`).
    pub name: String,
    /// Output dimension.
    pub dim: u32,
    /// Output dtype (e.g. `"float"`).
    pub dtype: String,
}

/// The corpus's active embedding models, decoded from `GET /v1/models/active`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveModels {
    /// General-model identity (wire id + name/dim/dtype).
    pub general: ActiveModelEntry,
    /// Code-model identity, when the corpus has one. `None` means code search
    /// is unavailable server-side.
    pub code: Option<ActiveModelEntry>,
}

/// Default embedding dim when a `/v1/models/active` entry omits `dim` (a server
/// that predates the field). 1024 is the corpus's Matryoshka dimension.
const DEFAULT_ACTIVE_DIM: u32 = 1024;
/// Default dtype when an entry omits `dtype` (a server that predates the field).
const DEFAULT_ACTIVE_DTYPE: &str = "float";

/// Parse one model entry (the top-level object or its `code` sub-object) into an
/// [`ActiveModelEntry`]. Requires `name` (string) + `revision` (integer);
/// `dim`/`dtype` default leniently so older servers keep working. Returns `None`
/// when the required fields are absent.
fn parse_active_entry(v: &serde_json::Value) -> Option<ActiveModelEntry> {
    let name = v.get("name")?.as_str()?;
    let revision = v.get("revision")?.as_i64()?;
    let dim = v
        .get("dim")
        .and_then(serde_json::Value::as_i64)
        .and_then(|d| u32::try_from(d).ok())
        .unwrap_or(DEFAULT_ACTIVE_DIM);
    let dtype = v
        .get("dtype")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(DEFAULT_ACTIVE_DTYPE)
        .to_owned();
    Some(ActiveModelEntry {
        wire: format!("{name}@{revision}"),
        name: name.to_owned(),
        dim,
        dtype,
    })
}

/// Snapshot of the server's rate-limit headers captured from a `429` response.
///
/// The rate-limit middleware sets `Retry-After` and `X-RateLimit-{Limit,
/// Remaining,Reset}` on every rejection
/// (`midnight-manual-server/src/middleware/rate_limit.rs`). Capturing them on
/// the error path lets the MCP layer tell the agent exactly how long to WAIT
/// (issue #133) instead of retrying blindly. Every field is optional so a
/// response missing a header degrades gracefully rather than failing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitSnapshot {
    /// `Retry-After` in seconds. `None` when the header is absent or
    /// unparseable — the error mapper then advises a conservative default.
    pub retry_after_secs: Option<u64>,
    /// `X-RateLimit-Limit` — the tier's budget (requests/sec).
    pub limit: Option<u64>,
    /// `X-RateLimit-Remaining` — tokens left in the bucket (0 on a rejection).
    pub remaining: Option<u64>,
    /// `X-RateLimit-Reset` — seconds until the bucket refills to capacity.
    pub reset_secs: Option<u64>,
}

/// Errors the cloud client can produce.
#[derive(Debug, Error)]
pub enum CloudError {
    /// HTTP transport failure (connect, TLS, timeout, ...).
    #[error("cloud transport error: {0}")]
    Transport(String),
    /// 404 from the cloud (e.g. unknown chunk id).
    #[error("cloud not found: {0}")]
    NotFound(String),
    /// 429 rate limited. Carries the `Retry-After` / `X-RateLimit-*` snapshot
    /// read from the response headers so the MCP layer can tell the agent to
    /// WAIT the advised delay rather than retry immediately (issue #133). The
    /// upstream body is deliberately NOT captured — it is never surfaced to the
    /// agent, so retaining it would only be a latent leak footgun.
    #[error("cloud rate limited")]
    RateLimited {
        /// Rate-limit headers captured before the body was consumed.
        snapshot: RateLimitSnapshot,
    },
    /// 401/403 authentication/authorization failure. `status` distinguishes
    /// invalid/expired credentials (401) from a valid credential with an
    /// insufficient tier (403), so the MCP layer can name the right recovery.
    /// The upstream body is deliberately NOT captured (never surfaced to the
    /// agent; a raw auth-error body is exactly the sort of thing not to echo).
    #[error("cloud auth failed (HTTP {status})")]
    AuthFailed {
        /// The failing status — `401` or `403`.
        status: u16,
    },
    /// 409 embedding-model mismatch — surfaced specially so the MCP layer can
    /// emit a typed JSON-RPC error carrying the cloud-provided remediation.
    #[error(
        "embedding model mismatch: corpus expects `{corpus_model}`, client sent `{client_model}`"
    )]
    EmbeddingModelMismatch {
        /// `{name}@{revision}` of the corpus's active model.
        corpus_model: String,
        /// `{name}@{revision}` the client sent.
        client_model: String,
        /// Operator-facing message from the cloud.
        message: String,
        /// Concrete next step (cloud-provided).
        remediation: String,
    },
    /// Any other non-success status — body is parsed best-effort as JSON.
    #[error("cloud status {status}: {body}")]
    Status {
        /// HTTP status code returned by the cloud.
        status: u16,
        /// Best-effort body string for diagnostics.
        body: String,
    },
    /// JSON decoding failure on a successful HTTP response.
    #[error("cloud decode error: {0}")]
    Decode(String),
}

/// HTTP wrapper around the cloud read API.
#[derive(Debug, Clone)]
pub struct CloudClient {
    base: Url,
    bearer: Option<String>,
    http: reqwest::Client,
}

impl CloudClient {
    /// Build a client targeting `base` with an optional bearer token.
    ///
    /// # Errors
    ///
    /// Returns [`CloudError::Transport`] if `base` is not a valid URL or if the
    /// underlying `reqwest::Client` cannot be built (TLS init failure, etc.).
    pub fn new(base: &str, bearer: Option<String>) -> Result<Self, CloudError> {
        let mut base = Url::parse(base).map_err(|e| CloudError::Transport(e.to_string()))?;
        // Guarantee the base path ends in `/` so relative endpoint joins (RFC
        // 3986 §5.2) preserve any reverse-proxy path prefix. `Url::parse` yields
        // path `/mnm` (no trailing slash) for `https://host/mnm`; joining a
        // relative `v1/search` onto that would replace the `mnm` segment and
        // drop the prefix. Appending the slash first makes it `/mnm/`, so the
        // prefix survives every join below (host-only bases already end in `/`).
        if !base.path().ends_with('/') {
            let with_slash = format!("{}/", base.path());
            base.set_path(&with_slash);
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("midnight-manual-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        Ok(Self { base, bearer, http })
    }

    /// Return the bearer token configured on this client, if any.
    ///
    /// Used by `run_search` when it needs to forward the same bearer for the
    /// server-proxy embedding path (`EmbedSource::Server`).
    #[must_use]
    pub fn bearer(&self) -> Option<&str> {
        self.bearer.as_deref()
    }

    /// `GET /v1/models/active` — returns the corpus's active embedding models:
    /// the general `{name}@{revision}` wire id plus, when the corpus carries
    /// dual embeddings, the code model's wire id from the response's `code`
    /// sub-object.
    ///
    /// The response is expected to have at least `name` (string) and `revision`
    /// (integer) fields. The optional `code` sub-object is decoded leniently —
    /// absent or malformed means "no code model" (code search unavailable),
    /// never a decode error, so older servers keep working.
    ///
    /// # Errors
    ///
    /// Returns [`CloudError::Transport`] on a connection failure,
    /// [`CloudError::NotFound`] on a 404, [`CloudError::Status`] for any other
    /// non-2xx response, or [`CloudError::Decode`] for a body that fails to
    /// parse or is missing the top-level `name`/`revision` fields.
    pub async fn fetch_active_model(&self) -> Result<ActiveModels, CloudError> {
        let v = self.get_json("v1/models/active").await?;
        let general = parse_active_entry(&v).ok_or_else(|| {
            CloudError::Decode(
                "/v1/models/active response missing `name`/`revision` field".to_owned(),
            )
        })?;
        // The optional `code` sub-object is decoded leniently — absent or
        // malformed means "no code model", never a decode error.
        let code = v.get("code").and_then(parse_active_entry);
        Ok(ActiveModels { general, code })
    }

    /// `POST /v1/search`.
    pub async fn search(&self, req: &SearchRequest) -> Result<serde_json::Value, CloudError> {
        let url = self.endpoint("v1/search")?;
        let mut rb = self.http.post(url).json(req);
        if let Some(b) = &self.bearer {
            rb = rb.bearer_auth(b);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<serde_json::Value>()
                .await
                .map_err(|e| CloudError::Decode(e.to_string()));
        }

        // Non-2xx. Capture the rate-limit headers BEFORE consuming the body,
        // then buffer and classify. 409 embedding-model mismatch is checked
        // first because only `/v1/search` can raise it.
        let snapshot = read_rate_limit_snapshot(resp.headers());
        let body_bytes = resp.bytes().await.unwrap_or_default();
        if status == reqwest::StatusCode::CONFLICT {
            if let Some(typed) = parse_mismatch(&body_bytes) {
                return Err(typed);
            }
        }
        Err(classify_status(status, snapshot, &body_bytes))
    }

    /// `GET /v1/chunks/:id`.
    pub async fn get_chunk(&self, id: &str) -> Result<serde_json::Value, CloudError> {
        let path = format!("v1/chunks/{id}");
        self.get_json(&path).await
    }

    /// `GET /v1/chunks/:id/next?count=N`.
    pub async fn get_chunk_next(
        &self,
        id: &str,
        count: u32,
    ) -> Result<serde_json::Value, CloudError> {
        let path = format!("v1/chunks/{id}/next?count={count}");
        self.get_json(&path).await
    }

    /// `GET /v1/chunks/:id/prev?count=N`.
    pub async fn get_chunk_prev(
        &self,
        id: &str,
        count: u32,
    ) -> Result<serde_json::Value, CloudError> {
        let path = format!("v1/chunks/{id}/prev?count={count}");
        self.get_json(&path).await
    }

    /// `GET /v1/chunks/:id/parents`.
    pub async fn get_chunk_parents(&self, id: &str) -> Result<serde_json::Value, CloudError> {
        let path = format!("v1/chunks/{id}/parents");
        self.get_json(&path).await
    }

    /// Compose `get_chunk_prev` + `get_chunk` + `get_chunk_next` into a single
    /// round-trip by issuing the three HTTP calls concurrently with
    /// `tokio::try_join!`. The cost on the wire is one connection (reqwest's
    /// connection pool keeps things keep-alive), but with three parallel
    /// in-flight responses — so latency is `max(prev, get, next)` rather than
    /// their sum.
    ///
    /// Returns `{prev, chunk, next}` where `prev`/`next` are the cloud's full
    /// `{chunks: ChunkWithContext[]}` envelopes and `chunk` is the cloud's
    /// `/v1/chunks/:id` body verbatim. Any of the three failing aborts the
    /// other two and propagates the error — most importantly, a 404 on the
    /// anchor `chunk` yields the same [`CloudError::NotFound`] a plain
    /// `get_chunk` would.
    pub async fn get_chunk_neighbors(
        &self,
        id: &str,
        prev_count: u32,
        next_count: u32,
    ) -> Result<serde_json::Value, CloudError> {
        let (prev, chunk, next) = tokio::try_join!(
            self.get_chunk_prev(id, prev_count),
            self.get_chunk(id),
            self.get_chunk_next(id, next_count),
        )?;
        Ok(serde_json::json!({
            "prev": prev,
            "chunk": chunk,
            "next": next,
        }))
    }

    /// `GET /v1/chunks?ids=a,b,c` — batch fetch, input order preserved server-side.
    /// Returns `{ chunks: [...], missing: [...] }`.
    ///
    /// # Errors
    ///
    /// Propagates any [`CloudError`] from the transport / status mapping.
    pub async fn get_chunks(&self, ids: &[String]) -> Result<serde_json::Value, CloudError> {
        let mut url = self.endpoint("v1/chunks")?;
        url.query_pairs_mut().append_pair("ids", &ids.join(","));
        self.get_json_url(url).await
    }

    /// `GET /v1/sources` with pagination/filter params, appended as query
    /// pairs (percent-encoding handled by `query_pairs_mut`).
    ///
    /// # Errors
    ///
    /// Propagates any [`CloudError`] from the transport / status mapping.
    pub async fn list_sources(
        &self,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, CloudError> {
        let mut url = self.endpoint("v1/sources")?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
        self.get_json_url(url).await
    }

    /// `GET /v1/documents/:id`.
    pub async fn get_document(&self, id: &str) -> Result<serde_json::Value, CloudError> {
        let path = format!("v1/documents/{id}");
        self.get_json(&path).await
    }

    /// `GET /v1/documents/:id/chunks?from=K&limit=N`.
    pub async fn get_document_chunks(
        &self,
        id: &str,
        from: u32,
        limit: u32,
    ) -> Result<serde_json::Value, CloudError> {
        let path = format!("v1/documents/{id}/chunks?from={from}&limit={limit}");
        self.get_json(&path).await
    }

    /// `GET /v1/facets` — overview when `params` is empty, drill-down otherwise.
    ///
    /// # Errors
    ///
    /// Propagates any [`CloudError`] from the transport / status mapping.
    pub async fn get_facets(
        &self,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, CloudError> {
        let mut url = self.endpoint("v1/facets")?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
        self.get_json_url(url).await
    }

    /// `GET /v1/me` — auth / rate-limit / token-budget introspection.
    ///
    /// # Errors
    ///
    /// Propagates any [`CloudError`] from the transport / status mapping.
    pub async fn get_me(&self) -> Result<serde_json::Value, CloudError> {
        self.get_json("v1/me").await
    }

    /// `GET /readyz` — returns the HTTP status code (no body parsing).
    ///
    /// # Errors
    ///
    /// Returns [`CloudError::Transport`] on connection failure only; any
    /// HTTP status (200 or not) is returned as data.
    pub async fn readyz(&self) -> Result<u16, CloudError> {
        let url = self.endpoint("readyz")?;
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        Ok(resp.status().as_u16())
    }

    /// Join a **relative** endpoint reference (no leading `/`) onto
    /// [`Self::base`], which [`Self::new`] guarantees ends in `/`. A relative
    /// reference preserves any base-URL path prefix; an absolute-path reference
    /// (`/v1/...`) would replace the whole path per RFC 3986 §5.2 and silently
    /// drop a configured mount point like `/mnm/`, 404-ing every call behind a
    /// reverse-proxy prefix.
    fn endpoint(&self, rel: &str) -> Result<Url, CloudError> {
        self.base
            .join(rel)
            .map_err(|e| CloudError::Transport(e.to_string()))
    }

    async fn get_json(&self, path: &str) -> Result<serde_json::Value, CloudError> {
        let url = self.endpoint(path)?;
        self.get_json_url(url).await
    }

    async fn get_json_url(&self, url: Url) -> Result<serde_json::Value, CloudError> {
        let mut rb = self.http.get(url);
        if let Some(b) = &self.bearer {
            rb = rb.bearer_auth(b);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<serde_json::Value>()
                .await
                .map_err(|e| CloudError::Decode(e.to_string()));
        }
        // Capture rate-limit headers before consuming the body, then classify.
        let snapshot = read_rate_limit_snapshot(resp.headers());
        let body_bytes = resp.bytes().await.unwrap_or_default();
        Err(classify_status(status, snapshot, &body_bytes))
    }
}

/// Upper clamp on any upstream-supplied wait/reset seconds. `Retry-After` and
/// `X-RateLimit-Reset` are attacker-influenceable; without a ceiling a parseable
/// but absurd value (e.g. `u64::MAX`) would be echoed into the agent's wait hint
/// and wedge an agent that honors it. One hour is far beyond any real per-second
/// bucket reset, so clamping here can only ever cap a hostile/broken value.
const MAX_RATE_LIMIT_BACKOFF_SECS: u64 = 3600;

/// Read the server's rate-limit headers (`Retry-After`, `X-RateLimit-*`) into a
/// [`RateLimitSnapshot`]. MUST be called BEFORE the response body is consumed
/// (`resp.bytes()` takes `self`), so the 429 path captures the wait hint the
/// agent needs. Header names are matched case-insensitively by `reqwest`.
fn read_rate_limit_snapshot(headers: &reqwest::header::HeaderMap) -> RateLimitSnapshot {
    fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
        headers.get(name)?.to_str().ok()?.trim().parse().ok()
    }
    RateLimitSnapshot {
        // Clamp the two duration-valued headers — they gate how long an agent
        // waits, so a hostile/absurd value must not be able to wedge it (#133).
        retry_after_secs: header_u64(headers, "retry-after")
            .map(|v| v.min(MAX_RATE_LIMIT_BACKOFF_SECS)),
        limit: header_u64(headers, "x-ratelimit-limit"),
        remaining: header_u64(headers, "x-ratelimit-remaining"),
        reset_secs: header_u64(headers, "x-ratelimit-reset")
            .map(|v| v.min(MAX_RATE_LIMIT_BACKOFF_SECS)),
    }
}

/// Classify a non-success status into a typed [`CloudError`]:
/// `429` → [`CloudError::RateLimited`] (carrying the header snapshot),
/// `401`/`403` → [`CloudError::AuthFailed`], `404` → [`CloudError::NotFound`],
/// everything else (5xx, unexpected 4xx) → [`CloudError::Status`]. The `409`
/// embedding-model mismatch is handled by the caller before this, since only
/// `/v1/search` can raise it.
///
/// `snapshot` is taken by value because the `429` arm moves it into
/// `CloudError::RateLimited`; the other arms simply drop it — so a by-reference
/// signature would force a needless clone on the one path that matters.
fn classify_status(
    status: reqwest::StatusCode,
    snapshot: RateLimitSnapshot,
    body_bytes: &[u8],
) -> CloudError {
    match status.as_u16() {
        429 => CloudError::RateLimited { snapshot },
        401 | 403 => CloudError::AuthFailed { status: status.as_u16() },
        404 => CloudError::NotFound(String::from_utf8_lossy(body_bytes).into_owned()),
        other => CloudError::Status {
            status: other,
            body: String::from_utf8_lossy(body_bytes).into_owned(),
        },
    }
}

/// Parse the cloud's `{ error: { code, message, remediation, context: {corpus_model, client_model} } }`
/// envelope into [`CloudError::EmbeddingModelMismatch`]. Returns `None` if the
/// envelope shape doesn't match — in that case the caller falls back to
/// [`CloudError::Status`].
fn parse_mismatch(body: &[u8]) -> Option<CloudError> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let err = v.get("error")?;
    let code = err.get("code")?.as_str()?;
    if code != "embedding_model_mismatch" {
        return None;
    }
    let message = err.get("message")?.as_str().unwrap_or("").to_owned();
    let remediation = err
        .get("remediation")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_owned();
    let ctx = err.get("context").and_then(|c| c.as_object());
    let corpus_model = ctx
        .and_then(|c| c.get("corpus_model"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    let client_model = ctx
        .and_then(|c| c.get("client_model"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some(CloudError::EmbeddingModelMismatch {
        corpus_model,
        client_model,
        message,
        remediation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mismatch_extracts_corpus_and_client_model() {
        let body = serde_json::json!({
            "error": {
                "code": "embedding_model_mismatch",
                "message": "client_embedding_model `bge-base-en-v1.5@1` does not match corpus model `bge-base-en-v1.5@2`",
                "remediation": "run `mnm models active` to see the corpus's active model",
                "context": {
                    "corpus_model": "bge-base-en-v1.5@2",
                    "client_model": "bge-base-en-v1.5@1",
                },
            },
            "request_id": "abc",
        })
        .to_string();
        let err = parse_mismatch(body.as_bytes()).expect("typed mismatch");
        match err {
            CloudError::EmbeddingModelMismatch { corpus_model, client_model, .. } => {
                assert_eq!(corpus_model, "bge-base-en-v1.5@2");
                assert_eq!(client_model, "bge-base-en-v1.5@1");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_mismatch_returns_none_for_unrelated_envelope() {
        let body = serde_json::json!({
            "error": { "code": "invalid_request", "message": "bad", "remediation": "fix" },
        })
        .to_string();
        assert!(parse_mismatch(body.as_bytes()).is_none());
    }

    #[test]
    fn new_rejects_invalid_url() {
        let r = CloudClient::new("not-a-url", None);
        assert!(matches!(r, Err(CloudError::Transport(_))));
    }

    #[test]
    fn endpoint_preserves_base_path_prefix() {
        // A reverse-proxy mount at `/mnm` must survive endpoint joins. Both the
        // slash-terminated and bare forms normalize to the same prefixed URL —
        // an absolute-path `join("/v1/...")` would instead 404 by dropping `/mnm`.
        for base in ["https://host.example/mnm/", "https://host.example/mnm"] {
            let c = CloudClient::new(base, None).expect("client");
            assert_eq!(
                c.endpoint("v1/search").unwrap().as_str(),
                "https://host.example/mnm/v1/search",
                "base {base} lost its path prefix on a v1 join",
            );
            assert_eq!(
                c.endpoint("readyz").unwrap().as_str(),
                "https://host.example/mnm/readyz",
                "base {base} lost its path prefix on a readyz join",
            );
        }
    }

    #[test]
    fn endpoint_deep_prefix_and_query_string_are_preserved() {
        // A multi-segment prefix and a query-bearing reference both survive.
        let c = CloudClient::new("https://host.example/a/b", None).expect("client");
        assert_eq!(
            c.endpoint("v1/chunks/xyz/next?count=3").unwrap().as_str(),
            "https://host.example/a/b/v1/chunks/xyz/next?count=3",
        );
    }

    #[test]
    fn endpoint_host_only_base_is_unaffected() {
        // The production host-only base keeps working: there is no prefix to
        // preserve, and the parsed path already ends in `/`.
        let c = CloudClient::new("https://host.example", None).expect("client");
        assert_eq!(c.endpoint("v1/search").unwrap().as_str(), "https://host.example/v1/search",);
        assert_eq!(c.endpoint("readyz").unwrap().as_str(), "https://host.example/readyz",);
    }

    fn headers(pairs: &[(&'static str, &str)]) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, reqwest::header::HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn read_snapshot_parses_all_rate_limit_headers() {
        let h = headers(&[
            ("retry-after", "30"),
            ("x-ratelimit-limit", "5"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "2"),
        ]);
        let s = read_rate_limit_snapshot(&h);
        assert_eq!(s.retry_after_secs, Some(30));
        assert_eq!(s.limit, Some(5));
        assert_eq!(s.remaining, Some(0));
        assert_eq!(s.reset_secs, Some(2));
    }

    #[test]
    fn read_snapshot_missing_headers_are_none() {
        let s = read_rate_limit_snapshot(&reqwest::header::HeaderMap::new());
        assert_eq!(s, RateLimitSnapshot::default());
        // A non-numeric Retry-After (HTTP-date form) degrades to None, not a panic.
        let h = headers(&[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")]);
        assert_eq!(read_rate_limit_snapshot(&h).retry_after_secs, None);
    }

    #[test]
    fn read_snapshot_clamps_absurd_duration_headers() {
        // An upstream-influenced, parseable-but-absurd duration must be capped so
        // it cannot wedge an agent that honors the wait (#133 L1). Non-duration
        // headers (limit/remaining) are echoed verbatim — they are not waits.
        let h = headers(&[
            ("retry-after", "18446744073709551615"), // u64::MAX
            ("x-ratelimit-reset", "999999999"),
            ("x-ratelimit-remaining", "0"),
        ]);
        let s = read_rate_limit_snapshot(&h);
        assert_eq!(s.retry_after_secs, Some(MAX_RATE_LIMIT_BACKOFF_SECS));
        assert_eq!(s.reset_secs, Some(MAX_RATE_LIMIT_BACKOFF_SECS));
        assert_eq!(s.remaining, Some(0), "non-duration headers are not clamped");
    }

    #[test]
    fn classify_maps_429_to_rate_limited_with_snapshot() {
        let snap = RateLimitSnapshot {
            retry_after_secs: Some(30),
            ..Default::default()
        };
        let e = classify_status(reqwest::StatusCode::TOO_MANY_REQUESTS, snap, b"{}");
        match e {
            CloudError::RateLimited { snapshot, .. } => {
                assert_eq!(snapshot.retry_after_secs, Some(30));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn classify_maps_401_and_403_to_auth_failed() {
        for code in [
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::FORBIDDEN,
        ] {
            let e = classify_status(code, RateLimitSnapshot::default(), b"{}");
            match e {
                CloudError::AuthFailed { status, .. } => assert_eq!(status, code.as_u16()),
                other => panic!("expected AuthFailed for {code}, got {other:?}"),
            }
        }
    }

    #[test]
    fn classify_keeps_404_and_5xx_unchanged() {
        assert!(matches!(
            classify_status(reqwest::StatusCode::NOT_FOUND, RateLimitSnapshot::default(), b"x"),
            CloudError::NotFound(_)
        ));
        assert!(matches!(
            classify_status(
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                RateLimitSnapshot::default(),
                b"boom"
            ),
            CloudError::Status { status: 500, .. }
        ));
    }
}

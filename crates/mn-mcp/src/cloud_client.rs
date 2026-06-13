//! HTTP client used by the MCP tools to call the cloud server's read API.
//!
//! The MCP server is a thin local proxy — it embeds queries with the in-process
//! `mn_embedding` models, posts to `/v1/search` on the cloud, and pretty-prints
//! per-chunk lookups (`/v1/chunks/:id`, `/parents`,
//! `/v1/sources`). The wire shapes mirror what `mn-server` returns; rather
//! than couple to `mn-server`'s types we deserialize to `serde_json::Value`
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

/// Request body for `POST /v1/search` matching `mn-server`'s shape.
#[derive(Debug, Clone, Serialize)]
pub struct SearchRequest {
    /// Query pairs (one or more).
    pub queries: Vec<QueryPair>,
    /// `{name}@{revision}` model identifier of the local embedder.
    pub client_embedding_model: String,
    /// Max results to return from the cloud (cloud caps at 100).
    pub limit: u32,
    /// Per-facet filter spec. The MCP tool validates this at the boundary
    /// (deserialize into `mn_retrieval::filters::SearchFilters` + `.validate()`)
    /// before forwarding, so the cloud receives a registry-conformant object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<serde_json::Value>,
    /// Cloud-side ordering key. When reranking locally, the MCP server asks for
    /// `"score"` (RRF/relevance order) so the candidate pool the cross-encoder
    /// reranks isn't pre-filtered by the cloud's confidence-first default
    /// (US6). `None` lets the cloud apply its default (`confidence`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<&'static str>,
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

/// The corpus's active embedding models, decoded from `GET /v1/models/active`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveModels {
    /// General-model `{name}@{revision}` wire id (e.g. `voyage-context-3@1`).
    pub general: String,
    /// Code-model wire id, when the corpus has one. `None` means code search
    /// is unavailable server-side.
    pub code: Option<String>,
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
        let base = Url::parse(base).map_err(|e| CloudError::Transport(e.to_string()))?;
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
        let v = self.get_json("/v1/models/active").await?;
        let name = v
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                CloudError::Decode("/v1/models/active response missing `name` field".to_owned())
            })?;
        let revision = v
            .get("revision")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| {
                CloudError::Decode("/v1/models/active response missing `revision` field".to_owned())
            })?;
        let code = v.get("code").and_then(|c| {
            let code_name = c.get("name")?.as_str()?;
            let code_revision = c.get("revision")?.as_i64()?;
            Some(format!("{code_name}@{code_revision}"))
        });
        Ok(ActiveModels {
            general: format!("{name}@{revision}"),
            code,
        })
    }

    /// `POST /v1/search`.
    pub async fn search(&self, req: &SearchRequest) -> Result<serde_json::Value, CloudError> {
        let url = self
            .base
            .join("/v1/search")
            .map_err(|e| CloudError::Transport(e.to_string()))?;
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

        // Non-2xx. Buffer the body and inspect; we treat 409 specially.
        let body_bytes = resp.bytes().await.unwrap_or_default();
        if status == reqwest::StatusCode::CONFLICT {
            if let Some(typed) = parse_mismatch(&body_bytes) {
                return Err(typed);
            }
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(CloudError::NotFound(String::from_utf8_lossy(&body_bytes).into_owned()));
        }
        Err(CloudError::Status {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&body_bytes).into_owned(),
        })
    }

    /// `GET /v1/chunks/:id`.
    pub async fn get_chunk(&self, id: &str) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/chunks/{id}");
        self.get_json(&path).await
    }

    /// `GET /v1/chunks/:id/next?count=N`.
    pub async fn get_chunk_next(
        &self,
        id: &str,
        count: u32,
    ) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/chunks/{id}/next?count={count}");
        self.get_json(&path).await
    }

    /// `GET /v1/chunks/:id/prev?count=N`.
    pub async fn get_chunk_prev(
        &self,
        id: &str,
        count: u32,
    ) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/chunks/{id}/prev?count={count}");
        self.get_json(&path).await
    }

    /// `GET /v1/chunks/:id/parents`.
    pub async fn get_chunk_parents(&self, id: &str) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/chunks/{id}/parents");
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
        let mut url = self
            .base
            .join("/v1/chunks")
            .map_err(|e| CloudError::Transport(e.to_string()))?;
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
        let mut url = self
            .base
            .join("/v1/sources")
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
        self.get_json_url(url).await
    }

    /// `GET /v1/documents/:id`.
    pub async fn get_document(&self, id: &str) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/documents/{id}");
        self.get_json(&path).await
    }

    /// `GET /v1/documents/:id/chunks?from=K&limit=N`.
    pub async fn get_document_chunks(
        &self,
        id: &str,
        from: u32,
        limit: u32,
    ) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/documents/{id}/chunks?from={from}&limit={limit}");
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
        let mut url = self
            .base
            .join("/v1/facets")
            .map_err(|e| CloudError::Transport(e.to_string()))?;
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
        self.get_json("/v1/me").await
    }

    /// `GET /readyz` — returns the HTTP status code (no body parsing).
    ///
    /// # Errors
    ///
    /// Returns [`CloudError::Transport`] on connection failure only; any
    /// HTTP status (200 or not) is returned as data.
    pub async fn readyz(&self) -> Result<u16, CloudError> {
        let url = self
            .base
            .join("/readyz")
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        Ok(resp.status().as_u16())
    }

    async fn get_json(&self, path: &str) -> Result<serde_json::Value, CloudError> {
        let url = self
            .base
            .join(path)
            .map_err(|e| CloudError::Transport(e.to_string()))?;
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
        let body_bytes = resp.bytes().await.unwrap_or_default();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(CloudError::NotFound(String::from_utf8_lossy(&body_bytes).into_owned()));
        }
        Err(CloudError::Status {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&body_bytes).into_owned(),
        })
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
                "remediation": "re-run `mnm models pull` to fetch the corpus model",
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
}

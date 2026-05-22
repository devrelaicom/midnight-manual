//! HTTP client used by the MCP tools to call the cloud server's read API.
//!
//! The MCP server is a thin local proxy — it embeds queries with the in-process
//! `mn_embedding` models, posts to `/v1/search` on the cloud, and pretty-prints
//! per-chunk lookups (`/v1/chunks/:id`, `/siblings`, `/parents`,
//! `/v1/sources`). The wire shapes mirror what `mn-server` returns; rather
//! than couple to `mn-server`'s types we deserialize to `serde_json::Value`
//! and pass through, which keeps the MCP tool's response shape additive as
//! the cloud surface evolves.
//!
//! All methods translate non-success status codes into typed [`CloudError`]
//! variants. The most important one is [`CloudError::EmbeddingModelMismatch`],
//! which the cloud raises (HTTP 409) when the caller's embedder revision
//! doesn't match the corpus's active revision (D12 / FR-038). The MCP `search`
//! tool surfaces this as a typed MCP error so AI clients can call
//! `pull_models` and retry.

use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use url::Url;

/// One `{text, vector}` pair posted to the cloud `/v1/search` endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct QueryPair {
    /// Original query text — kept for FTS / logging on the cloud side.
    pub text: String,
    /// Locally produced embedding vector (768 dims for bge-base-en-v1.5).
    pub vector: Vec<f32>,
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
    /// Filter spec passed through verbatim. The MCP tool's input schema mirrors
    /// the cloud's, so callers can pass arbitrary filter JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<serde_json::Value>,
    /// Cloud-side ordering key. When reranking locally, the MCP server asks for
    /// `"score"` (RRF/relevance order) so the candidate pool the cross-encoder
    /// reranks isn't pre-filtered by the cloud's confidence-first default
    /// (US6). `None` lets the cloud apply its default (`confidence`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<&'static str>,
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
    /// emit a typed JSON-RPC error pointing the caller at `pull_models`.
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

    /// `GET /v1/chunks/:id/siblings`.
    pub async fn get_chunk_siblings(&self, id: &str) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/chunks/{id}/siblings");
        self.get_json(&path).await
    }

    /// `GET /v1/chunks/:id/parents`.
    pub async fn get_chunk_parents(&self, id: &str) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/chunks/{id}/parents");
        self.get_json(&path).await
    }

    /// `GET /v1/sources`.
    pub async fn list_sources(&self) -> Result<serde_json::Value, CloudError> {
        self.get_json("/v1/sources").await
    }

    async fn get_json(&self, path: &str) -> Result<serde_json::Value, CloudError> {
        let url = self
            .base
            .join(path)
            .map_err(|e| CloudError::Transport(e.to_string()))?;
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

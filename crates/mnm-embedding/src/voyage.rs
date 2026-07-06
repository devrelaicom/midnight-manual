//! VoyageAI embeddings + reranking HTTP client (raw reqwest; no official Rust SDK).
use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.voyageai.com";

/// Default per-request timeout (seconds) for embedding calls.
///
/// Over HTTP/1.1 (see `voyage_http_client`) a 250-chunk `voyage-code-3` batch
/// returns in ~2.4s, so this is generous headroom for very large batches or a
/// slow/throttled account tier — not a hot path.
pub const DEFAULT_EMBED_TIMEOUT_SECS: u64 = 120;

/// Build the reqwest client used for all VoyageAI calls.
///
/// Forces HTTP/1.1. reqwest negotiates HTTP/2 by default, but Voyage's HTTP/2
/// endpoint stalls and resets mid-request on multi-hundred-chunk embedding
/// batches — surfacing as `error sending request` after ~20-40s even with a
/// generous timeout. Over HTTP/1.1 the identical batch returns in ~2.4s
/// (verified against the live API).
pub(crate) fn voyage_http_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .http1_only()
        .build()
        .expect("build reqwest client")
}

/// Whether the input texts are queries or documents.
///
/// Voyage recommends setting this so the model can optimise embeddings accordingly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputType {
    /// Short search queries.
    Query,
    /// Full documents or chunks being indexed.
    Document,
}

impl InputType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Document => "document",
        }
    }
}

/// Errors returned by the Voyage HTTP client.
#[derive(Debug, thiserror::Error)]
pub enum VoyageError {
    /// A transport-level error that is safe to retry: the request never reached
    /// the server or was rejected before any work began (connection refused /
    /// reset, DNS failure, …). Because no tokens were consumed, retrying the
    /// identical batch cannot double-count against the shared cap.
    #[error("voyage http error: {0}")]
    Http(String),
    /// A client-side timeout. Distinct from [`VoyageError::Http`] because the
    /// request may already have reached the server and be consuming tokens
    /// (Voyage keeps proving after our deadline elapses), so the lost response
    /// does NOT mean no work was done. Retrying it would re-POST the identical
    /// batch and bill the same tokens a second time, so it is treated as
    /// non-retryable (issue #164).
    #[error("voyage request timed out: {0}")]
    Timeout(String),
    /// The server returned a non-2xx status code.
    #[error("voyage returned status {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Response body, if readable.
        body: String,
    },
    /// The response body could not be decoded as the expected JSON shape.
    #[error("voyage response decode error: {0}")]
    Decode(String),
}

impl VoyageError {
    /// Classify a `reqwest` transport failure, separating a client-side timeout
    /// (which may already have reached the server — not idempotent to retry)
    /// from every other transport error (safe to retry).
    ///
    /// A connection-phase failure that never delivered the request body — a
    /// refused/reset connection or DNS failure — is [`VoyageError::Http`]; a
    /// request that timed out waiting for the response is [`VoyageError::Timeout`].
    /// A *connect* timeout also reports `is_timeout()`, so it is conservatively
    /// classed as a timeout too: giving up on the rare case where the server was
    /// merely slow to accept the connection is the safe trade against ever
    /// double-billing a batch the server did receive (issue #164).
    pub(crate) fn from_reqwest(e: &reqwest::Error) -> Self {
        if e.is_timeout() {
            Self::Timeout(e.to_string())
        } else {
            Self::Http(e.to_string())
        }
    }
}

/// Output from a successful embedding call.
#[derive(Debug, Clone)]
pub struct EmbedOutput {
    /// One embedding vector per input text, in the same order as the input.
    pub vectors: Vec<Vec<f32>>,
    /// Total tokens consumed by the request (for quota / cost tracking).
    pub total_tokens: u64,
    /// The model identifier echoed back by the API.
    pub model: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
struct EmbedRequest<'a> {
    model: &'a str,
    input: Vec<String>,
    input_type: &'a str,
    output_dimension: u32,
    output_dtype: &'a str,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
pub(crate) struct Usage {
    pub(crate) total_tokens: u64,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
    model: String,
    usage: Usage,
}

/// HTTP client for the VoyageAI embeddings API.
#[derive(Clone)]
pub struct VoyageEmbedder {
    client: reqwest::Client,
    api_key: String,
    model: String,
    /// Matryoshka output dimension sent as output_dimension.
    dim: u32,
    /// Output dtype sent as output_dtype (e.g. "float").
    dtype: String,
    base_url: String,
}

impl VoyageEmbedder {
    /// Create a new embedder.
    ///
    /// * `api_key` — Voyage API key (BYOK).
    /// * `model`   — e.g. `"voyage-code-3"`.
    /// * `dim`     — output dimension (e.g. `1024`).
    /// * `dtype`   — output dtype string (e.g. `"float"`).
    #[must_use]
    pub fn new(api_key: &str, model: &str, dim: u32, dtype: &str) -> Self {
        Self {
            client: voyage_http_client(DEFAULT_EMBED_TIMEOUT_SECS),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            dim,
            dtype: dtype.to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// Override the per-request timeout (seconds); rebuilds the inner client.
    #[must_use]
    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.client = voyage_http_client(secs);
        self
    }

    /// Override the base URL (for tests / local proxies).
    #[must_use]
    pub fn with_base_url(mut self, base: &str) -> Self {
        base.trim_end_matches('/').clone_into(&mut self.base_url);
        self
    }

    /// Embed a batch (≤1 000 texts / ≤120 K tokens per Voyage limits — caller batches).
    ///
    /// Returns vectors in the same order as `input`, regardless of how the API
    /// orders `data` items in the response.
    pub async fn embed(
        &self,
        input: Vec<String>,
        input_type: InputType,
    ) -> Result<EmbedOutput, VoyageError> {
        let input_len = input.len();
        let body = EmbedRequest {
            model: &self.model,
            input,
            input_type: input_type.as_str(),
            output_dimension: self.dim,
            output_dtype: &self.dtype,
        };
        let resp = self
            .client
            .post(format!("{}/v1/embeddings", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| VoyageError::from_reqwest(&e))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(VoyageError::Status { status: status.as_u16(), body });
        }

        let mut parsed: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| VoyageError::Decode(e.to_string()))?;

        if parsed.data.len() != input_len {
            return Err(VoyageError::Decode(format!(
                "expected {input_len} embeddings, got {}",
                parsed.data.len()
            )));
        }

        // Voyage may return data items out of order; sort by index to restore
        // the original input ordering before returning.
        parsed.data.sort_by_key(|d| d.index);

        Ok(EmbedOutput {
            vectors: parsed.data.into_iter().map(|d| d.embedding).collect(),
            total_tokens: parsed.usage.total_tokens,
            model: parsed.model,
        })
    }
}

/// Results from a Voyage `/v1/rerank` call.
#[derive(Debug, Clone)]
pub struct RerankOutput {
    /// Reranked results in the API's returned order (not re-sorted). When
    /// `top_k` is set, this holds at most `top_k` entries; each
    /// [`RerankResult.index`](crate::reranker::RerankResult) refers back into
    /// the original `documents` slice.
    pub results: Vec<crate::reranker::RerankResult>,
    /// Total tokens Voyage reported consuming.
    pub total_tokens: u64,
}

/// VoyageAI reranker client (`POST /v1/rerank`). Reranking stays client-side.
#[derive(Clone)]
pub struct VoyageReranker {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl VoyageReranker {
    /// Construct a reranker for `model` (e.g. `"rerank-2.5-lite"`) with the default base URL.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `reqwest::Client` cannot be built (e.g. the TLS
    /// backend fails to initialize) — the same failure mode as
    /// [`VoyageEmbedder::new`].
    #[must_use]
    pub fn new(api_key: &str, model: &str) -> Self {
        Self {
            client: voyage_http_client(30),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// Override the API base URL (trailing slash trimmed). For tests / proxies.
    #[must_use]
    pub fn with_base_url(mut self, base: &str) -> Self {
        base.trim_end_matches('/').clone_into(&mut self.base_url);
        self
    }

    /// Rerank `documents` against `query`; optional `top_k` caps the returned set.
    ///
    /// # Errors
    ///
    /// Returns [`VoyageError`] on transport failure, a non-2xx status, or a body
    /// that cannot be decoded.
    pub async fn rerank(
        &self,
        query: String,
        documents: Vec<String>,
        top_k: Option<usize>,
    ) -> Result<RerankOutput, VoyageError> {
        #[derive(Serialize)]
        struct Req<'a> {
            model: &'a str,
            query: String,
            documents: Vec<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            top_k: Option<usize>,
        }
        #[derive(Deserialize)]
        struct Data {
            relevance_score: f32,
            index: usize,
        }
        #[derive(Deserialize)]
        struct Resp {
            data: Vec<Data>,
            usage: Usage,
        }

        let resp = self
            .client
            .post(format!("{}/v1/rerank", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&Req {
                model: &self.model,
                query,
                documents,
                top_k,
            })
            .send()
            .await
            .map_err(|e| VoyageError::from_reqwest(&e))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(VoyageError::Status {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        let parsed: Resp = resp
            .json()
            .await
            .map_err(|e| VoyageError::Decode(e.to_string()))?;
        Ok(RerankOutput {
            results: parsed
                .data
                .into_iter()
                .map(|d| crate::reranker::RerankResult {
                    index: d.index,
                    score: d.relevance_score,
                })
                .collect(),
            total_tokens: parsed.usage.total_tokens,
        })
    }
}

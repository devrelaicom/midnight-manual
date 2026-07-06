//! VoyageAI contextualized embeddings client (`POST /v1/contextualizedembeddings`).
//!
//! Each inner input list is one document's chunks, embedded together so every
//! chunk vector carries document-level context (spec §4). A query is a
//! single-chunk document: `inputs = [[query]]`, `input_type = "query"`.

use serde::{Deserialize, Serialize};

use crate::voyage::{
    voyage_http_client, InputType, Usage, VoyageError, DEFAULT_BASE_URL, DEFAULT_EMBED_TIMEOUT_SECS,
};

/// Output of a contextualized group-embedding call.
#[derive(Debug, Clone)]
pub struct GroupEmbedOutput {
    /// One vector list per input group, vectors in chunk order.
    pub groups: Vec<Vec<Vec<f32>>>,
    /// Total tokens Voyage reported consuming.
    pub total_tokens: u64,
    /// The model identifier echoed back by the API.
    pub model: String,
}

#[derive(Serialize)]
struct CtxRequest<'a> {
    model: &'a str,
    inputs: &'a [Vec<String>],
    input_type: &'a str,
    output_dimension: u32,
    output_dtype: &'a str,
}

#[derive(Deserialize)]
struct CtxItem {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
struct CtxGroup {
    data: Vec<CtxItem>,
    index: usize,
}

#[derive(Deserialize)]
struct CtxResponse {
    data: Vec<CtxGroup>,
    model: String,
    usage: Usage,
}

/// HTTP client for the VoyageAI contextualized-embeddings API.
#[derive(Clone)]
pub struct ContextualizedVoyageEmbedder {
    client: reqwest::Client,
    api_key: String,
    model: String,
    dim: u32,
    dtype: String,
    base_url: String,
}

impl ContextualizedVoyageEmbedder {
    /// Create a new contextualized embedder (e.g. model `"voyage-context-3"`).
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

    /// Embed `groups` (one inner list per document; caller enforces the
    /// per-group 28 800-token budget and the per-request limits: ≤1 000
    /// inputs, ≤120 K tokens, ≤16 K chunks).
    ///
    /// Returns groups in input order with vectors in chunk order, regardless
    /// of how the API orders `data` items.
    ///
    /// # Errors
    /// [`VoyageError`] on transport failure, non-2xx status, or a response
    /// whose group/chunk counts don't match the request.
    pub async fn embed_groups(
        &self,
        groups: Vec<Vec<String>>,
        input_type: InputType,
    ) -> Result<GroupEmbedOutput, VoyageError> {
        let expected: Vec<usize> = groups.iter().map(Vec::len).collect();
        let body = CtxRequest {
            model: &self.model,
            inputs: &groups,
            input_type: input_type.as_str(),
            output_dimension: self.dim,
            output_dtype: &self.dtype,
        };
        let resp = self
            .client
            .post(format!("{}/v1/contextualizedembeddings", self.base_url))
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
        let mut parsed: CtxResponse = resp
            .json()
            .await
            .map_err(|e| VoyageError::Decode(e.to_string()))?;

        if parsed.data.len() != expected.len() {
            return Err(VoyageError::Decode(format!(
                "expected {} embedding groups, got {}",
                expected.len(),
                parsed.data.len()
            )));
        }
        parsed.data.sort_by_key(|g| g.index);
        let mut out_groups = Vec::with_capacity(parsed.data.len());
        for (gi, mut g) in parsed.data.into_iter().enumerate() {
            if g.data.len() != expected[gi] {
                return Err(VoyageError::Decode(format!(
                    "group {gi}: expected {} embeddings, got {}",
                    expected[gi],
                    g.data.len()
                )));
            }
            g.data.sort_by_key(|d| d.index);
            out_groups.push(g.data.into_iter().map(|d| d.embedding).collect());
        }
        Ok(GroupEmbedOutput {
            groups: out_groups,
            total_tokens: parsed.usage.total_tokens,
            model: parsed.model,
        })
    }

    /// Embed query texts, each as its own single-chunk document
    /// (`input_type = "query"`), returning one vector per text in order.
    ///
    /// # Errors
    /// See [`Self::embed_groups`].
    pub async fn embed_queries(
        &self,
        texts: Vec<String>,
    ) -> Result<crate::voyage::EmbedOutput, VoyageError> {
        let groups: Vec<Vec<String>> = texts.into_iter().map(|t| vec![t]).collect();
        let out = self.embed_groups(groups, InputType::Query).await?;
        let mut vectors = Vec::with_capacity(out.groups.len());
        for mut g in out.groups {
            let Some(v) = g.pop() else {
                return Err(VoyageError::Decode("empty query embedding group".into()));
            };
            vectors.push(v);
        }
        Ok(crate::voyage::EmbedOutput {
            vectors,
            total_tokens: out.total_tokens,
            model: out.model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ctx_response() -> serde_json::Value {
        // Two documents; items deliberately OUT of order at both levels to
        // pin the index-based reordering.
        serde_json::json!({
            "object": "list",
            "data": [
                { "object": "list", "index": 1, "data": [
                    { "object": "embedding", "index": 0, "embedding": [3.0, 3.0] }
                ]},
                { "object": "list", "index": 0, "data": [
                    { "object": "embedding", "index": 1, "embedding": [2.0, 2.0] },
                    { "object": "embedding", "index": 0, "embedding": [1.0, 1.0] }
                ]}
            ],
            "model": "voyage-context-3",
            "usage": { "total_tokens": 42 }
        })
    }

    #[tokio::test]
    async fn embeds_groups_and_restores_order() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/contextualizedembeddings"))
            .and(body_partial_json(serde_json::json!({
                "model": "voyage-context-3",
                "input_type": "document",
                "inputs": [["a", "b"], ["c"]],
                "output_dimension": 2,
                "output_dtype": "float",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(ctx_response()))
            .mount(&server)
            .await;

        let e = ContextualizedVoyageEmbedder::new("k", "voyage-context-3", 2, "float")
            .with_base_url(&server.uri());
        let out = e
            .embed_groups(vec![vec!["a".into(), "b".into()], vec!["c".into()]], InputType::Document)
            .await
            .unwrap();
        assert_eq!(out.total_tokens, 42);
        assert_eq!(out.groups, vec![vec![vec![1.0, 1.0], vec![2.0, 2.0]], vec![vec![3.0, 3.0]],]);
    }

    #[tokio::test]
    async fn query_embeds_as_single_chunk_document() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/contextualizedembeddings"))
            .and(body_partial_json(serde_json::json!({
                "input_type": "query",
                "inputs": [["how do I compile?"]],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [{ "object": "list", "index": 0, "data": [
                    { "object": "embedding", "index": 0, "embedding": [9.0, 9.0] }
                ]}],
                "model": "voyage-context-3",
                "usage": { "total_tokens": 5 }
            })))
            .mount(&server)
            .await;

        let e = ContextualizedVoyageEmbedder::new("k", "voyage-context-3", 2, "float")
            .with_base_url(&server.uri());
        let out = e
            .embed_queries(vec!["how do I compile?".into()])
            .await
            .unwrap();
        assert_eq!(out.vectors, vec![vec![9.0, 9.0]]);
        assert_eq!(out.total_tokens, 5);
    }

    #[tokio::test]
    async fn group_count_mismatch_is_a_decode_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/contextualizedembeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list", "data": [], "model": "voyage-context-3",
                "usage": { "total_tokens": 0 }
            })))
            .mount(&server)
            .await;
        let e = ContextualizedVoyageEmbedder::new("k", "voyage-context-3", 2, "float")
            .with_base_url(&server.uri());
        let err = e
            .embed_groups(vec![vec!["a".into()]], InputType::Document)
            .await
            .unwrap_err();
        assert!(matches!(err, VoyageError::Decode(_)));
    }
}

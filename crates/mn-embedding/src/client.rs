//! Client-side embedding resolution: BYOK (Voyage direct) or via our server's
//! `POST /v1/embeddings` endpoint.
//!
//! Shared by both the CLI (`mnm search` / `mnm ingest`) and the MCP server so
//! the "embed these texts, give me vectors + token usage" decision lives in one
//! place. The corpus is always encoded client-side; this just picks where the
//! Voyage call happens — directly with the caller's own key, or proxied through
//! the server (which holds the key and enforces token limits).

use serde::Deserialize;

use crate::voyage::{InputType, VoyageEmbedder, VoyageError};

/// Where to perform the embedding.
pub enum EmbedSource<'a> {
    /// Bring-your-own-key: call Voyage directly with the caller's embedder.
    Byok(&'a VoyageEmbedder),
    /// Proxy through our server's `/v1/embeddings` (server holds the key +
    /// enforces token limits). `bearer` carries an admin/read-uplift token
    /// when the caller has one.
    Server {
        /// Base URL of the `midnight-manual-server` deployment.
        base_url: &'a str,
        /// Optional bearer token for tier-based limits.
        bearer: Option<&'a str>,
    },
}

/// Vectors + the Voyage-reported token usage for an embedding request.
pub struct Embedded {
    /// One vector per input text, in input order.
    pub vectors: Vec<Vec<f32>>,
    /// Total tokens Voyage reported consuming.
    pub total_tokens: u64,
}

#[derive(Deserialize)]
struct ServerResp {
    embeddings: Vec<Vec<f32>>,
    usage: ServerUsage,
}
#[derive(Deserialize)]
struct ServerUsage {
    total_tokens: u64,
}

/// Embed `texts`, either directly via Voyage (BYOK) or through the server.
///
/// # Errors
///
/// Returns [`VoyageError`] on transport failure, a non-2xx response, or a
/// response body that fails to decode.
pub async fn embed(
    texts: Vec<String>,
    input_type: InputType,
    src: EmbedSource<'_>,
) -> Result<Embedded, VoyageError> {
    match src {
        EmbedSource::Byok(v) => {
            let out = v.embed(texts, input_type).await?;
            Ok(Embedded {
                vectors: out.vectors,
                total_tokens: out.total_tokens,
            })
        }
        EmbedSource::Server { base_url, bearer } => {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| VoyageError::Http(e.to_string()))?;
            let it = match input_type {
                InputType::Query => "query",
                InputType::Document => "document",
            };
            let mut rb = client
                .post(format!("{}/v1/embeddings", base_url.trim_end_matches('/')))
                .json(&serde_json::json!({ "input": texts, "input_type": it }));
            if let Some(b) = bearer {
                rb = rb.bearer_auth(b);
            }
            let resp = rb
                .send()
                .await
                .map_err(|e| VoyageError::Http(e.to_string()))?;
            let status = resp.status();
            if !status.is_success() {
                return Err(VoyageError::Status {
                    status: status.as_u16(),
                    body: resp.text().await.unwrap_or_default(),
                });
            }
            let parsed: ServerResp = resp
                .json()
                .await
                .map_err(|e| VoyageError::Decode(e.to_string()))?;
            Ok(Embedded {
                vectors: parsed.embeddings,
                total_tokens: parsed.usage.total_tokens,
            })
        }
    }
}

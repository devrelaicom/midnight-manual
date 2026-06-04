//! Client-side embedding resolution: BYOK (Voyage direct) or via our server's
//! `POST /v1/embeddings` endpoint.
//!
//! Shared by both the CLI (`mnm search` / `mnm ingest`) and the MCP server so
//! the "embed these texts, give me vectors + token usage" decision lives in one
//! place. The corpus is always encoded client-side; this just picks where the
//! Voyage call happens — directly with the caller's own key, or proxied through
//! the server (which holds the key and enforces token limits).

use serde::Deserialize;

use crate::voyage::{InputType, VoyageEmbedder, VoyageError, DEFAULT_EMBED_TIMEOUT_SECS};

/// Where to perform the embedding.
#[derive(Clone, Copy)]
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
        /// Admin-only opt-out from the server's site-wide token cap. The server
        /// honours it ONLY for admin-tier callers; for everyone else it is a
        /// no-op (the request is still counted). Send `false` for normal
        /// requests.
        no_global_limit: bool,
    },
}

/// Vectors + the Voyage-reported token usage for an embedding request.
#[derive(Debug)]
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

/// Max embedding attempts (1 initial + retries) before giving up.
const MAX_EMBED_ATTEMPTS: usize = 3;

/// Whether a failed embed is worth retrying. Transient transport errors
/// (connection drops — including the HTTP/2 stalls and free-tier throttling
/// Voyage exhibits under load), 429s, and 5xx are retryable; a 4xx (e.g. a 400
/// over-limit batch, or a 401 auth failure) and decode errors are permanent.
const fn is_retryable(e: &VoyageError) -> bool {
    match e {
        VoyageError::Http(_) => true,
        VoyageError::Status { status, .. } => *status == 429 || *status >= 500,
        VoyageError::Decode(_) => false,
    }
}

/// Embed `texts`, either directly via Voyage (BYOK) or through the server,
/// retrying transient failures with exponential backoff.
///
/// Voyage's HTTP/2 endpoint and free-tier throttling drop connections
/// intermittently under load (surfacing as transport errors); a bounded retry
/// lets a single dropped request recover instead of failing the whole ingest
/// run — which would otherwise re-embed every prior batch. Retries cover
/// transport errors, 429, and 5xx; never a 400 (e.g. an over-limit batch) or a
/// decode error (see `is_retryable`).
///
/// # Errors
///
/// Returns the last [`VoyageError`] if every attempt fails, or immediately on a
/// non-retryable error.
pub async fn embed(
    texts: Vec<String>,
    input_type: InputType,
    src: EmbedSource<'_>,
) -> Result<Embedded, VoyageError> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        match embed_once(texts.clone(), input_type, src).await {
            Ok(out) => return Ok(out),
            Err(e) if attempt < MAX_EMBED_ATTEMPTS && is_retryable(&e) => {
                let backoff = std::time::Duration::from_secs(1u64 << (attempt - 1));
                tracing::warn!(
                    attempt,
                    max = MAX_EMBED_ATTEMPTS,
                    backoff_secs = backoff.as_secs(),
                    error = %e,
                    "Voyage embed failed; retrying after backoff",
                );
                tokio::time::sleep(backoff).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// A single embedding attempt (no retry); [`embed`] wraps this with backoff.
async fn embed_once(
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
        EmbedSource::Server {
            base_url,
            bearer,
            no_global_limit,
        } => {
            // The server embeds via Voyage on our behalf; a large document batch
            // can take ~40s, so this client must allow at least as long as the
            // BYOK embedder (else proxy-mode ingest would hit the same 30s abort
            // the BYOK path was fixed for).
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(DEFAULT_EMBED_TIMEOUT_SECS))
                .build()
                .map_err(|e| VoyageError::Http(e.to_string()))?;
            let it = match input_type {
                InputType::Query => "query",
                InputType::Document => "document",
            };
            let mut rb = client
                .post(format!("{}/v1/embeddings", base_url.trim_end_matches('/')))
                .json(&serde_json::json!({
                    "input": texts,
                    "input_type": it,
                    "no_global_limit": no_global_limit,
                }));
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

#[cfg(test)]
mod tests {
    use super::is_retryable;
    use crate::voyage::VoyageError;

    #[test]
    fn classifies_retryable_errors() {
        assert!(is_retryable(&VoyageError::Http("connection reset".into())));
        assert!(is_retryable(&VoyageError::Status {
            status: 429,
            body: String::new()
        }));
        assert!(is_retryable(&VoyageError::Status {
            status: 500,
            body: String::new()
        }));
        assert!(is_retryable(&VoyageError::Status {
            status: 503,
            body: String::new()
        }));
        assert!(!is_retryable(&VoyageError::Status {
            status: 400,
            body: String::new()
        }));
        assert!(!is_retryable(&VoyageError::Status {
            status: 401,
            body: String::new()
        }));
        assert!(!is_retryable(&VoyageError::Decode("bad json".into())));
    }
}

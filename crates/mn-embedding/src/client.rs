//! Client-side embedding resolution: BYOK (Voyage direct) or via our server's
//! `POST /v1/embeddings` endpoint.
//!
//! Shared by both the CLI (`mnm search` / `mnm ingest`) and the MCP server so
//! the "embed these texts, give me vectors + token usage" decision lives in one
//! place. The corpus is always encoded client-side; this just picks where the
//! Voyage call happens — directly with the caller's own key, or proxied through
//! the server (which holds the key and enforces token limits).

use serde::Deserialize;

use crate::contextualized::ContextualizedVoyageEmbedder;
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

/// Where to perform a GENERAL (voyage-context-3) embedding.
#[derive(Clone, Copy)]
pub enum GeneralEmbedSource<'a> {
    /// BYOK: call the contextualized endpoint directly.
    Byok(&'a ContextualizedVoyageEmbedder),
    /// Proxy through our server's `/v1/embeddings` with `type=general`.
    Server {
        /// Base URL of the `midnight-manual-server` deployment.
        base_url: &'a str,
        /// Optional bearer token for tier-based limits.
        bearer: Option<&'a str>,
        /// Admin-only opt-out from the server's site-wide token cap.
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

/// Nested vectors + token usage for a group-embedding request.
#[derive(Debug)]
pub struct EmbeddedGroups {
    /// One vector list per input group, vectors in chunk order.
    pub groups: Vec<Vec<Vec<f32>>>,
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

#[derive(serde::Serialize)]
struct ServerEmbedBody<'a, I> {
    input: &'a I,
    input_type: &'a str,
    #[serde(rename = "type")]
    embed_type: &'a str,
    no_global_limit: bool,
}

/// POST one body to the server's `/v1/embeddings` and decode the flat reply.
async fn server_embed_once<I: serde::Serialize + Sync>(
    base_url: &str,
    bearer: Option<&str>,
    body: &ServerEmbedBody<'_, I>,
) -> Result<Embedded, VoyageError> {
    let client = crate::voyage::voyage_http_client(DEFAULT_EMBED_TIMEOUT_SECS);
    let mut req = client
        .post(format!("{}/v1/embeddings", base_url.trim_end_matches('/')))
        .json(body);
    if let Some(b) = bearer {
        req = req.bearer_auth(b);
    }
    let resp = req
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

/// Embed query/document texts with the GENERAL model (voyage-context-3),
/// each text as its own single-chunk document.
///
/// Retries transient failures with exponential backoff (transport errors,
/// 429, 5xx — see `is_retryable`).
///
/// # Errors
///
/// Returns the last [`VoyageError`] if every attempt fails, or immediately on a
/// non-retryable error.
pub async fn embed_general(
    texts: Vec<String>,
    input_type: InputType,
    src: GeneralEmbedSource<'_>,
) -> Result<Embedded, VoyageError> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = match src {
            GeneralEmbedSource::Byok(e) => match input_type {
                InputType::Query => e.embed_queries(texts.clone()).await.map(|o| Embedded {
                    vectors: o.vectors,
                    total_tokens: o.total_tokens,
                }),
                InputType::Document => {
                    let groups: Vec<Vec<String>> =
                        texts.clone().into_iter().map(|t| vec![t]).collect();
                    e.embed_groups(groups, InputType::Document)
                        .await
                        .map(|o| Embedded {
                            vectors: o.groups.into_iter().flatten().collect(),
                            total_tokens: o.total_tokens,
                        })
                }
            },
            GeneralEmbedSource::Server {
                base_url,
                bearer,
                no_global_limit,
            } => {
                server_embed_once(
                    base_url,
                    bearer,
                    &ServerEmbedBody {
                        input: &texts,
                        input_type: input_type.as_str(),
                        embed_type: "general",
                        no_global_limit,
                    },
                )
                .await
            }
        };
        match result {
            Ok(out) => return Ok(out),
            Err(e) if attempt < MAX_EMBED_ATTEMPTS && is_retryable(&e) => {
                backoff_sleep(attempt, &e).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Embed caller-provided context groups with the GENERAL model (ingest path).
///
/// BYOK hits the contextualized endpoint with nested inputs; server-proxy
/// sends nested `input` with `type=general` and re-nests the flat reply.
///
/// # Errors
///
/// Returns the last [`VoyageError`] if every attempt fails, or immediately on a
/// non-retryable error.
pub async fn embed_general_groups(
    groups: Vec<Vec<String>>,
    src: GeneralEmbedSource<'_>,
) -> Result<EmbeddedGroups, VoyageError> {
    let sizes: Vec<usize> = groups.iter().map(Vec::len).collect();
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = match src {
            GeneralEmbedSource::Byok(e) => e
                .embed_groups(groups.clone(), InputType::Document)
                .await
                .map(|o| EmbeddedGroups {
                    groups: o.groups,
                    total_tokens: o.total_tokens,
                }),
            GeneralEmbedSource::Server {
                base_url,
                bearer,
                no_global_limit,
            } => server_embed_once(
                base_url,
                bearer,
                &ServerEmbedBody {
                    input: &groups,
                    input_type: "document",
                    embed_type: "general",
                    no_global_limit,
                },
            )
            .await
            .and_then(|flat| renest(flat, &sizes)),
        };
        match result {
            Ok(out) => return Ok(out),
            Err(e) if attempt < MAX_EMBED_ATTEMPTS && is_retryable(&e) => {
                backoff_sleep(attempt, &e).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Embed texts with the CODE model (voyage-code-3, flat endpoint).
/// BYOK reuses the flat [`VoyageEmbedder`]; server-proxy sends `type=code`.
///
/// # Errors
///
/// Returns the last [`VoyageError`] if every attempt fails, or immediately on a
/// non-retryable error.
pub async fn embed_code(
    texts: Vec<String>,
    input_type: InputType,
    src: EmbedSource<'_>,
) -> Result<Embedded, VoyageError> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = match src {
            EmbedSource::Byok(v) => v.embed(texts.clone(), input_type).await.map(|o| Embedded {
                vectors: o.vectors,
                total_tokens: o.total_tokens,
            }),
            EmbedSource::Server {
                base_url,
                bearer,
                no_global_limit,
            } => {
                server_embed_once(
                    base_url,
                    bearer,
                    &ServerEmbedBody {
                        input: &texts,
                        input_type: input_type.as_str(),
                        embed_type: "code",
                        no_global_limit,
                    },
                )
                .await
            }
        };
        match result {
            Ok(out) => return Ok(out),
            Err(e) if attempt < MAX_EMBED_ATTEMPTS && is_retryable(&e) => {
                backoff_sleep(attempt, &e).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Re-nest a flat row-per-chunk reply into the caller's group sizes.
fn renest(flat: Embedded, sizes: &[usize]) -> Result<EmbeddedGroups, VoyageError> {
    let total: usize = sizes.iter().sum();
    if flat.vectors.len() != total {
        return Err(VoyageError::Decode(format!(
            "expected {total} vectors, got {}",
            flat.vectors.len()
        )));
    }
    let mut it = flat.vectors.into_iter();
    let groups = sizes
        .iter()
        .map(|&n| it.by_ref().take(n).collect())
        .collect();
    Ok(EmbeddedGroups {
        groups,
        total_tokens: flat.total_tokens,
    })
}

/// Log a retryable embed failure and sleep with exponential backoff.
async fn backoff_sleep(attempt: usize, e: &VoyageError) {
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

#[cfg(test)]
mod tests {
    use super::{embed_code, embed_general_groups, is_retryable, EmbedSource, GeneralEmbedSource};
    use crate::voyage::{InputType, VoyageError};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn embed_code_sends_type_code_to_server() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(body_partial_json(serde_json::json!({
                "type": "code", "input": ["fn main() {}"], "input_type": "query",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "voyage-code-3@1",
                "embeddings": [[1.0, 2.0]],
                "usage": { "total_tokens": 3 },
                "rate": { "hour": {"limit":1,"remaining":1,"reset_at":""},
                          "day":  {"limit":1,"remaining":1,"reset_at":""} },
            })))
            .mount(&server)
            .await;
        let out = embed_code(
            vec!["fn main() {}".into()],
            InputType::Query,
            EmbedSource::Server {
                base_url: &server.uri(),
                bearer: None,
                no_global_limit: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(out.vectors, vec![vec![1.0, 2.0]]);
        assert_eq!(out.total_tokens, 3);
    }

    #[tokio::test]
    async fn embed_general_groups_sends_nested_input_with_type_general() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(body_partial_json(serde_json::json!({
                "type": "general", "input": [["a", "b"], ["c"]], "input_type": "document",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "voyage-context-3@1",
                "embeddings": [[1.0], [2.0], [3.0]],
                "usage": { "total_tokens": 9 },
                "rate": { "hour": {"limit":1,"remaining":1,"reset_at":""},
                          "day":  {"limit":1,"remaining":1,"reset_at":""} },
            })))
            .mount(&server)
            .await;
        let out = embed_general_groups(
            vec![vec!["a".into(), "b".into()], vec!["c".into()]],
            GeneralEmbedSource::Server {
                base_url: &server.uri(),
                bearer: None,
                no_global_limit: false,
            },
        )
        .await
        .unwrap();
        // The server returns row-per-chunk in input order; the client re-nests
        // by group sizes.
        assert_eq!(out.groups, vec![vec![vec![1.0], vec![2.0]], vec![vec![3.0]]]);
        assert_eq!(out.total_tokens, 9);
    }

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

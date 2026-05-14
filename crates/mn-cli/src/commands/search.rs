//! `mnm search <query>` — quick ad-hoc retrieval from the command line
//! (Story 12 / FR-058 / FR-117 read path).
//!
//! Flow:
//!
//! 1. Resolve the cloud URL via the global precedence (`--server` > config >
//!    compiled-in default).
//!
//! 2. Resolve a bearer token — prefer admin > read-uplift > anonymous.
//!    Anonymous still works; the server's `/v1/search` is public and the
//!    bearer only affects rate-limit tier.
//!
//! 3. Load the local embedder (`bge-base-en-v1.5`) and encode the query.
//!    First-run cost is the ~100 MB model download; subsequent runs hit
//!    the on-disk cache and are fast.
//!
//! 4. `POST /v1/search` with the resulting `{text, vector}` pair.
//!
//! 5. Render the response — human table by default, single-line NDJSON when
//!    `--json` is set.
//!
//! Unlike the MCP `search` tool, this command does NOT run a local reranker.
//! Quick CLI queries trade the rerank quality boost for lower latency and a
//! smaller install footprint.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mn_core::auth_file::AuthFile;
use mn_retrieval::filters::SearchFilters;
use mn_telemetry::events::{CliCommandName, Component, EventPayload, Outcome};
use mn_telemetry::{Event, TelemetryClient};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Default embedding model the CLI declares to the server. The server 409s
/// `embedding_model_mismatch` if its active corpus model doesn't agree.
pub const DEFAULT_EMBEDDING_MODEL: &str = "bge-base-en-v1.5@1";

/// Args for `mnm search`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The query string.
    pub query: String,

    /// Maximum number of results. Capped server-side at 100.
    #[arg(long, default_value_t = 10)]
    pub limit: u32,

    /// Override the embedding-model wire id. Defaults to
    /// `bge-base-en-v1.5@1`; only override if your local model and the
    /// corpus's model are out of sync (run `mnm models pull` first).
    #[arg(long, default_value = DEFAULT_EMBEDDING_MODEL)]
    pub embedding_model: String,
}

/// Dispatch.
///
/// # Errors
///
/// Returns `anyhow::Error` when the embedder cannot be loaded, the cache dir
/// cannot be resolved, the HTTP round-trip fails, or the response can't be
/// decoded.
pub async fn run(
    args: Args,
    server_flag: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server_flag);
    let cache_env = mn_embedding::cache::StdEnv;
    let cache_dir = mn_embedding::cache::resolve(&cache_env).context(
        "could not resolve model cache dir; set MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME",
    )?;
    let cfg_env = mn_core::config::StdEnv;
    let auth_path = mn_core::paths::auth_file_path(&cfg_env);
    run_with_paths(
        args,
        &server_url,
        auth_path.as_deref(),
        &cache_dir,
        telemetry,
        cli_version,
        json,
    )
    .await
}

/// Path-explicit driver. Loads the local embedder, encodes the query, and
/// hands off to [`search_via_http`].
///
/// # Errors
///
/// See [`run`].
pub async fn run_with_paths(
    args: Args,
    server_url: &str,
    auth_path: Option<&Path>,
    cache_dir: &Path,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    if args.query.trim().is_empty() {
        return Err(anyhow!("query must not be empty"));
    }
    let started = Instant::now();

    let embedder = mn_embedding::embedder::global(cache_dir.to_path_buf())
        .await
        .context("load embedder (first run downloads ~100 MB)")?;
    let vectors = embedder
        .embed_blocking(vec![args.query.clone()], None)
        .await
        .context("encode query")?;
    let vector = vectors
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("embedder returned no vector"))?;

    let bearer = auth_path.and_then(resolve_best_bearer);
    let request = SearchRequest {
        queries: vec![QueryPair {
            text: args.query.clone(),
            vector,
        }],
        client_embedding_model: args.embedding_model.clone(),
        limit: args.limit,
        filters: SearchFilters::default(),
    };

    let result = search_via_http(server_url, bearer.as_deref(), &request, json).await;

    let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    let outcome = if result.is_ok() {
        Outcome::Ok
    } else {
        Outcome::Error
    };
    telemetry
        .emit(Event::new(
            Component::Cli,
            cli_version,
            EventPayload::CliCommand {
                command: CliCommandName::Search,
                duration_ms,
                outcome,
            },
        ))
        .await;
    result
}

/// POST `/v1/search` with the supplied request and render the response.
///
/// Exposed for integration testing without spinning up the local embedder
/// (model downloads make in-CI tests slow and flaky). Production callers
/// should usually go through [`run`] / [`run_with_paths`].
///
/// # Errors
///
/// Returns `anyhow::Error` on HTTP failure or on a non-success status from
/// the server. The error message strips long base64-y blobs before echoing
/// the server's body (FR-019).
pub async fn search_via_http(
    server_url: &str,
    bearer: Option<&str>,
    request: &SearchRequest,
    json: bool,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build HTTP client")?;
    let mut req = client.post(format!("{server_url}/v1/search")).json(request);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.context("POST /v1/search")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("{status} from /v1/search: {}", redact_token_like(&body)));
    }
    let parsed: SearchResponse = resp
        .json()
        .await
        .context("parse /v1/search response body")?;
    println!("{}", render(&request.queries[0].text, &parsed, json));
    Ok(())
}

fn resolve_best_bearer(auth_path: &Path) -> Option<String> {
    let file = AuthFile::read_optional(auth_path).ok().flatten()?;
    let now = OffsetDateTime::now_utc();
    file.active_admin_token(now)
        .or_else(|| file.active_read_uplift_token(now))
        .map(str::to_owned)
}

/// Strip any 32-char-or-longer run of base64-ish characters from `s`.
/// Catches a bearer that ended up embedded inside a JSON error body (e.g.
/// `"message":"see token=eyJ..."`) — the simple split-on-whitespace form
/// used elsewhere doesn't fire when punctuation glues the bearer to
/// surrounding tokens.
fn redact_token_like(s: &str) -> String {
    let is_b64 =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '=' | '+' | '/');
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |out: &mut String, run: &mut String| {
        if run.len() >= 32 {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in s.chars() {
        if is_b64(c) {
            run.push(c);
        } else {
            flush(&mut out, &mut run);
            out.push(c);
        }
    }
    flush(&mut out, &mut run);
    out
}

/// Outgoing search request body. Matches `SearchRequest` on the server side.
#[derive(Debug, Clone, Serialize)]
pub struct SearchRequest {
    /// Query pairs.
    pub queries: Vec<QueryPair>,
    /// Embedding model wire id the queries were encoded against.
    pub client_embedding_model: String,
    /// Maximum number of results.
    pub limit: u32,
    /// Per-result filters.
    pub filters: SearchFilters,
}

/// One {text, vector} pair.
#[derive(Debug, Clone, Serialize)]
pub struct QueryPair {
    /// Verbatim query text.
    pub text: String,
    /// Pre-computed embedding (768 dims for bge-base-en-v1.5).
    pub vector: Vec<f32>,
}

/// Search response shape. Matches `SearchResponse` on the server side, with
/// every field declared `#[serde(default)]` so server-side additions stay
/// additive without breaking older CLIs.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchResponse {
    /// Ordered list of matching chunks.
    #[serde(default)]
    pub results: Vec<SearchResult>,
    /// Optional metadata bag from the server — not rendered by the CLI today.
    #[serde(default)]
    pub search_metadata: Option<serde_json::Value>,
}

/// One result row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Chunk identifier.
    pub chunk_id: uuid::Uuid,
    /// Chunk content text.
    #[serde(default)]
    pub content: String,
    /// Owning document id (rendered as a context hint).
    #[serde(default)]
    pub document_id: Option<uuid::Uuid>,
    /// 0-indexed chunk position within the document.
    #[serde(default)]
    pub chunk_index: i32,
    /// Total chunks in the parent document.
    #[serde(default)]
    pub total_chunks: i32,
    /// Per-result score breakdown.
    #[serde(default)]
    pub scores: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct SearchOutput<'a> {
    action: &'a str,
    query: &'a str,
    result_count: usize,
    results: &'a [SearchResult],
}

fn render(query: &str, resp: &SearchResponse, json: bool) -> String {
    if json {
        let body = SearchOutput {
            action: "search",
            query,
            result_count: resp.results.len(),
            results: &resp.results,
        };
        return serde_json::to_string(&body).unwrap_or_default();
    }
    if resp.results.is_empty() {
        return format!("no results for `{query}`");
    }
    let mut out = String::new();
    let plural = if resp.results.len() == 1 { "" } else { "s" };
    let count = resp.results.len();
    writeln!(out, "{count} result{plural} for `{query}`:").ok();
    for (i, r) in resp.results.iter().enumerate() {
        let preview = preview_line(&r.content);
        let idx = i + 1;
        let chunk_idx = r.chunk_index + 1;
        let total = r.total_chunks.max(1);
        let chunk_id = r.chunk_id;
        writeln!(out, "  {idx}. chunk {chunk_idx}/{total} [{chunk_id}]").ok();
        writeln!(out, "     {preview}").ok();
    }
    out.trim_end().to_owned()
}

/// One-line summary of a chunk's text — first 120 chars on a single line.
fn preview_line(content: &str) -> String {
    let oneline: String = content
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let trimmed = oneline.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() <= 120 {
        trimmed
    } else {
        let head: String = trimmed.chars().take(117).collect();
        format!("{head}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_response(n: usize) -> SearchResponse {
        let total = i32::try_from(n).unwrap_or(i32::MAX);
        let results = (0..n)
            .map(|i| SearchResult {
                chunk_id: uuid::Uuid::from_u128(u128::try_from(i + 1).unwrap_or(1)),
                content: format!("This is result {i} body text."),
                document_id: Some(uuid::Uuid::from_u128(100 + u128::try_from(i).unwrap_or(0))),
                chunk_index: i32::try_from(i).unwrap_or(i32::MAX),
                total_chunks: total,
                scores: None,
            })
            .collect();
        SearchResponse { results, search_metadata: None }
    }

    #[test]
    fn human_output_lists_each_result() {
        let r = sample_response(2);
        let s = render("hello", &r, false);
        assert!(s.contains("2 results for `hello`"));
        assert!(s.contains("1. chunk 1/2"));
        assert!(s.contains("2. chunk 2/2"));
        assert!(s.contains("This is result 0 body text"));
    }

    #[test]
    fn human_output_handles_empty_results() {
        let r = SearchResponse {
            results: Vec::new(),
            search_metadata: None,
        };
        let s = render("nope", &r, false);
        assert_eq!(s, "no results for `nope`");
    }

    #[test]
    fn human_output_singular_for_one_result() {
        let r = sample_response(1);
        let s = render("q", &r, false);
        assert!(s.starts_with("1 result for"));
        assert!(!s.starts_with("1 results"));
    }

    #[test]
    fn json_output_stable_shape() {
        let r = sample_response(2);
        let s = render("hello", &r, true);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["action"], "search");
        assert_eq!(v["query"], "hello");
        assert_eq!(v["result_count"], 2);
        assert!(v["results"].is_array());
        assert_eq!(v["results"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn preview_collapses_whitespace_and_truncates() {
        let s = preview_line("line one\nline two\n   extra");
        assert_eq!(s, "line one line two extra");
        let long: String = "a ".repeat(200);
        let p = preview_line(&long);
        assert!(p.ends_with("..."));
        assert!(p.chars().count() <= 120);
    }

    #[test]
    fn redacts_long_alnum_blobs() {
        let body = "forbidden: eyJhbGciOiJIUzI1NiJ9.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let r = redact_token_like(body);
        assert!(r.contains("[redacted]"));
        assert!(!r.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn resolve_best_bearer_returns_none_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("auth.toml");
        assert!(resolve_best_bearer(&missing).is_none());
    }
}

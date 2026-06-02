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
//! 3. Embed every query via VoyageAI. Two modes:
//!    - **BYOK** (flag/env/config key present): call Voyage directly with the
//!      caller's own key via [`mn_embedding::client::EmbedSource::Byok`].
//!    - **Server-proxy** (no key): POST to the server's `/v1/embeddings`
//!      endpoint, which holds the platform key and enforces token limits.
//!
//!    The corpus active model is fetched from `GET /v1/models/active` to form
//!    the canonical wire id (`name@revision`) labelling the request.
//!
//! 4. `POST /v1/search` with the resulting `{text, vector}` pairs. With more
//!    than one query the server RRFs across them; the response's per-query and
//!    per-result diagnostics are surfaced in the rendered output.
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

/// Sentinel embedding model wire id used when `--embedding-model` is not
/// explicitly overridden. At runtime the CLI resolves the true corpus wire id
/// from `GET /v1/models/active`; this constant is only the `clap` default so
/// `args.embedding_model` has a value for the explicit-override comparison.
pub const DEFAULT_EMBEDDING_MODEL: &str = "auto";

/// Maximum number of queries the CLI will send in one request (matches the
/// server's hard ceiling).
const MAX_QUERIES: usize = 10;

/// Args for `mnm search`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The primary query string. Required unless `--queries-stdin` is set.
    pub query: Option<String>,

    /// Additional query texts for multi-query retrieval (HyDE / expansion /
    /// step-back). Repeatable: `--query "alt 1" --query "alt 2"`.
    #[arg(long = "query")]
    pub extra_queries: Vec<String>,

    /// Read a JSON document `{ "queries": ["...", ...] }` from stdin instead of
    /// passing query text as arguments. Mutually exclusive with the positional
    /// query and `--query`.
    #[arg(long)]
    pub queries_stdin: bool,

    /// Maximum number of results. Capped server-side at 100.
    #[arg(long, default_value_t = 10)]
    pub limit: u32,

    /// Override the embedding-model wire id sent with the search request.
    /// When omitted (or set to `"auto"`), the CLI fetches the corpus's active
    /// model from `GET /v1/models/active` and uses that wire id. Only set
    /// this explicitly when you need to pin a specific `name@revision`.
    #[arg(long, default_value = DEFAULT_EMBEDDING_MODEL)]
    pub embedding_model: String,
}

/// Dispatch.
///
/// # Errors
///
/// Returns `anyhow::Error` when the active model cannot be resolved, the
/// embedding call fails, the HTTP round-trip fails, or the response can't be
/// decoded.
pub async fn run(
    args: Args,
    server_flag: Option<&str>,
    config_path: Option<&Path>,
    voyage_api_key: Option<&str>,
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
        config_path,
        voyage_api_key,
        telemetry,
        cli_version,
        json,
    )
    .await
}

/// Path-explicit driver. Embeds the query via VoyageAI (BYOK or server-proxy),
/// resolves the corpus wire id, and hands off to [`search_via_http`].
///
/// The `cache_dir` parameter is retained for the local reranker path that the
/// `--rerank` flag will use (Task 9.4); the corpus embedder is now Voyage, so
/// it is currently unused in the body.
///
/// # Errors
///
/// See [`run`].
#[allow(clippy::too_many_arguments)]
pub async fn run_with_paths(
    args: Args,
    server_url: &str,
    auth_path: Option<&Path>,
    cache_dir: &Path,
    config_path: Option<&Path>,
    voyage_api_key: Option<&str>,
    telemetry: &TelemetryClient,
    cli_version: &str,
    json: bool,
) -> Result<()> {
    // Retained for the upcoming `--rerank` local-reranker path (Task 9.4).
    let _ = cache_dir;

    let texts = collect_query_texts(&args)?;
    let started = Instant::now();

    // Resolve bearer first — needed for both the server-embed path and the
    // subsequent /v1/search request.
    let bearer = auth_path.and_then(resolve_best_bearer);

    // Resolve the Voyage API key (flag > VOYAGE_API_KEY env > config). Honor the
    // caller's `--config` path so a key stored in a non-default config is found.
    let env = mn_core::config::StdEnv;
    let (cfg, _) = mn_core::config::Config::discover(config_path, &env).unwrap_or_default();
    let voyage_key = mn_core::config::resolve_voyage_api_key(voyage_api_key, &cfg.models, &env);

    let input_type = mn_embedding::voyage::InputType::Query;
    let embedded = if let Some(key) = voyage_key.as_deref() {
        let embedder = mn_embedding::voyage::VoyageEmbedder::new(
            key,
            &cfg.models.embedding,
            cfg.models.voyage_output_dimension,
            &cfg.models.voyage_output_dtype,
        );
        mn_embedding::client::embed(
            texts.clone(),
            input_type,
            mn_embedding::client::EmbedSource::Byok(&embedder),
        )
        .await
    } else {
        mn_embedding::client::embed(
            texts.clone(),
            input_type,
            mn_embedding::client::EmbedSource::Server {
                base_url: server_url,
                bearer: bearer.as_deref(),
            },
        )
        .await
    }
    .context("embed queries via Voyage")?;

    let vectors = embedded.vectors;
    if vectors.len() != texts.len() {
        return Err(anyhow!(
            "embedder returned {} vectors for {} queries",
            vectors.len(),
            texts.len()
        ));
    }

    // Resolve the corpus wire id. When the sentinel "auto" is present, fetch
    // the active model from the server so the wire id always matches the
    // active corpus model. If the caller supplied an explicit
    // --embedding-model override, honour it directly and skip the round-trip.
    let client_embedding_model = if args.embedding_model == DEFAULT_EMBEDDING_MODEL {
        let active = crate::commands::models::fetch_active(server_url)
            .await
            .context("resolve active corpus model")?;
        format!("{}@{}", active.name, active.revision)
    } else {
        args.embedding_model.clone()
    };

    let queries: Vec<QueryPair> = texts
        .into_iter()
        .zip(vectors)
        .map(|(text, vector)| QueryPair { text, vector })
        .collect();
    let request = SearchRequest {
        queries,
        client_embedding_model,
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
    let texts: Vec<String> = request.queries.iter().map(|q| q.text.clone()).collect();
    println!("{}", render(&texts, &parsed, json));
    Ok(())
}

/// Assemble the final list of query texts from the CLI args.
///
/// Either the positional query (plus any repeated `--query`) OR a stdin JSON
/// document `{ "queries": [...] }` — the two are mutually exclusive. Texts are
/// trimmed; empties are dropped; the result must be 1..=[`MAX_QUERIES`].
///
/// # Errors
///
/// Returns `anyhow::Error` when the forms are combined, stdin can't be read or
/// parsed, no non-empty query remains, or more than [`MAX_QUERIES`] are given.
fn collect_query_texts(args: &Args) -> Result<Vec<String>> {
    let raw = if args.queries_stdin {
        if args.query.is_some() || !args.extra_queries.is_empty() {
            return Err(anyhow!(
                "--queries-stdin cannot be combined with a positional query or --query"
            ));
        }
        read_queries_from_stdin(&mut std::io::stdin().lock())?
    } else {
        let primary = args.query.clone().ok_or_else(|| {
            anyhow!("a query is required (positional argument or --queries-stdin)")
        })?;
        let mut v = Vec::with_capacity(1 + args.extra_queries.len());
        v.push(primary);
        v.extend(args.extra_queries.iter().cloned());
        v
    };

    let texts: Vec<String> = raw
        .into_iter()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect();
    if texts.is_empty() {
        return Err(anyhow!("no non-empty query text provided"));
    }
    if texts.len() > MAX_QUERIES {
        return Err(anyhow!("at most {MAX_QUERIES} queries are allowed (got {})", texts.len()));
    }
    Ok(texts)
}

/// Parse a `{ "queries": ["...", ...] }` JSON document from `reader`.
///
/// # Errors
///
/// Returns `anyhow::Error` if the stream can't be read or isn't the expected
/// JSON shape.
fn read_queries_from_stdin(reader: &mut impl std::io::Read) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct StdinQueries {
        queries: Vec<String>,
    }
    let mut buf = String::new();
    reader.read_to_string(&mut buf).context("read stdin")?;
    let parsed: StdinQueries =
        serde_json::from_str(&buf).context("parse stdin as JSON {\"queries\": [\"...\", ...]}")?;
    Ok(parsed.queries)
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
    /// Pre-computed embedding; its dimension is set by the active corpus model
    /// (e.g. 1024 for voyage-code-3).
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
    queries: &'a [String],
    result_count: usize,
    results: &'a [SearchResult],
    search_metadata: &'a Option<serde_json::Value>,
}

fn render(queries: &[String], resp: &SearchResponse, json: bool) -> String {
    if json {
        let body = SearchOutput {
            action: "search",
            queries,
            result_count: resp.results.len(),
            results: &resp.results,
            search_metadata: &resp.search_metadata,
        };
        return serde_json::to_string(&body).unwrap_or_default();
    }
    let label = query_label(queries);
    if resp.results.is_empty() {
        return format!("no results for {label}");
    }
    let mut out = String::new();
    let plural = if resp.results.len() == 1 { "" } else { "s" };
    let count = resp.results.len();
    writeln!(out, "{count} result{plural} for {label}:").ok();
    for (i, r) in resp.results.iter().enumerate() {
        let preview = preview_line(&r.content);
        let idx = i + 1;
        let chunk_idx = r.chunk_index + 1;
        let total = r.total_chunks.max(1);
        let chunk_id = r.chunk_id;
        let score = result_score_suffix(r);
        writeln!(out, "  {idx}. chunk {chunk_idx}/{total} [{chunk_id}]{score}").ok();
        writeln!(out, "     {preview}").ok();
    }
    if let Some(diag) = diagnostics_block(queries, resp) {
        out.push('\n');
        out.push_str(&diag);
        out.push('\n');
    }
    out.trim_end().to_owned()
}

/// Human-readable label for the query set: a single backticked query, or a
/// count for multi-query requests.
fn query_label(queries: &[String]) -> String {
    match queries {
        [one] => format!("`{one}`"),
        _ => format!("{} queries", queries.len()),
    }
}

/// Trailing ` (rrf …, queries […])` annotation for a result, parsed
/// defensively from the server's `scores` bag (absent fields are skipped).
fn result_score_suffix(r: &SearchResult) -> String {
    let Some(scores) = r.scores.as_ref() else {
        return String::new();
    };
    let mut parts = Vec::new();
    if let Some(rrf) = scores.get("rrf_score").and_then(serde_json::Value::as_f64) {
        parts.push(format!("rrf {rrf:.4}"));
    }
    if let Some(mq) = scores
        .get("matched_queries")
        .and_then(serde_json::Value::as_array)
    {
        let idxs: Vec<String> = mq
            .iter()
            .filter_map(serde_json::Value::as_u64)
            .map(|n| n.to_string())
            .collect();
        if !idxs.is_empty() {
            parts.push(format!("queries [{}]", idxs.join(", ")));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  ({})", parts.join(", "))
    }
}

/// Per-query diagnostics block from `search_metadata.per_query` (FTS/vector
/// candidate counts + latencies, plus any de-duplication note). Returns `None`
/// when the server sent no per-query metadata.
fn diagnostics_block(queries: &[String], resp: &SearchResponse) -> Option<String> {
    let meta = resp.search_metadata.as_ref()?;
    let per_query = meta
        .get("per_query")
        .and_then(serde_json::Value::as_array)?;
    if per_query.is_empty() {
        return None;
    }
    let mut out = String::from("diagnostics:");
    if let Some(dups) = meta
        .get("deduplicated_count")
        .and_then(serde_json::Value::as_u64)
    {
        if dups > 0 {
            write!(out, "\n  {dups} duplicate quer{} dropped", if dups == 1 { "y" } else { "ies" })
                .ok();
        }
    }
    for rec in per_query {
        let qi = rec
            .get("query_index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        let fts_c = rec
            .get("fts_candidates")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let vec_c = rec
            .get("vector_candidates")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let fts_ms = rec
            .get("fts_latency_ms")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let vec_ms = rec
            .get("vector_latency_ms")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let text = queries.get(qi).map_or("", String::as_str);
        let label = preview_line(text);
        write!(
            out,
            "\n  query {qi} (`{label}`): {fts_c} fts ({fts_ms:.1} ms) + {vec_c} vec ({vec_ms:.1} ms)"
        )
        .ok();
    }
    Some(out)
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

    fn texts(qs: &[&str]) -> Vec<String> {
        qs.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn human_output_lists_each_result() {
        let r = sample_response(2);
        let s = render(&texts(&["hello"]), &r, false);
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
        let s = render(&texts(&["nope"]), &r, false);
        assert_eq!(s, "no results for `nope`");
    }

    #[test]
    fn human_output_singular_for_one_result() {
        let r = sample_response(1);
        let s = render(&texts(&["q"]), &r, false);
        assert!(s.starts_with("1 result for"));
        assert!(!s.starts_with("1 results"));
    }

    #[test]
    fn multi_query_label_uses_count() {
        let r = sample_response(2);
        let s = render(&texts(&["a", "b", "c"]), &r, false);
        assert!(s.contains("for 3 queries"), "got: {s}");
    }

    #[test]
    fn json_output_stable_shape() {
        let r = sample_response(2);
        let s = render(&texts(&["hello", "world"]), &r, true);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["action"], "search");
        assert_eq!(v["queries"][0], "hello");
        assert_eq!(v["queries"][1], "world");
        assert_eq!(v["result_count"], 2);
        assert!(v["results"].is_array());
        assert_eq!(v["results"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn human_output_renders_per_query_and_per_result_diagnostics() {
        let r = SearchResponse {
            results: vec![SearchResult {
                chunk_id: uuid::Uuid::from_u128(1),
                content: "body".to_owned(),
                document_id: None,
                chunk_index: 0,
                total_chunks: 1,
                scores: Some(serde_json::json!({
                    "rrf_score": 0.0312,
                    "matched_queries": [0, 1],
                })),
            }],
            search_metadata: Some(serde_json::json!({
                "per_query": [
                    { "query_index": 0, "fts_candidates": 5, "fts_latency_ms": 1.2,
                      "vector_candidates": 30, "vector_latency_ms": 3.4 },
                    { "query_index": 1, "fts_candidates": 0, "fts_latency_ms": 0.4,
                      "vector_candidates": 30, "vector_latency_ms": 2.9 },
                ],
                "deduplicated_count": 1,
            })),
        };
        let s = render(&texts(&["alpha", "beta"]), &r, false);
        // Per-result annotation.
        assert!(s.contains("rrf 0.0312"), "got: {s}");
        assert!(s.contains("queries [0, 1]"), "got: {s}");
        // Per-query diagnostics block.
        assert!(s.contains("diagnostics:"), "got: {s}");
        assert!(s.contains("query 0 (`alpha`): 5 fts"), "got: {s}");
        assert!(s.contains("query 1 (`beta`):"), "got: {s}");
        assert!(s.contains("1 duplicate query dropped"), "got: {s}");
    }

    #[test]
    fn reads_queries_from_stdin_json() {
        let mut c = std::io::Cursor::new(br#"{"queries": ["one", "two", "three"]}"#.to_vec());
        let got = read_queries_from_stdin(&mut c).unwrap();
        assert_eq!(got, texts(&["one", "two", "three"]));
    }

    #[test]
    fn stdin_rejects_non_object_json() {
        let mut c = std::io::Cursor::new(br#"["one", "two"]"#.to_vec());
        assert!(read_queries_from_stdin(&mut c).is_err());
    }

    fn args(query: Option<&str>, extra: &[&str], stdin: bool) -> Args {
        Args {
            query: query.map(str::to_owned),
            extra_queries: texts(extra),
            queries_stdin: stdin,
            limit: 10,
            embedding_model: DEFAULT_EMBEDDING_MODEL.to_owned(),
        }
    }

    #[test]
    fn collect_texts_combines_positional_and_extra() {
        let got = collect_query_texts(&args(Some("primary"), &["alt1", "alt2"], false)).unwrap();
        assert_eq!(got, texts(&["primary", "alt1", "alt2"]));
    }

    #[test]
    fn collect_texts_drops_empty_and_requires_one() {
        // whitespace-only is trimmed away; nothing left → error.
        assert!(collect_query_texts(&args(Some("   "), &[], false)).is_err());
        // missing positional without stdin → error.
        assert!(collect_query_texts(&args(None, &[], false)).is_err());
    }

    #[test]
    fn collect_texts_rejects_stdin_combined_with_args() {
        assert!(collect_query_texts(&args(Some("x"), &[], true)).is_err());
    }

    #[test]
    fn collect_texts_rejects_over_cap() {
        let many: Vec<&str> = vec!["x"; MAX_QUERIES]; // primary + MAX extras = MAX+1
        assert!(collect_query_texts(&args(Some("primary"), &many, false)).is_err());
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

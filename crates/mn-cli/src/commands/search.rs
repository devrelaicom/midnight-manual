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
//! 5. Optionally rerank. By default the CLI does NOT rerank — quick queries
//!    trade the quality boost for lower latency and a smaller install
//!    footprint. Pass `--rerank` to opt in: the response candidates are
//!    reranked client-side via the configured reranker catalog id
//!    (`--reranker` flag > `MIDNIGHT_MANUAL_RERANKER` env > config, default
//!    `bge-reranker-base`), the same catalog the MCP `search` tool uses.
//!
//! 6. Render the response — human table by default, single-line NDJSON when
//!    `--json` is set.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mn_core::auth_file::AuthFile;
use mn_core::config::ConfigEnv as _;
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

/// Candidate pool size requested from the cloud when `--rerank` is set (mirrors
/// the MCP `search` tool's constant of the same name). Reranking needs a pool
/// wider than the caller's `--limit` so the cross-encoder can *promote* a chunk
/// the cloud ranked below the cutoff — not merely reorder the caller's top-N.
/// The reranker truncates back to `--limit` after scoring; the server's
/// `/v1/search` accepts a `limit` up to 100, so 50 is within range.
const RERANK_FETCH: u32 = 50;

/// Args for `mnm search`.
// A clap `Args` struct naturally accumulates one bool per boolean flag
// (`--queries-stdin`, `--rerank`, `--no-deprecated`, `--verified`); these are
// independent CLI switches, not a state enum, so the >3-bools lint doesn't apply.
#[allow(clippy::struct_excessive_bools)]
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

    /// Rerank the results client-side with a cross-encoder before rendering.
    /// Off by default (lower latency, no model download); the MCP `search` tool
    /// reranks by default, this CLI is opt-in.
    #[arg(long)]
    pub rerank: bool,

    /// Reranker catalog id to use when `--rerank` is set (e.g.
    /// `bge-reranker-base`, `jina-reranker-v1-turbo-en`, `voyage-rerank-2.5-lite`,
    /// `custom`). Precedence: this flag > `MIDNIGHT_MANUAL_RERANKER` env >
    /// config `[models].reranker` (default `bge-reranker-base`). Ignored unless
    /// `--rerank` is set.
    #[arg(long)]
    pub reranker: Option<String>,

    /// Query mode: hybrid (default), vector, or fts.
    #[arg(long, default_value = "hybrid", value_parser = ["hybrid", "vector", "fts"])]
    pub mode: String,

    /// Restrict to these chunk kinds (markdown|code|plaintext). Repeatable.
    #[arg(long = "kind")]
    pub kind: Vec<String>,

    /// Restrict to these programming languages. Repeatable.
    #[arg(long = "language")]
    pub language: Vec<String>,

    /// Exclude these languages. Repeatable.
    #[arg(long = "exclude-language")]
    pub exclude_language: Vec<String>,

    /// Restrict to these tags. Repeatable.
    #[arg(long = "tag")]
    pub tag: Vec<String>,

    /// Exclude these tags. Repeatable.
    #[arg(long = "exclude-tag")]
    pub exclude_tag: Vec<String>,

    /// Match symbols as `kind:name` (either side optional, e.g. `circuit:` or
    /// `:deployContract`). Repeatable.
    #[arg(long = "symbol")]
    pub symbol: Vec<String>,

    /// Restrict to these source slugs. Repeatable.
    #[arg(long = "source")]
    pub source: Vec<String>,

    /// Restrict to these content types. Repeatable.
    #[arg(long = "content-type")]
    pub content_type: Vec<String>,

    /// Restrict to these attributions. Repeatable.
    #[arg(long = "attribution")]
    pub attribution: Vec<String>,

    /// Exclude deprecated content.
    #[arg(long = "no-deprecated")]
    pub no_deprecated: bool,

    /// Restrict to verified content.
    #[arg(long = "verified")]
    pub verified: bool,

    /// Only chunks ingested on/after this ISO date (YYYY-MM-DD).
    #[arg(long = "ingested-after")]
    pub ingested_after: Option<String>,

    /// Only chunks ingested on/before this ISO date (YYYY-MM-DD).
    #[arg(long = "ingested-before")]
    pub ingested_before: Option<String>,

    /// Minimum chunk token count.
    #[arg(long = "min-tokens")]
    pub min_tokens: Option<i64>,

    /// Maximum chunk token count.
    #[arg(long = "max-tokens")]
    pub max_tokens: Option<i64>,

    /// Full filter object as JSON (mutually exclusive with the granular filter
    /// flags).
    #[arg(
        long = "filter-json",
        conflicts_with_all = [
            "kind", "language", "exclude_language", "tag", "exclude_tag", "symbol",
            "source", "content_type", "attribution", "no_deprecated", "verified",
            "ingested_after", "ingested_before", "min_tokens", "max_tokens",
        ]
    )]
    pub filter_json: Option<String>,
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
/// resolves the corpus wire id, posts `/v1/search`, and — when `args.rerank` is
/// set — reranks the candidates client-side via the configured reranker catalog
/// id before rendering. `cache_dir` is the on-disk fastembed model cache used
/// by the local reranker variants.
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
                // Search never opts out of the global cap (read path, not ingest).
                no_global_limit: false,
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

    let client_embedding_model =
        resolve_client_embedding_model(&args.embedding_model, server_url).await?;

    // Resolve mode + filters from the granular flags (or --filter-json) and
    // fail fast on an invalid filter before any further work.
    let (mode, filters) = build_filters(&args)?;
    validate_filters(&filters)?;

    let queries: Vec<QueryPair> = texts
        .into_iter()
        .zip(vectors)
        .map(|(text, vector)| QueryPair { text, vector })
        .collect();
    let request = build_search_request(
        queries,
        client_embedding_model,
        args.limit,
        args.rerank,
        mode,
        filters,
    );

    // Resolve the env-dependent rerank selection up front (synchronously) into
    // owned data. The `ConfigEnv` trait carries no `Sync` guarantee, so threading
    // an `&impl ConfigEnv` borrow through the `.await` below would make this
    // future non-`Send` for arbitrary impls; resolving to owned values first keeps
    // `DispatchSearch` env-free and its future `Send`. Cheap when `--rerank` is
    // off (only a couple of lookups).
    let reranker_id =
        mn_core::config::resolve_reranker(args.reranker.as_deref(), &cfg.models, &env);
    let voyage_base_url = env
        .var("MIDNIGHT_MANUAL_VOYAGE_BASE_URL")
        .filter(|s| !s.is_empty());

    let result = dispatch_search(DispatchSearch {
        rerank: args.rerank,
        limit: args.limit,
        server_url,
        bearer: bearer.as_deref(),
        request: &request,
        reranker_id: &reranker_id,
        reranker_path: cfg.models.reranker_path.as_deref(),
        voyage_key: voyage_key.as_deref(),
        voyage_base_url: voyage_base_url.as_deref(),
        cache_dir,
        json,
    })
    .await;

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

/// Resolve the embedding-model wire id sent with the search request. When the
/// sentinel [`DEFAULT_EMBEDDING_MODEL`] (`"auto"`) is in effect, fetch the
/// corpus's active model from `GET /v1/models/active` so the wire id always
/// matches the active corpus model; an explicit `--embedding-model` override is
/// honoured verbatim and skips the round-trip.
///
/// # Errors
///
/// Returns `anyhow::Error` when the active-model fetch fails.
async fn resolve_client_embedding_model(embedding_model: &str, server_url: &str) -> Result<String> {
    if embedding_model == DEFAULT_EMBEDDING_MODEL {
        let active = crate::commands::models::fetch_active(server_url)
            .await
            .context("resolve active corpus model")?;
        Ok(format!("{}@{}", active.name, active.revision))
    } else {
        Ok(embedding_model.to_owned())
    }
}

/// Map the granular filter flags (or `--filter-json`) into a [`SearchFilters`],
/// returning it alongside the resolved `mode`.
///
/// `--filter-json` is mutually exclusive with the granular flags (enforced at
/// clap parse time); when present it is parsed directly and a malformed document
/// is a hard error — including a misspelled facet, which `SearchFilters`'
/// `deny_unknown_fields` rejects rather than silently dropping. A present-but-
/// unparseable `--ingested-after` / `--ingested-before` is likewise an error;
/// an absent date stays `None`. With no filter flags at all this yields
/// `SearchFilters::default()` (so `is_empty()` holds) and the clap-default
/// `mode` of `"hybrid"`.
///
/// # Errors
///
/// Returns `anyhow::Error` on malformed `--filter-json` or an unparseable
/// `--ingested-after` / `--ingested-before` date.
fn build_filters(args: &Args) -> Result<(String, mn_retrieval::filters::SearchFilters)> {
    use mn_retrieval::filters::{
        NumericRange, SearchFilters, SetMatch, SymbolMatch, TemporalRange,
    };
    if let Some(js) = &args.filter_json {
        let f: SearchFilters = serde_json::from_str(js)
            .context("parse --filter-json (see `mnm facets` for the filter shape)")?;
        return Ok((args.mode.clone(), f));
    }
    let set = |any_of: &[String], none_of: &[String]| SetMatch {
        any_of: any_of.to_vec(),
        none_of: none_of.to_vec(),
    };
    let symbols = args
        .symbol
        .iter()
        .map(|s| {
            let (k, n) = s.split_once(':').map_or((s.as_str(), ""), |(k, n)| (k, n));
            SymbolMatch {
                kind: if k.is_empty() {
                    None
                } else {
                    Some(k.to_owned())
                },
                name: if n.is_empty() {
                    None
                } else {
                    Some(n.to_owned())
                },
            }
        })
        .collect();
    let parse_date = |s: &Option<String>| -> Result<Option<time::Date>> {
        s.as_deref()
            .map(|d| {
                time::Date::parse(d, &time::format_description::well_known::Iso8601::DATE)
                    .with_context(|| format!("invalid ISO date `{d}` (expected YYYY-MM-DD)"))
            })
            .transpose()
    };
    let ingested = if args.ingested_after.is_some() || args.ingested_before.is_some() {
        Some(TemporalRange {
            after: parse_date(&args.ingested_after)?,
            before: parse_date(&args.ingested_before)?,
        })
    } else {
        None
    };
    let token_count =
        (args.min_tokens.is_some() || args.max_tokens.is_some()).then_some(NumericRange {
            min: args.min_tokens,
            max: args.max_tokens,
        });
    let f = SearchFilters {
        kind: set(&args.kind, &[]),
        language: set(&args.language, &args.exclude_language),
        tags: set(&args.tag, &args.exclude_tag),
        source_slug: set(&args.source, &[]),
        content_type: set(&args.content_type, &[]),
        attribution: set(&args.attribution, &[]),
        symbol: SetMatch {
            any_of: symbols,
            none_of: vec![],
        },
        deprecated: args.no_deprecated.then_some(false),
        verified: args.verified.then_some(true),
        ingested_at: ingested,
        token_count,
        ..Default::default()
    };
    Ok((args.mode.clone(), f))
}

/// Client-side fail-fast filter validation. Maps a [`mn_retrieval::filters::FilterError`]
/// to a friendly `anyhow::Error` that names the offending facet and points at
/// `mnm facets`, so an invalid filter is rejected before any embedding /
/// network work (rather than surfacing as an opaque server 400).
///
/// # Errors
///
/// Returns `anyhow::Error` when `filters.validate()` reports a violation.
fn validate_filters(filters: &mn_retrieval::filters::SearchFilters) -> Result<()> {
    if let Err(e) = filters.validate() {
        anyhow::bail!(
            "invalid filter `{}`: {} (see `mnm facets` for valid facets and values)",
            e.facet,
            e.message
        );
    }
    Ok(())
}

/// Build the outgoing `/v1/search` body, sizing the candidate pool for the
/// chosen path.
///
/// When `rerank` is set we widen the cloud `limit` to [`RERANK_FETCH`] and ask
/// for relevance order (`sort_by = "score"`) so the cross-encoder can *promote* a
/// chunk the cloud ranked below the caller's `limit` — not merely reorder the
/// caller's top-N (this mirrors the MCP `search` tool). `apply_rerank` later
/// truncates the reranked set back to `limit`. When `rerank` is off the body
/// carries the caller's `limit` and omits `sort_by` (`skip_serializing_if`), so
/// the wire body is byte-identical to the pre-Task-9.4 form.
fn build_search_request(
    queries: Vec<QueryPair>,
    client_embedding_model: String,
    limit: u32,
    rerank: bool,
    mode: String,
    filters: SearchFilters,
) -> SearchRequest {
    let (cloud_limit, sort_by) = if rerank {
        (RERANK_FETCH, Some("score".to_owned()))
    } else {
        (limit, None)
    };
    SearchRequest {
        queries,
        client_embedding_model,
        limit: cloud_limit,
        mode,
        filters,
        sort_by,
    }
}

/// Everything [`dispatch_search`] needs, already resolved off the `ConfigEnv`
/// (whose trait carries no `Sync` guarantee) into owned data so the async future
/// stays `Send`. Grouped into a struct to keep the function under the
/// argument-count lint.
struct DispatchSearch<'a> {
    /// Whether to run the client-side rerank pass.
    rerank: bool,
    /// Caller's result limit.
    limit: u32,
    /// Cloud base URL.
    server_url: &'a str,
    /// Bearer to forward (rate-limit tier; `/v1/search` is public).
    bearer: Option<&'a str>,
    /// The fully-formed search request.
    request: &'a SearchRequest,
    /// Resolved reranker catalog id (flag > env > config).
    reranker_id: &'a str,
    /// Backing dir for the `custom` catalog id.
    reranker_path: Option<&'a Path>,
    /// Voyage API key (required for Voyage catalog ids).
    voyage_key: Option<&'a str>,
    /// Resolved Voyage base-url override (self-host / proxy / test mock).
    voyage_base_url: Option<&'a str>,
    /// On-disk fastembed model cache.
    cache_dir: &'a Path,
    /// Render as NDJSON when set.
    json: bool,
}

/// Pick the search path based on `d.rerank`: the rerank path hands off to
/// [`rerank_via_http`] with the pre-resolved reranker selection; the no-rerank
/// path is the unchanged [`search_via_http`]. All env reads happen before this
/// `async fn` (see [`DispatchSearch`]) so its future is `Send`.
///
/// # Errors
///
/// Propagates the HTTP / decode / rerank-load errors from the chosen path.
async fn dispatch_search(d: DispatchSearch<'_>) -> Result<()> {
    if !d.rerank {
        return search_via_http(d.server_url, d.bearer, d.request, d.json).await;
    }
    rerank_via_http(
        d.server_url,
        d.bearer,
        d.request,
        RerankSelection {
            reranker_id: d.reranker_id,
            reranker_path: d.reranker_path,
            voyage_key: d.voyage_key,
            voyage_base_url: d.voyage_base_url,
            cache_dir: d.cache_dir,
        },
        d.limit,
        d.json,
    )
    .await
}

/// POST `/v1/search` with the supplied request and render the response.
///
/// Exposed for integration testing without spinning up the local embedder
/// (model downloads make in-CI tests slow and flaky). Production callers
/// should usually go through [`run`] / [`run_with_paths`].
///
/// This is the no-rerank path: it fetches via [`fetch_search`] then renders.
/// The signature and observable behaviour are unchanged from before Task 9.4
/// (the integration tests in `tests/search_integration.rs` depend on it).
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
    let resp = fetch_search(server_url, bearer, request).await?;
    let texts: Vec<String> = request.queries.iter().map(|q| q.text.clone()).collect();
    println!("{}", render(&texts, &resp, json));
    Ok(())
}

/// POST `/v1/search` and decode the response, WITHOUT rendering. The fetch +
/// decode half of [`search_via_http`], split out so the `--rerank` path can
/// post-process the decoded results before rendering. Used by both
/// [`search_via_http`] (no rerank) and the private `rerank_via_http` path.
///
/// # Errors
///
/// Returns `anyhow::Error` on HTTP failure or on a non-success status from the
/// server. The error message strips long base64-y blobs before echoing the
/// server's body (FR-019).
pub async fn fetch_search(
    server_url: &str,
    bearer: Option<&str>,
    request: &SearchRequest,
) -> Result<SearchResponse> {
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
    resp.json().await.context("parse /v1/search response body")
}

/// The reranker selection threaded into [`rerank_via_http`]. Grouped into a
/// struct so the function stays under the argument-count lint while carrying
/// everything [`mn_embedding::reranker::LoadedReranker::load`] needs.
struct RerankSelection<'a> {
    /// Catalog id (resolved flag > env > config).
    reranker_id: &'a str,
    /// Backing dir for the `custom` catalog id; `None` otherwise.
    reranker_path: Option<&'a Path>,
    /// Voyage API key (required for Voyage catalog ids).
    voyage_key: Option<&'a str>,
    /// Optional Voyage base-url override (self-host / proxy / test mock).
    voyage_base_url: Option<&'a str>,
    /// On-disk fastembed model cache.
    cache_dir: &'a Path,
}

/// `--rerank` path: fetch `/v1/search`, load the configured reranker, rerank
/// the candidates against the first query, re-order + truncate, then render.
///
/// This mirrors the MCP `search` tool: the `request` passed here was built with
/// a wider [`RERANK_FETCH`] candidate pool sorted by score (`sort_by =
/// "score"`), the candidates are reranked against the first query, and
/// [`apply_rerank`] truncates the reranked set back to the caller's `--limit`.
/// Candidates are mapped back by [`mn_embedding::RerankResult::index`] (NOT
/// positionally — a remote reranker like Voyage reorders), and each surviving
/// result is stamped with a `rerank_score` in its `scores` bag.
///
/// # Errors
///
/// Returns `anyhow::Error` on the HTTP fetch failure, a reranker load failure
/// (bad catalog id, missing model, missing Voyage key), or a rerank failure.
async fn rerank_via_http(
    server_url: &str,
    bearer: Option<&str>,
    request: &SearchRequest,
    sel: RerankSelection<'_>,
    limit: u32,
    json: bool,
) -> Result<()> {
    let resp = fetch_search(server_url, bearer, request).await?;
    let texts: Vec<String> = request.queries.iter().map(|q| q.text.clone()).collect();

    // Empty result set: nothing to rerank — render straight through.
    if resp.results.is_empty() {
        println!("{}", render(&texts, &resp, json));
        return Ok(());
    }

    let spec = mn_embedding::reranker_catalog::resolve(sel.reranker_id, sel.reranker_path)
        .with_context(|| format!("resolve reranker `{}`", sel.reranker_id))?;
    let reranker = mn_embedding::reranker::LoadedReranker::load(
        spec,
        sel.cache_dir.to_path_buf(),
        sel.voyage_key,
        sel.voyage_base_url,
    )
    .await
    .with_context(|| format!("load reranker `{}`", sel.reranker_id))?;

    // Pivot on the first query (the most "user-facing" text for HyDE / expansion).
    let pivot = texts.first().cloned().unwrap_or_default();
    let docs: Vec<String> = resp.results.iter().map(|r| r.content.clone()).collect();
    let scores = reranker
        .rerank(pivot, docs)
        .await
        .with_context(|| format!("rerank with `{}`", sel.reranker_id))?;

    let reordered = apply_rerank(resp.results, &scores, limit);
    let out = SearchResponse {
        results: reordered,
        search_metadata: resp.search_metadata,
    };
    println!("{}", render(&texts, &out, json));
    Ok(())
}

/// Re-order `results` by reranker score (descending) and truncate to `limit`,
/// mapping each score back to its result by [`mn_embedding::RerankResult::index`]
/// — NOT positionally, because a remote reranker (Voyage) may return results in
/// a different order than the input. A `rerank_score` is stamped into each
/// surviving result's `scores` JSON. Indices that fall outside `results` or
/// repeat are dropped defensively.
///
/// Pure (no model / IO) so it is unit-testable without a reranker or network.
fn apply_rerank(
    mut results: Vec<SearchResult>,
    scores: &[mn_embedding::RerankResult],
    limit: u32,
) -> Vec<SearchResult> {
    let mut seen = std::collections::HashSet::new();
    let mut indexed: Vec<(f32, SearchResult)> = scores
        .iter()
        .filter_map(|s| {
            if s.index >= results.len() || !seen.insert(s.index) {
                return None;
            }
            let mut taken = std::mem::take(&mut results[s.index]);
            stamp_rerank_score(&mut taken, s.score);
            Some((s.score, taken))
        })
        .collect();
    // total_cmp keeps a strict total order even if a NaN score sneaks in.
    indexed.sort_by(|a, b| b.0.total_cmp(&a.0));
    indexed.truncate(limit as usize);
    indexed.into_iter().map(|(_, r)| r).collect()
}

/// Record the raw reranker logit as `scores.rerank_score`, creating the
/// `scores` object if the server didn't send one.
fn stamp_rerank_score(result: &mut SearchResult, score: f32) {
    let scores = result
        .scores
        .get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(obj) = scores.as_object_mut() {
        obj.insert("rerank_score".to_owned(), serde_json::Value::from(score));
    }
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
#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchRequest {
    /// Query pairs.
    pub queries: Vec<QueryPair>,
    /// Embedding model wire id the queries were encoded against.
    pub client_embedding_model: String,
    /// Maximum number of results.
    pub limit: u32,
    /// Query mode (`hybrid` | `vector` | `fts`); serialized as the `mode` key,
    /// matching the cloud's snake_case `SearchMode` values.
    pub mode: String,
    /// Per-result filters.
    pub filters: SearchFilters,
    /// Optional ordering hint for the candidate pool. The rerank path sets this
    /// to `"score"` so the cloud returns relevance-ordered candidates (rather
    /// than its confidence-first default) before the cross-encoder reranks them.
    /// `None` for the non-rerank path — `skip_serializing_if` then omits the key
    /// entirely, keeping that wire body byte-identical to the pre-Task-9.4 form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
            rerank: false,
            reranker: None,
            mode: "hybrid".to_owned(),
            kind: Vec::new(),
            language: Vec::new(),
            exclude_language: Vec::new(),
            tag: Vec::new(),
            exclude_tag: Vec::new(),
            symbol: Vec::new(),
            source: Vec::new(),
            content_type: Vec::new(),
            attribution: Vec::new(),
            no_deprecated: false,
            verified: false,
            ingested_after: None,
            ingested_before: None,
            min_tokens: None,
            max_tokens: None,
            filter_json: None,
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

    fn result(chunk: u128, content: &str) -> SearchResult {
        SearchResult {
            chunk_id: uuid::Uuid::from_u128(chunk),
            content: content.to_owned(),
            ..SearchResult::default()
        }
    }

    #[test]
    fn apply_rerank_reorders_by_score_via_index_not_position() {
        // Three results in input order a/b/c. The reranker returns scores keyed
        // by the ORIGINAL index, out of order, with b most relevant. The remap
        // must be index-based: if it zipped positionally instead, the scores
        // would be misattributed and the order would be wrong.
        let results = vec![result(1, "a"), result(2, "b"), result(3, "c")];
        let scores = vec![
            mn_embedding::RerankResult { index: 2, score: 0.20 }, // c
            mn_embedding::RerankResult { index: 0, score: 0.10 }, // a
            mn_embedding::RerankResult { index: 1, score: 0.95 }, // b — most relevant
        ];
        let out = apply_rerank(results, &scores, 10);
        let order: Vec<&str> = out.iter().map(|r| r.content.as_str()).collect();
        assert_eq!(order, vec!["b", "c", "a"], "must sort by score desc, mapped by index");
        // b kept its own content despite arriving second in `scores`.
        assert_eq!(out[0].content, "b");
        // rerank_score stamped, and it matches b's score (0.95), proving the
        // score was attributed to the right result by index.
        let top_score = out[0].scores.as_ref().unwrap()["rerank_score"]
            .as_f64()
            .unwrap();
        assert!((top_score - 0.95).abs() < 1e-6, "top rerank_score was {top_score}");
    }

    #[test]
    fn build_request_rerank_widens_pool_and_sets_score_sort() {
        let q = vec![QueryPair {
            text: "x".to_owned(),
            vector: vec![0.0],
        }];
        let req = build_search_request(
            q,
            "voyage-code-3@1".to_owned(),
            5,
            true,
            "hybrid".to_owned(),
            SearchFilters::default(),
        );
        // Caller asked for 5 but the cloud pool is widened so the reranker can
        // promote a below-cutoff candidate.
        assert_eq!(req.limit, RERANK_FETCH);
        assert_eq!(req.sort_by.as_deref(), Some("score"));
        // sort_by serializes as the "score" key when present.
        let body = serde_json::to_value(&req).unwrap();
        assert_eq!(body["limit"], RERANK_FETCH);
        assert_eq!(body["sort_by"], "score");
    }

    #[test]
    fn build_request_non_rerank_keeps_limit_and_omits_sort_by() {
        let q = vec![QueryPair {
            text: "x".to_owned(),
            vector: vec![0.0],
        }];
        let req = build_search_request(
            q,
            "voyage-code-3@1".to_owned(),
            5,
            false,
            "hybrid".to_owned(),
            SearchFilters::default(),
        );
        assert_eq!(req.limit, 5);
        assert!(req.sort_by.is_none());
        // None must be OMITTED on the wire (skip_serializing_if), not null —
        // proving the non-rerank body stays byte-identical to pre-Task-9.4.
        let body = serde_json::to_value(&req).unwrap();
        assert_eq!(body["limit"], 5);
        assert!(
            body.as_object().unwrap().get("sort_by").is_none(),
            "sort_by key must be absent for the non-rerank path"
        );
    }

    #[test]
    fn apply_rerank_truncates_to_limit() {
        let results = vec![result(1, "a"), result(2, "b"), result(3, "c")];
        let scores = vec![
            mn_embedding::RerankResult { index: 0, score: 0.1 },
            mn_embedding::RerankResult { index: 1, score: 0.9 },
            mn_embedding::RerankResult { index: 2, score: 0.5 },
        ];
        let out = apply_rerank(results, &scores, 2);
        assert_eq!(out.len(), 2);
        // Top two by score: b (0.9) then c (0.5); a (0.1) is dropped.
        assert_eq!(out[0].content, "b");
        assert_eq!(out[1].content, "c");
    }

    #[test]
    fn apply_rerank_drops_out_of_range_and_duplicate_indices() {
        let results = vec![result(1, "a"), result(2, "b")];
        let scores = vec![
            mn_embedding::RerankResult { index: 0, score: 0.5 },
            mn_embedding::RerankResult { index: 5, score: 0.9 }, // out of range — dropped
            mn_embedding::RerankResult { index: 0, score: 0.8 }, // duplicate — dropped
            mn_embedding::RerankResult { index: 1, score: 0.3 },
        ];
        let out = apply_rerank(results, &scores, 10);
        assert_eq!(out.len(), 2, "only the two valid, unique indices survive");
        assert_eq!(out[0].content, "a"); // 0.5 > 0.3
        assert_eq!(out[1].content, "b");
    }

    #[test]
    fn apply_rerank_stamps_score_creating_scores_object() {
        // A result with no `scores` from the server still gains a scores object
        // carrying rerank_score.
        let results = vec![result(1, "a")];
        let scores = vec![mn_embedding::RerankResult { index: 0, score: 1.25 }];
        let out = apply_rerank(results, &scores, 10);
        assert!(out[0].scores.is_some(), "scores object must be created");
        assert!(out[0].scores.as_ref().unwrap()["rerank_score"].is_number());
    }

    #[test]
    fn search_args_rerank_flag_defaults_false_and_reranker_parses() {
        use clap::Parser as _;

        #[derive(clap::Parser)]
        struct Probe {
            #[command(flatten)]
            inner: Args,
        }

        // Default: --rerank absent → false, --reranker absent → None.
        let p = Probe::parse_from(["search", "q"]);
        assert!(!p.inner.rerank, "--rerank must default to false (opt-in)");
        assert!(p.inner.reranker.is_none());

        // Present: both flags parse.
        let p = Probe::parse_from([
            "search",
            "q",
            "--rerank",
            "--reranker",
            "voyage-rerank-2.5-lite",
        ]);
        assert!(p.inner.rerank);
        assert_eq!(p.inner.reranker.as_deref(), Some("voyage-rerank-2.5-lite"));
    }

    #[test]
    fn flags_map_to_filters_and_mode() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Probe {
            #[command(flatten)]
            inner: Args,
        }
        let p = Probe::parse_from([
            "search",
            "q",
            "--mode",
            "fts",
            "--kind",
            "code",
            "--language",
            "compact",
            "--exclude-language",
            "typescript",
            "--tag",
            "quickstart",
            "--symbol",
            "circuit:deployContract",
            "--no-deprecated",
            "--min-tokens",
            "50",
        ]);
        let (mode, filters) = build_filters(&p.inner).expect("valid flags");
        assert_eq!(mode, "fts");
        assert_eq!(filters.kind.any_of, vec!["code".to_owned()]);
        assert_eq!(filters.language.none_of, vec!["typescript".to_owned()]);
        assert_eq!(filters.symbol.any_of[0].kind.as_deref(), Some("circuit"));
        assert_eq!(filters.symbol.any_of[0].name.as_deref(), Some("deployContract"));
        assert_eq!(filters.deprecated, Some(false));
        assert_eq!(filters.token_count.unwrap().min, Some(50));
    }

    #[test]
    fn build_filters_rejects_bad_filter_json_and_dates() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Probe {
            #[command(flatten)]
            inner: Args,
        }
        let bad_json = Probe::parse_from(["search", "q", "--filter-json", "{ not valid json"]);
        assert!(build_filters(&bad_json.inner).is_err());
        let bad_date = Probe::parse_from(["search", "q", "--ingested-after", "not-a-date"]);
        assert!(build_filters(&bad_date.inner).is_err());
    }
}

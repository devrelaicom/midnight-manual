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
//! 3. Embed every query via VoyageAI — with the GENERAL (contextualized)
//!    model unless `--code-mode exclusive`, and additionally with the CODE
//!    model unless `--code-mode off`. The effective need mirrors the server's
//!    `code_mode` defaults (D6: explicit parameter, never query sniffing);
//!    `--mode fts` embeds nothing. Each embedding runs in one of two modes:
//!    - **BYOK** (flag/env/config key present): call Voyage directly with the
//!      caller's own key via [`mnm_embedding::client::EmbedSource::Byok`] /
//!      [`mnm_embedding::client::GeneralEmbedSource::Byok`].
//!    - **Server-proxy** (no key): POST to the server's `/v1/embeddings`
//!      endpoint with the matching `type` tag (`general` | `code`); the server
//!      holds the platform key and enforces token limits.
//!
//!    The corpus active models are fetched from `GET /v1/models/active` to
//!    form the canonical wire ids (`name@revision`) labelling the request.
//!
//! 4. `POST /v1/search` with the resulting `{text, vector, code_vector}` pairs
//!    (plus `code_mode` when the flag was given). With more than one query the
//!    server RRFs across them; the response's per-query and per-result
//!    diagnostics are surfaced in the rendered output.
//!
//! 5. Rerank according to the resolved placement (`--rerank` flag >
//!    `MIDNIGHT_MANUAL_RERANK` env > config `[rerank].location`, default
//!    `auto`). `auto` picks local when a Voyage key is present, else server.
//!    Local/off always send `rerank: "none"` to the server, so there is
//!    exactly one rerank pass regardless of placement:
//!    - **local**: candidates are reranked client-side via `VoyageAI`'s
//!      `/v1/rerank` with the caller's own key; the cloud pool is over-fetched
//!      to `RERANK_FETCH`.
//!    - **server**: the server reranks inline in `/v1/search` with the
//!      resolved `--rerank-model` and instructions.
//!    - **off**: no reranking anywhere.
//!
//! 6. Render the response — human table by default, single-line NDJSON when
//!    `--json` is set.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Context as _, Result};
use clap::Args as ClapArgs;
use mnm_core::auth_file::AuthFile;
use mnm_core::config::ConfigEnv as _;
use mnm_retrieval::filters::SearchFilters;
use mnm_telemetry::events::{CliCommandName, Component, EventPayload, Outcome};
use mnm_telemetry::{Event, TelemetryClient};
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

/// Candidate pool size requested from the cloud when reranking locally (mirrors
/// the MCP `search` tool's constant of the same name). Local reranking needs a
/// pool wider than the caller's `--limit` so the reranker can *promote* a chunk
/// the cloud ranked below the cutoff — not merely reorder the caller's top-N.
/// The reranker truncates back to `--limit` after scoring; the server's
/// `/v1/search` accepts a `limit` up to 100, so 50 is within range.
const RERANK_FETCH: u32 = 50;

/// Args for `mnm search`.
// A clap `Args` struct naturally accumulates one bool per boolean flag
// (`--queries-stdin`, `--no-deprecated`, `--verified`); these are independent
// CLI switches, not a state enum, so the >3-bools lint doesn't apply.
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

    /// Where reranking runs: auto (default; local with a Voyage key, else
    /// server), local (BYOK Voyage), server, or off.
    #[arg(long, default_value = "auto", value_parser = ["auto", "local", "server", "off"])]
    pub rerank: String,

    /// Voyage rerank model. rerank-2.5-lite is faster and billed at half rate
    /// server-side. Precedence: this flag > MIDNIGHT_MANUAL_RERANK_MODEL env >
    /// config `[rerank].model`.
    #[arg(long = "rerank-model", value_parser = ["rerank-2.5", "rerank-2.5-lite"])]
    pub rerank_model: Option<String>,

    /// Natural-language rerank instruction (max 400 chars). Replaces the
    /// derived default. Keep terse — instruction tokens multiply by pool size.
    #[arg(long = "rerank-instructions")]
    pub rerank_instructions: Option<String>,

    /// Query mode: hybrid (default), vector, or fts.
    #[arg(long, default_value = "hybrid", value_parser = ["hybrid", "vector", "fts"])]
    pub mode: String,

    /// Version-filter semantics: permissive (default) biases ranking; strict
    /// hard-filters. Only meaningful with a version-bearing --filter-json.
    #[arg(long, value_parser = ["strict", "permissive"])]
    pub version_match: Option<String>,

    /// Code-vector fusion mode: on (default for hybrid/vector), off, or
    /// exclusive (code vectors replace the general vector list). Incompatible
    /// with --mode fts.
    #[arg(long = "code-mode", value_parser = ["on", "off", "exclusive"])]
    pub code_mode: Option<String>,

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
    let cfg_env = mnm_core::config::StdEnv;
    let auth_path = mnm_core::paths::auth_file_path(&cfg_env);
    run_with_paths(
        args,
        &server_url,
        auth_path.as_deref(),
        config_path,
        voyage_api_key,
        telemetry,
        cli_version,
        json,
    )
    .await
}

/// Path-explicit driver. Embeds the query via VoyageAI (BYOK or server-proxy)
/// with the general and/or code model as the effective mode/`--code-mode`
/// requires, resolves the corpus wire ids, posts `/v1/search`, and — when the
/// resolved placement is `local` — reranks the candidates client-side via
/// `VoyageAI`'s `/v1/rerank` (BYOK) before rendering.
///
/// # Errors
///
/// See [`run`].
#[allow(clippy::too_many_arguments)]
pub async fn run_with_paths(
    args: Args,
    server_url: &str,
    auth_path: Option<&Path>,
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
    let env = mnm_core::config::StdEnv;
    let (cfg, _) = mnm_core::config::Config::discover(config_path, &env).unwrap_or_default();
    let voyage_key = mnm_core::config::resolve_voyage_api_key(voyage_api_key, &cfg.models, &env);

    // Resolve rerank placement + model and validate the instruction up front, so
    // a bad placement/key/instruction fails fast — before any embedding / network
    // work.
    let (placement, rerank_model) = resolve_rerank(&args, &cfg.rerank, &env, voyage_key.is_some())?;

    // Resolve mode + filters from the granular flags (or --filter-json) and
    // fail fast on an invalid filter before any embedding / network work.
    let (mode, filters) = build_filters(&args)?;
    validate_filters(&filters)?;

    // Which query embeddings this request needs — mirrors the server's
    // code_mode defaults client-side (D6: explicit parameter, never query
    // sniffing). The raw `args.code_mode` is still sent on the wire, so an
    // invalid combination (fts + on/exclusive) gets the server's 400 message.
    let (embed_general_query, embed_code_query) =
        query_embed_needs(&args.mode, args.code_mode.as_deref());

    // Fetch the corpus's active model FIRST so the embedders below are built
    // from the SAME source that labels the resulting vectors (cross-element
    // drift fix) — config is only a logged fallback when the fetch is
    // unavailable. One round-trip covers both the embedder identities and the
    // wire-id labels; it's skipped only when no embedding and no auto wire id
    // is needed.
    let (general_id, code_id, client_embedding_model, client_code_embedding_model) =
        resolve_active_identities(
            &args.embedding_model,
            &cfg.models,
            embed_general_query,
            embed_code_query,
            server_url,
        )
        .await?;

    let general_vectors = if embed_general_query {
        embed_general_queries(
            &texts,
            voyage_key.as_deref(),
            &general_id,
            server_url,
            bearer.as_deref(),
        )
        .await?
    } else {
        vec![Vec::new(); texts.len()]
    };
    let code_vectors = if embed_code_query {
        embed_code_queries(&texts, voyage_key.as_deref(), &code_id, server_url, bearer.as_deref())
            .await?
    } else {
        vec![Vec::new(); texts.len()]
    };

    let queries: Vec<QueryPair> = texts
        .into_iter()
        .zip(general_vectors)
        .zip(code_vectors)
        .map(|((text, vector), code_vector)| QueryPair { text, vector, code_vector })
        .collect();
    let request = build_search_request(SearchRequestParts {
        queries,
        client_embedding_model,
        client_code_embedding_model,
        limit: args.limit,
        placement,
        rerank_model,
        rerank_instructions: args.rerank_instructions.clone(),
        mode,
        code_mode: args.code_mode.clone(),
        version_match: args.version_match.clone(),
        filters,
    });

    // Resolve the env-dependent Voyage base-url override up front (synchronously)
    // into owned data. The `ConfigEnv` trait carries no `Sync` guarantee, so
    // threading an `&impl ConfigEnv` borrow through the `.await` below would make
    // this future non-`Send` for arbitrary impls; resolving to an owned value
    // first keeps `DispatchSearch` env-free and its future `Send`.
    let voyage_base_url = env
        .var("MIDNIGHT_MANUAL_VOYAGE_BASE_URL")
        .filter(|s| !s.is_empty());

    let result = dispatch_search(DispatchSearch {
        placement,
        limit: args.limit,
        server_url,
        bearer: bearer.as_deref(),
        request: &request,
        rerank_model,
        rerank_instructions: args.rerank_instructions.as_deref(),
        voyage_key: voyage_key.as_deref(),
        voyage_base_url: voyage_base_url.as_deref(),
        json,
    })
    .await;

    let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    emit_search_telemetry(EmitSearchTelemetry {
        telemetry,
        cli_version,
        placement,
        rerank_model,
        outcome: result.as_ref().ok(),
        ok: result.is_ok(),
        duration_ms,
    })
    .await;
    result.map(|_| ())
}

/// Inputs to [`emit_search_telemetry`] — grouped to keep the call free of a long
/// positional argument list.
struct EmitSearchTelemetry<'a> {
    /// Telemetry sink (the three-mechanism opt-out wraps each `emit`).
    telemetry: &'a TelemetryClient,
    /// Reporting component version for the event envelope.
    cli_version: &'a str,
    /// Resolved rerank placement (drives the `Rerank` event's wire fields).
    placement: mnm_core::config::RerankPlacement,
    /// Resolved Voyage rerank model.
    rerank_model: mnm_core::rerank::RerankParam,
    /// The search's [`RerankOutcome`] on success; `None` when the search failed
    /// before the rerank completed (reported as not applied).
    outcome: Option<&'a RerankOutcome>,
    /// Whether the overall search succeeded (the `CliCommand` outcome).
    ok: bool,
    /// End-to-end search duration in milliseconds.
    duration_ms: u32,
}

/// Emit the two FR-109 events for one search (spec §6): the `Rerank` event and
/// the `CliCommand` event. Factored out of [`run_with_paths`] so the dispatch
/// body stays focused on the request round-trip.
async fn emit_search_telemetry(args: EmitSearchTelemetry<'_>) {
    let EmitSearchTelemetry {
        telemetry,
        cli_version,
        placement,
        rerank_model,
        outcome,
        ok,
        duration_ms,
    } = args;
    // One `Rerank` event per search. On success the placement-specific facts come
    // from the `RerankOutcome`; on failure the rerank never completed, so it is
    // reported as not applied (the three-mechanism opt-out wraps each `emit`).
    let rerank_payload = rerank_event(placement, rerank_model, outcome);
    telemetry
        .emit(Event::new(Component::Cli, cli_version, rerank_payload))
        .await;
    let outcome = if ok { Outcome::Ok } else { Outcome::Error };
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
}

/// The rerank model name for the `Rerank` event: the resolved model on the
/// `Local` / `Server` placements, `None` on `Off` (no rerank attempted).
fn rerank_event_model(
    placement: mnm_core::config::RerankPlacement,
    rerank_model: mnm_core::rerank::RerankParam,
) -> Option<String> {
    use mnm_core::config::RerankPlacement;
    match placement {
        RerankPlacement::Local | RerankPlacement::Server => {
            rerank_model.model_name().map(str::to_owned)
        }
        RerankPlacement::Off => None,
    }
}

/// Build the FR-109 `Rerank` event payload (spec §6) for one search. The
/// placement / model come from the resolved request; `applied` / `reason` /
/// `billed_tokens` come from the [`RerankOutcome`] on success, or are reported
/// as "not applied" when the search failed before the rerank completed. The
/// placement wire string comes from the shared
/// [`mnm_core::config::RerankPlacement::wire`] (one source of truth across the
/// CLI + MCP clients).
fn rerank_event(
    placement: mnm_core::config::RerankPlacement,
    rerank_model: mnm_core::rerank::RerankParam,
    outcome: Option<&RerankOutcome>,
) -> EventPayload {
    EventPayload::Rerank {
        placement: placement.wire().to_owned(),
        model: rerank_event_model(placement, rerank_model),
        applied: outcome.is_some_and(|o| o.applied),
        reason: outcome.and_then(|o| o.reason.clone()),
        billed_tokens: outcome.and_then(|o| o.billed_tokens),
    }
}

/// Resolve the rerank placement + model and validate the instruction, failing
/// fast before any embedding / network work.
///
/// Placement resolves with precedence `--rerank` flag > `MIDNIGHT_MANUAL_RERANK`
/// env > config `[rerank].location`; `auto` (the flag default, passed through as
/// absent) picks local BYOK when a Voyage key is present, else server (D6). A
/// `local` placement with no key is a hard error rather than a silent degrade.
///
/// # Errors
///
/// Returns `anyhow::Error` when the instruction exceeds the cap, or when `local`
/// was selected without a Voyage key.
fn resolve_rerank(
    args: &Args,
    cfg: &mnm_core::config::RerankConfig,
    env: &impl mnm_core::config::ConfigEnv,
    has_voyage_key: bool,
) -> Result<(mnm_core::config::RerankPlacement, mnm_core::rerank::RerankParam)> {
    let placement = mnm_core::config::resolve_rerank_placement(
        (args.rerank != "auto").then_some(args.rerank.as_str()),
        cfg,
        env,
        has_voyage_key,
    );
    let rerank_model =
        mnm_core::config::resolve_rerank_model(args.rerank_model.as_deref(), cfg, env);
    if let Some(i) = args.rerank_instructions.as_deref() {
        mnm_core::rerank::validate_instruction(i).map_err(|e| anyhow!(e))?;
    }
    // Local placement requires a key: tell the user instead of silently degrading.
    if matches!(placement, mnm_core::config::RerankPlacement::Local) && !has_voyage_key {
        anyhow::bail!(
            "--rerank local needs a Voyage API key (--voyage-api-key, VOYAGE_API_KEY, or config)"
        );
    }
    Ok((placement, rerank_model))
}

/// Which query embeddings the request needs: `(general, code)`. Mirrors the
/// server's `code_mode` defaults client-side (D6: explicit parameter, never
/// query sniffing) — fts embeds nothing; `exclusive` skips the general
/// embedding; `off` skips the code embedding; hybrid/vector default to both.
fn query_embed_needs(mode: &str, code_mode: Option<&str>) -> (bool, bool) {
    let general = mode != "fts" && code_mode != Some("exclusive");
    let code = mode != "fts" && code_mode != Some("off");
    (general, code)
}

/// Embed the query texts with the GENERAL (contextualized) model — BYOK via
/// [`mnm_embedding::contextualized::ContextualizedVoyageEmbedder`] when a
/// Voyage key is present, otherwise proxied through the server's
/// `/v1/embeddings` with `type=general`.
///
/// The embedder `name`/`dim`/`dtype` come from `identity`, which is derived from
/// the corpus's active model (see [`resolve_active_identities`]) so the model
/// computing these vectors matches the wire id labelling them.
///
/// # Errors
///
/// Returns `anyhow::Error` when the embedding call fails or the vector count
/// doesn't match the query count.
async fn embed_general_queries(
    texts: &[String],
    voyage_key: Option<&str>,
    identity: &mnm_core::embedder_identity::EmbedderIdentity,
    server_url: &str,
    bearer: Option<&str>,
) -> Result<Vec<Vec<f32>>> {
    let input_type = mnm_embedding::voyage::InputType::Query;
    let embedded = if let Some(key) = voyage_key {
        let embedder = mnm_embedding::contextualized::ContextualizedVoyageEmbedder::new(
            key,
            &identity.name,
            identity.dim,
            &identity.dtype,
        );
        mnm_embedding::client::embed_general(
            texts.to_vec(),
            input_type,
            mnm_embedding::client::GeneralEmbedSource::Byok(&embedder),
        )
        .await
    } else {
        mnm_embedding::client::embed_general(
            texts.to_vec(),
            input_type,
            mnm_embedding::client::GeneralEmbedSource::Server {
                base_url: server_url,
                bearer,
                // Search never opts out of the global cap (read path, not ingest).
                no_global_limit: false,
            },
        )
        .await
    }
    .context("embed queries via Voyage (general model)")?;
    ensure_vector_count(embedded.vectors, texts.len(), "general")
}

/// Embed the query texts with the CODE model (voyage-code-3, flat endpoint) —
/// BYOK via the flat [`mnm_embedding::voyage::VoyageEmbedder`] when a Voyage
/// key is present, otherwise proxied through the server's `/v1/embeddings`
/// with `type=code`.
///
/// The embedder `name`/`dim`/`dtype` come from `identity`, derived from the
/// active model's `code` half (see [`resolve_active_identities`]) so the model
/// computing these vectors matches the code wire id labelling them.
///
/// # Errors
///
/// Returns `anyhow::Error` when the embedding call fails or the vector count
/// doesn't match the query count.
async fn embed_code_queries(
    texts: &[String],
    voyage_key: Option<&str>,
    identity: &mnm_core::embedder_identity::EmbedderIdentity,
    server_url: &str,
    bearer: Option<&str>,
) -> Result<Vec<Vec<f32>>> {
    let input_type = mnm_embedding::voyage::InputType::Query;
    let embedded = if let Some(key) = voyage_key {
        let embedder = mnm_embedding::voyage::VoyageEmbedder::new(
            key,
            &identity.name,
            identity.dim,
            &identity.dtype,
        );
        mnm_embedding::client::embed_code(
            texts.to_vec(),
            input_type,
            mnm_embedding::client::EmbedSource::Byok(&embedder),
        )
        .await
    } else {
        mnm_embedding::client::embed_code(
            texts.to_vec(),
            input_type,
            mnm_embedding::client::EmbedSource::Server {
                base_url: server_url,
                bearer,
                no_global_limit: false,
            },
        )
        .await
    }
    .context("embed queries via Voyage (code model)")?;
    ensure_vector_count(embedded.vectors, texts.len(), "code")
}

/// Guard: one vector per query text, in order.
fn ensure_vector_count(
    vectors: Vec<Vec<f32>>,
    expected: usize,
    which: &str,
) -> Result<Vec<Vec<f32>>> {
    if vectors.len() == expected {
        Ok(vectors)
    } else {
        Err(anyhow!(
            "{which} embedder returned {} vectors for {expected} queries",
            vectors.len()
        ))
    }
}

/// Resolve, from at most ONE `GET /v1/models/active` round-trip, both the
/// embedder identities used to COMPUTE the query vectors and the wire-id labels
/// sent with them — so the two cannot diverge (cross-element drift fix).
///
/// Returns `(general_id, code_id, general_wire, code_wire)`:
/// - `general_id` / `code_id` ([`EmbedderIdentity`]) drive embedder
///   construction. `dim`/`dtype` come from the active response (the general half
///   and the `code` half respectively); local config is only a logged fallback
///   when the active fetch is unavailable. The general `name` mirrors the ingest
///   path: under an explicit `--embedding-model` override its bare name (the wire
///   id with `@revision` stripped) drives COMPUTE so it matches the `general_wire`
///   LABEL; under `"auto"` the active name wins.
/// - `general_wire` honours an explicit `--embedding-model` override verbatim;
///   the sentinel [`DEFAULT_EMBEDDING_MODEL`] (`"auto"`) resolves from the active
///   model so the wire id always matches the embedded vectors.
/// - `code_wire` (resolved only when `need_code`) comes from the active `code`
///   half, falling back to `<config code model>@1` when the server reports none.
///
/// The round-trip is attempted whenever an embedding is needed or the general
/// wire id is `"auto"`. A fetch failure is NOT fatal here: it degrades to the
/// config-derived identities (logged by [`mnm_core::embedder_identity::derive`])
/// so offline behavior is preserved; the explicit-override / no-embed paths skip
/// the fetch entirely.
async fn resolve_active_identities(
    embedding_model: &str,
    models: &mnm_core::config::ModelsConfig,
    need_general: bool,
    need_code: bool,
    server_url: &str,
) -> Result<(
    mnm_core::embedder_identity::EmbedderIdentity,
    mnm_core::embedder_identity::EmbedderIdentity,
    String,
    Option<String>,
)> {
    use mnm_core::embedder_identity::{
        derive, derive_quiet, ActiveModelIdentity, FallbackIdentity,
    };

    let need_general_fetch = embedding_model == DEFAULT_EMBEDDING_MODEL;
    // Fetch the active model when any embed runs or the general wire id is auto.
    // A transport/decoding failure degrades to config fallback rather than
    // aborting the search (offline preservation), so this is best-effort. The
    // failure is logged HERE (once), so the per-identity `derive` calls below use
    // the quiet variant to avoid warning a second/third time for one event.
    let fetch_attempted = need_general || need_code || need_general_fetch;
    let active = if fetch_attempted {
        match crate::commands::models::fetch_active(server_url).await {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "active-model fetch failed; falling back to local model config");
                None
            }
        }
    } else {
        None
    };

    // General identity: active `{dim, dtype}` is the authority. The NAME mirrors
    // the ingest path (`derive_general_ingest_identity`): under an explicit
    // `--embedding-model` override the bare name (the wire id with `@revision`
    // stripped) drives COMPUTE, so the model embedding the query matches the wire
    // id LABEL `general_wire` stamps below; under "auto" the active name wins.
    // Config is the (already-logged) fallback when the active fetch was
    // unavailable.
    let explicit_general_name = (!need_general_fetch).then(|| {
        embedding_model
            .split_once('@')
            .map_or(embedding_model, |(n, _)| n)
    });
    let general_active = active.as_ref().map(|a| ActiveModelIdentity {
        name: explicit_general_name.map_or_else(|| a.name.clone(), str::to_owned),
        dim: u32::try_from(a.dim).unwrap_or(models.voyage_output_dimension),
        dtype: a.dtype.clone(),
    });
    let general_fallback = FallbackIdentity {
        name: explicit_general_name.unwrap_or(&models.embedding),
        dim: models.voyage_output_dimension,
        dtype: &models.voyage_output_dtype,
    };
    // Always quiet for general: a `None` `general_active` here means either the
    // fetch failed (already logged above) or no fetch was attempted because no
    // general embed is needed (fts / code_mode=exclusive) — in which case the
    // general vector is never produced and the "vectors are NOT guaranteed to
    // match" line would be misleading.
    let general_id = derive_quiet("general", general_active.as_ref(), &general_fallback);

    // Code identity: the active `code` half is the authority; config is the
    // logged fallback. Warn only when a code embed is actually needed AND the
    // fetch SUCCEEDED but reported no `code` half (a genuine, non-duplicate
    // signal). A whole-fetch failure is already logged above, and fts mode
    // (`!need_code`) never produces a code vector — both are quiet.
    let code_active = active
        .as_ref()
        .and_then(|a| a.code.as_ref())
        .map(|c| ActiveModelIdentity {
            name: c.name.clone(),
            dim: u32::try_from(c.dim).unwrap_or(models.voyage_output_dimension),
            dtype: c.dtype.clone(),
        });
    let code_should_warn = need_code && active.is_some();
    let code_derive = if code_should_warn {
        derive
    } else {
        derive_quiet
    };
    let code_id = code_derive(
        "code",
        code_active.as_ref(),
        &FallbackIdentity {
            name: &models.code_embedding,
            dim: models.voyage_output_dimension,
            dtype: &models.voyage_output_dtype,
        },
    );

    // Wire ids. The general wire honours an explicit override verbatim (the
    // override's bare name already drives `general_id.name` above, so COMPUTE and
    // LABEL agree); "auto" resolves from the active fetch (or the config name when
    // the fetch was unavailable).
    let general_wire = if need_general_fetch {
        active.as_ref().map_or_else(
            || format!("{}@1", models.embedding),
            |a| format!("{}@{}", a.name, a.revision),
        )
    } else {
        embedding_model.to_owned()
    };
    let code_wire = need_code.then(|| {
        active.as_ref().and_then(|a| a.code.as_ref()).map_or_else(
            || format!("{}@1", models.code_embedding),
            |c| format!("{}@{}", c.name, c.revision),
        )
    });

    Ok((general_id, code_id, general_wire, code_wire))
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
fn build_filters(args: &Args) -> Result<(String, mnm_retrieval::filters::SearchFilters)> {
    use mnm_retrieval::filters::{
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

/// Client-side fail-fast filter validation. Maps a [`mnm_retrieval::filters::FilterError`]
/// to a friendly `anyhow::Error` that names the offending facet and points at
/// `mnm facets`, so an invalid filter is rejected before any embedding /
/// network work (rather than surfacing as an opaque server 400).
///
/// # Errors
///
/// Returns `anyhow::Error` when `filters.validate()` reports a violation.
fn validate_filters(filters: &mnm_retrieval::filters::SearchFilters) -> Result<()> {
    if let Err(e) = filters.validate() {
        anyhow::bail!(
            "invalid filter `{}`: {} (see `mnm facets` for valid facets and values)",
            e.facet,
            e.message
        );
    }
    Ok(())
}

/// Everything [`build_search_request`] folds into the outgoing body. Grouped
/// into a struct (like [`DispatchSearch`]) to keep the builder under the
/// argument-count lint as fields accrue.
struct SearchRequestParts {
    /// Query pairs (general + code vectors already attached, empty halves for
    /// embeddings the effective mode/code_mode skipped).
    queries: Vec<QueryPair>,
    /// General embedding-model wire id.
    client_embedding_model: String,
    /// Code embedding-model wire id; `None` when no code embedding was made.
    client_code_embedding_model: Option<String>,
    /// Caller's result limit.
    limit: u32,
    /// Where reranking runs (`Local` widens the cloud pool + tells the server
    /// `none`; `Server` sends the model name; `Off` sends `none`).
    placement: mnm_core::config::RerankPlacement,
    /// Voyage rerank model (only sent on the `Server` path).
    rerank_model: mnm_core::rerank::RerankParam,
    /// Agent-supplied rerank instruction (only sent on the `Server` path).
    rerank_instructions: Option<String>,
    /// Query mode (`hybrid` | `vector` | `fts`).
    mode: String,
    /// Raw `--code-mode` value; `None` defers to the server default.
    code_mode: Option<String>,
    /// Raw `--version-match` value (`strict` | `permissive`); `None` defers to
    /// the server default (`permissive`).
    version_match: Option<String>,
    /// Per-result filters.
    filters: SearchFilters,
}

/// Build the outgoing `/v1/search` body, sizing the candidate pool and the
/// `rerank` parameter for the resolved placement.
///
/// On the `Local` path we widen the cloud `limit` to [`RERANK_FETCH`] and ask
/// for relevance order (`sort_by = "score"`) so the client-side reranker can
/// *promote* a chunk the cloud ranked below the caller's `limit` — not merely
/// reorder the caller's top-N (this mirrors the MCP `search` tool);
/// [`apply_rerank`] later truncates the reranked set back to `limit`. `Local`
/// and `Off` both send `rerank: "none"` (exactly one rerank pass, structurally);
/// `Server` sends the resolved model name plus any instructions. `None`-valued
/// `rerank` / `rerank_instructions` / `sort_by` are omitted on the wire
/// (`skip_serializing_if`).
fn build_search_request(parts: SearchRequestParts) -> SearchRequest {
    use mnm_core::config::RerankPlacement;
    let local = matches!(parts.placement, RerankPlacement::Local);
    let (cloud_limit, sort_by) = if local {
        (RERANK_FETCH, Some("score".to_owned()))
    } else {
        (parts.limit, None)
    };
    let (rerank, rerank_instructions) = match parts.placement {
        RerankPlacement::Server => (
            parts.rerank_model.model_name().map(str::to_owned),
            parts.rerank_instructions.clone(),
        ),
        // Local reranks client-side; Off skips. Both tell the server "none"
        // (exactly one rerank pass, structurally).
        RerankPlacement::Local | RerankPlacement::Off => (Some("none".to_owned()), None),
    };
    SearchRequest {
        queries: parts.queries,
        client_embedding_model: parts.client_embedding_model,
        client_code_embedding_model: parts.client_code_embedding_model,
        limit: cloud_limit,
        mode: parts.mode,
        code_mode: parts.code_mode,
        version_match: parts.version_match,
        filters: parts.filters,
        sort_by,
        rerank,
        rerank_instructions,
    }
}

/// What the rerank stage did, for the FR-109 `Rerank` telemetry event
/// (spec §6). Returned by [`dispatch_search`] so `run_with_paths` can emit one
/// event per search next to the `CliCommand` event.
#[derive(Debug, Default)]
struct RerankOutcome {
    /// Whether a rerank was actually applied to the result set (local pass ran,
    /// or the server reported `search_metadata.rerank.applied`).
    applied: bool,
    /// Degrade reason when not applied (server path only; mirrors
    /// `search_metadata.rerank.reason`).
    reason: Option<String>,
    /// Billed-equivalent tokens for a local rerank (`total_tokens` through
    /// [`mnm_core::rerank::RerankParam::billed_tokens`]); `None` on the server /
    /// off paths (the server tracks its own metrics).
    billed_tokens: Option<u64>,
}

/// Everything [`dispatch_search`] needs, already resolved off the `ConfigEnv`
/// (whose trait carries no `Sync` guarantee) into owned data so the async future
/// stays `Send`. Grouped into a struct to keep the function under the
/// argument-count lint.
struct DispatchSearch<'a> {
    /// Resolved rerank placement; `Local` routes to the client-side Voyage path.
    placement: mnm_core::config::RerankPlacement,
    /// Caller's result limit.
    limit: u32,
    /// Cloud base URL.
    server_url: &'a str,
    /// Bearer to forward (rate-limit tier; `/v1/search` is public).
    bearer: Option<&'a str>,
    /// The fully-formed search request.
    request: &'a SearchRequest,
    /// Resolved Voyage rerank model for the `Local` path. Its `model_name()` is
    /// always `Some` for a model variant (off never routes here); its
    /// `billed_tokens()` rate feeds the `Rerank` telemetry event.
    rerank_model: mnm_core::rerank::RerankParam,
    /// Agent-supplied rerank instruction for the `Local` path; `None` derives
    /// the same default the server uses.
    rerank_instructions: Option<&'a str>,
    /// Voyage API key (required for the `Local` path).
    voyage_key: Option<&'a str>,
    /// Resolved Voyage base-url override (self-host / proxy / test mock).
    voyage_base_url: Option<&'a str>,
    /// Render as NDJSON when set.
    json: bool,
}

/// Pick the search path based on `d.placement`: `Local` hands off to
/// [`rerank_via_http`] (client-side Voyage rerank); `Server` / `Off` fetch +
/// render directly (the server already reranked, or nobody did). All env reads
/// happen before this `async fn` (see [`DispatchSearch`]) so its future is
/// `Send`.
///
/// Returns the [`RerankOutcome`] for the FR-109 `Rerank` telemetry event: on
/// the `Server` / `Off` path it is read from the response's
/// `search_metadata.rerank` (`applied` / `reason`); on the `Local` path it
/// carries the actual rerank `applied` flag and billed tokens.
///
/// # Errors
///
/// Propagates the HTTP / decode / rerank errors from the chosen path.
async fn dispatch_search(d: DispatchSearch<'_>) -> Result<RerankOutcome> {
    use mnm_core::config::RerankPlacement;
    if !matches!(d.placement, RerankPlacement::Local) {
        // Server / Off: fetch + render directly (mirroring `search_via_http`)
        // so we can read the server's `search_metadata.rerank` outcome.
        let resp = fetch_search(d.server_url, d.bearer, d.request).await?;
        let outcome = server_rerank_outcome(d.placement, resp.search_metadata.as_ref());
        let texts: Vec<String> = d.request.queries.iter().map(|q| q.text.clone()).collect();
        println!("{}", render(&texts, &resp, d.json));
        return Ok(outcome);
    }
    // Local placement is guarded for a key + model in `run_with_paths`.
    let model = d.rerank_model.model_name().unwrap_or("rerank-2.5");
    let key = d
        .voyage_key
        .ok_or_else(|| anyhow!("local rerank reached dispatch without a Voyage key"))?;
    rerank_via_http(
        d.server_url,
        d.bearer,
        d.request,
        key,
        d.voyage_base_url,
        model,
        d.rerank_model,
        d.rerank_instructions,
        d.limit,
        d.json,
    )
    .await
}

/// Build the [`RerankOutcome`] for a non-local placement (spec §6).
///
/// On the `Server` placement the cloud performed the rerank, so `applied` /
/// `reason` are read from the response's `search_metadata.rerank` object; the
/// server always reports it, and a missing/legacy/malformed field is treated as
/// "not applied" with no reason. On the `Off` placement the client opted out and
/// knows the rerank was not applied without trusting the server echo, so it
/// reports `applied=false` / `reason=None` — matching the MCP client so the two
/// emit the same `reason` for the same logical situation.
///
/// The server `reason` is routed through [`mnm_core::rerank::known_reason`] so
/// only the documented closed set can reach the `Rerank` event — arbitrary
/// server text is dropped, preserving the telemetry privacy invariant.
/// `billed_tokens` is always `None` here — the server tracks its own metrics.
fn server_rerank_outcome(
    placement: mnm_core::config::RerankPlacement,
    search_metadata: Option<&serde_json::Value>,
) -> RerankOutcome {
    use mnm_core::config::RerankPlacement;
    if !matches!(placement, RerankPlacement::Server) {
        // Off (the only other non-local placement reaching here): no rerank was
        // requested anywhere; don't trust the server echo.
        return RerankOutcome::default();
    }
    let rerank = search_metadata.and_then(|m| m.get("rerank"));
    let applied = rerank
        .and_then(|r| r.get("applied"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let reason = rerank
        .and_then(|r| r.get("reason"))
        .and_then(serde_json::Value::as_str)
        .and_then(mnm_core::rerank::known_reason)
        .map(str::to_owned);
    RerankOutcome {
        applied,
        reason,
        billed_tokens: None,
    }
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
        // Decode the typed error envelope and, on a 409 embedding-model
        // mismatch, surface the server's `message` + `remediation` instead of
        // dumping the raw JSON body — parity with the MCP cloud client
        // (`mnm_mcp::cloud_client::parse_mismatch`). Any other decoded code, or
        // an undecodable body, falls back to the redacted raw form below.
        if let Some(err) = crate::shared::decode_error_envelope(&body) {
            if err.code == mnm_core::error::ErrorCode::EmbeddingModelMismatch {
                return Err(anyhow!("{}\n  remediation: {}", err.message, err.remediation));
            }
        }
        return Err(anyhow!("{status} from /v1/search: {}", redact_token_like(&body)));
    }
    resp.json().await.context("parse /v1/search response body")
}

/// `Local` rerank path: fetch `/v1/search`, rerank the candidates against the
/// first query via `VoyageAI`'s `/v1/rerank` (BYOK), re-order + truncate, then
/// render.
///
/// This mirrors the MCP `search` tool: the `request` passed here was built with
/// a wider [`RERANK_FETCH`] candidate pool sorted by score (`sort_by =
/// "score"`), the candidates are reranked against the first query, and
/// [`apply_rerank`] truncates the reranked set back to the caller's `--limit`.
/// Candidates are mapped back by [`mnm_embedding::RerankResult::index`] (NOT
/// positionally — Voyage reorders), and each surviving result is stamped with a
/// `rerank_score` (a 0–1 relevance) in its `scores` bag.
///
/// The instruction precedence matches the server so placement doesn't change
/// results: an agent-supplied `instruction` wins; otherwise the same derived
/// default the server uses (from `code_mode` exclusive / version filter).
///
/// Returns the [`RerankOutcome`] for the FR-109 `Rerank` event: `applied` is
/// `true` once the Voyage call succeeds (`false` for an empty result set, where
/// nothing is reranked), and `billed_tokens` is Voyage's reported `total_tokens`
/// through `rerank_model`'s [`mnm_core::rerank::RerankParam::billed_tokens`] rate.
///
/// # Errors
///
/// Returns `anyhow::Error` on the HTTP fetch failure or a Voyage rerank failure.
#[allow(clippy::too_many_arguments)]
async fn rerank_via_http(
    server_url: &str,
    bearer: Option<&str>,
    request: &SearchRequest,
    voyage_key: &str,
    voyage_base_url: Option<&str>,
    model: &'static str,
    rerank_model: mnm_core::rerank::RerankParam,
    instruction: Option<&str>,
    limit: u32,
    json: bool,
) -> Result<RerankOutcome> {
    let resp = fetch_search(server_url, bearer, request).await?;
    let texts: Vec<String> = request.queries.iter().map(|q| q.text.clone()).collect();

    // Empty result set: nothing to rerank — render straight through.
    if resp.results.is_empty() {
        println!("{}", render(&texts, &resp, json));
        return Ok(RerankOutcome::default());
    }

    let mut reranker = mnm_embedding::voyage::VoyageReranker::new(voyage_key, model);
    if let Some(base) = voyage_base_url {
        reranker = reranker.with_base_url(base);
    }
    // Pivot on the first query (the most "user-facing" text for HyDE / expansion).
    let pivot = texts.first().cloned().unwrap_or_default();
    // Agent instruction wins; else the same derived default the server uses
    // (code_mode exclusive / version filter), so placement doesn't change results.
    let derived;
    let instr = if let Some(i) = instruction {
        Some(i)
    } else {
        let code_exclusive = request.code_mode.as_deref() == Some("exclusive");
        let version = request
            .filters
            .language_target
            .any_of
            .first()
            .and_then(|lt| {
                lt.version_satisfies
                    .as_deref()
                    .map(|v| (lt.name.as_str(), v))
            });
        derived = mnm_core::rerank::default_instruction(code_exclusive, version);
        derived.as_deref()
    };
    let composed = mnm_core::rerank::compose_rerank_query(&pivot, instr);
    let docs: Vec<String> = resp.results.iter().map(|r| r.content.clone()).collect();
    let out = reranker
        .rerank(composed, docs, None)
        .await
        .context("voyage rerank")?;
    // Apply the model's billing rate to Voyage's reported tokens (D5).
    let billed_tokens = rerank_model.billed_tokens(out.total_tokens);
    let reordered = apply_rerank(resp.results, &out.results, limit);
    let out = SearchResponse {
        results: reordered,
        search_metadata: resp.search_metadata,
    };
    println!("{}", render(&texts, &out, json));
    Ok(RerankOutcome {
        applied: true,
        reason: None,
        billed_tokens: Some(billed_tokens),
    })
}

/// Re-order `results` by Voyage relevance score (0–1, descending) and truncate
/// to `limit`, mapping each score back to its result by
/// [`mnm_embedding::RerankResult::index`] — NOT positionally, because Voyage may
/// return results in a different order than the input. A `rerank_score` is
/// stamped into each surviving result's `scores` JSON. Indices that fall
/// outside `results` or repeat are dropped defensively.
///
/// Pure (no model / IO) so it is unit-testable without a reranker or network.
fn apply_rerank(
    mut results: Vec<SearchResult>,
    scores: &[mnm_embedding::RerankResult],
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

/// Record the Voyage relevance score (0–1) as `scores.rerank_score`, creating
/// the `scores` object if the server didn't send one.
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
    /// Code embedding-model wire id the `code_vector`s were encoded against
    /// (required server-side when the effective code_mode != off). `None` when
    /// no code embedding was made — the key is then omitted on the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_code_embedding_model: Option<String>,
    /// Maximum number of results.
    pub limit: u32,
    /// Query mode (`hybrid` | `vector` | `fts`); serialized as the `mode` key,
    /// matching the cloud's snake_case `SearchMode` values.
    pub mode: String,
    /// Code-vector fusion mode (`on` | `off` | `exclusive`), serialized as the
    /// raw string (the cloud's snake_case `CodeMode` values). `None` defers to
    /// the server default (on for hybrid/vector, forced off for fts) and omits
    /// the key entirely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_mode: Option<String>,
    /// Version-matching mode (`strict` | `permissive`). `None` defers to the
    /// server default (`permissive`) and omits the key on the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_match: Option<String>,
    /// Per-result filters.
    pub filters: SearchFilters,
    /// Optional ordering hint for the candidate pool. The local-rerank path sets
    /// this to `"score"` so the cloud returns relevance-ordered candidates
    /// (rather than its confidence-first default) before the client reranks
    /// them. `None` otherwise — `skip_serializing_if` then omits the key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    /// Server-side rerank parameter: the Voyage model name on the `Server` path,
    /// `"none"` on the `Local` / `Off` paths (exactly one rerank pass). `None`
    /// only on the legacy no-placement build — `skip_serializing_if` omits it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank: Option<String>,
    /// Agent-supplied rerank instruction forwarded on the `Server` path; `None`
    /// (and omitted) otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank_instructions: Option<String>,
}

/// One {text, vector, code_vector} triple.
#[derive(Debug, Clone, Serialize)]
pub struct QueryPair {
    /// Verbatim query text.
    pub text: String,
    /// Pre-computed general embedding; its dimension is set by the active
    /// corpus model (e.g. 1024 for voyage-context-3). Empty — and omitted on
    /// the wire — when the effective mode/code_mode skips the general
    /// embedding (fts, or code_mode=exclusive).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vector: Vec<f32>,
    /// Pre-computed code-model embedding (e.g. 1024 for voyage-code-3). Empty
    /// — and omitted on the wire — unless the effective code_mode != off.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub code_vector: Vec<f32>,
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
            rerank: "auto".to_owned(),
            rerank_model: None,
            rerank_instructions: None,
            mode: "hybrid".to_owned(),
            version_match: None,
            code_mode: None,
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
            mnm_embedding::RerankResult { index: 2, score: 0.20 }, // c
            mnm_embedding::RerankResult { index: 0, score: 0.10 }, // a
            mnm_embedding::RerankResult { index: 1, score: 0.95 }, // b — most relevant
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

    /// Default [`SearchRequestParts`] for the build tests: one hybrid query
    /// pair carrying a general vector, the given placement, no code fields.
    fn parts(
        queries: Vec<QueryPair>,
        limit: u32,
        placement: mnm_core::config::RerankPlacement,
    ) -> SearchRequestParts {
        SearchRequestParts {
            queries,
            client_embedding_model: "voyage-context-3@1".to_owned(),
            client_code_embedding_model: None,
            limit,
            placement,
            rerank_model: mnm_core::rerank::RerankParam::Rerank25,
            rerank_instructions: None,
            mode: "hybrid".to_owned(),
            code_mode: None,
            version_match: None,
            filters: SearchFilters::default(),
        }
    }

    #[test]
    fn build_request_local_widens_pool_and_sets_score_sort() {
        use mnm_core::config::RerankPlacement;
        let q = vec![QueryPair {
            text: "x".to_owned(),
            vector: vec![0.0],
            code_vector: Vec::new(),
        }];
        let req = build_search_request(parts(q, 5, RerankPlacement::Local));
        // Caller asked for 5 but the cloud pool is widened so the local reranker
        // can promote a below-cutoff candidate.
        assert_eq!(req.limit, RERANK_FETCH);
        assert_eq!(req.sort_by.as_deref(), Some("score"));
        // Local always tells the server "none" (exactly one rerank pass).
        assert_eq!(req.rerank.as_deref(), Some("none"));
        // sort_by serializes as the "score" key when present.
        let body = serde_json::to_value(&req).unwrap();
        assert_eq!(body["limit"], RERANK_FETCH);
        assert_eq!(body["sort_by"], "score");
        assert_eq!(body["rerank"], "none");
    }

    #[test]
    fn build_request_server_keeps_limit_and_omits_sort_by() {
        use mnm_core::config::RerankPlacement;
        let q = vec![QueryPair {
            text: "x".to_owned(),
            vector: vec![0.0],
            code_vector: Vec::new(),
        }];
        let req = build_search_request(parts(q, 5, RerankPlacement::Server));
        assert_eq!(req.limit, 5);
        assert!(req.sort_by.is_none());
        // Server placement sends the model name (not "none") and the caller's limit.
        assert_eq!(req.rerank.as_deref(), Some("rerank-2.5"));
        // sort_by None must be OMITTED on the wire (skip_serializing_if), not null.
        let body = serde_json::to_value(&req).unwrap();
        assert_eq!(body["limit"], 5);
        assert!(
            body.as_object().unwrap().get("sort_by").is_none(),
            "sort_by key must be absent for the server path"
        );
    }

    #[test]
    fn embed_needs_mirror_server_code_mode_defaults() {
        // (general, code) per mode × code_mode — no query sniffing (D6).
        assert_eq!(query_embed_needs("hybrid", None), (true, true));
        assert_eq!(query_embed_needs("vector", None), (true, true));
        assert_eq!(query_embed_needs("hybrid", Some("on")), (true, true));
        assert_eq!(query_embed_needs("hybrid", Some("off")), (true, false));
        assert_eq!(query_embed_needs("hybrid", Some("exclusive")), (false, true));
        // fts embeds nothing, whatever code_mode says (server 400s on/exclusive).
        assert_eq!(query_embed_needs("fts", None), (false, false));
        assert_eq!(query_embed_needs("fts", Some("on")), (false, false));
        assert_eq!(query_embed_needs("fts", Some("exclusive")), (false, false));
    }

    #[test]
    fn build_request_exclusive_sends_code_mode_and_permits_empty_general_vector() {
        let q = vec![QueryPair {
            text: "deployContract".to_owned(),
            vector: Vec::new(), // exclusive: no general embedding was made
            code_vector: vec![0.5, 0.25],
        }];
        let mut p = parts(q, 5, mnm_core::config::RerankPlacement::Server);
        p.client_code_embedding_model = Some("voyage-code-3@1".to_owned());
        p.code_mode = Some("exclusive".to_owned());
        let body = serde_json::to_value(build_search_request(p)).unwrap();
        assert_eq!(body["code_mode"], "exclusive");
        assert_eq!(body["client_code_embedding_model"], "voyage-code-3@1");
        let q0 = body["queries"][0].as_object().unwrap();
        assert!(
            q0.get("vector").is_none(),
            "empty general vector must be omitted in exclusive mode, got: {q0:?}"
        );
        assert_eq!(q0["code_vector"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn build_request_fts_omits_vector_and_code_fields() {
        let q = vec![QueryPair {
            text: "plain words".to_owned(),
            vector: Vec::new(),
            code_vector: Vec::new(),
        }];
        let mut p = parts(q, 5, mnm_core::config::RerankPlacement::Server);
        p.mode = "fts".to_owned();
        let body = serde_json::to_value(build_search_request(p)).unwrap();
        let top = body.as_object().unwrap();
        assert!(top.get("code_mode").is_none(), "code_mode key must be absent");
        assert!(
            top.get("client_code_embedding_model").is_none(),
            "client_code_embedding_model key must be absent"
        );
        let q0 = body["queries"][0].as_object().unwrap();
        assert!(q0.get("vector").is_none(), "fts sends no general vector");
        assert!(q0.get("code_vector").is_none(), "fts sends no code vector");
    }

    #[test]
    fn code_mode_flag_parses_enum_and_defaults_to_none() {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Probe {
            #[command(flatten)]
            inner: Args,
        }
        let p = Probe::parse_from(["search", "q"]);
        assert!(p.inner.code_mode.is_none(), "--code-mode must default to None");
        let p = Probe::parse_from(["search", "q", "--code-mode", "exclusive"]);
        assert_eq!(p.inner.code_mode.as_deref(), Some("exclusive"));
        assert!(
            Probe::try_parse_from(["search", "q", "--code-mode", "bogus"]).is_err(),
            "values outside on|off|exclusive must be rejected at parse time"
        );
    }

    #[test]
    fn apply_rerank_truncates_to_limit() {
        let results = vec![result(1, "a"), result(2, "b"), result(3, "c")];
        let scores = vec![
            mnm_embedding::RerankResult { index: 0, score: 0.1 },
            mnm_embedding::RerankResult { index: 1, score: 0.9 },
            mnm_embedding::RerankResult { index: 2, score: 0.5 },
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
            mnm_embedding::RerankResult { index: 0, score: 0.5 },
            mnm_embedding::RerankResult { index: 5, score: 0.9 }, // out of range — dropped
            mnm_embedding::RerankResult { index: 0, score: 0.8 }, // duplicate — dropped
            mnm_embedding::RerankResult { index: 1, score: 0.3 },
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
        let scores = vec![mnm_embedding::RerankResult { index: 0, score: 1.25 }];
        let out = apply_rerank(results, &scores, 10);
        assert!(out[0].scores.is_some(), "scores object must be created");
        assert!(out[0].scores.as_ref().unwrap()["rerank_score"].is_number());
    }

    #[test]
    fn search_args_rerank_flags_default_and_parse() {
        use clap::Parser as _;

        #[derive(clap::Parser)]
        struct Probe {
            #[command(flatten)]
            inner: Args,
        }

        // Defaults: --rerank → "auto", model/instructions → None.
        let p = Probe::parse_from(["search", "q"]);
        assert_eq!(p.inner.rerank, "auto", "--rerank must default to auto");
        assert!(p.inner.rerank_model.is_none());
        assert!(p.inner.rerank_instructions.is_none());

        // Present: all three flags parse with their value parsers.
        let p = Probe::parse_from([
            "search",
            "q",
            "--rerank",
            "local",
            "--rerank-model",
            "rerank-2.5-lite",
            "--rerank-instructions",
            "Prefer prose.",
        ]);
        assert_eq!(p.inner.rerank, "local");
        assert_eq!(p.inner.rerank_model.as_deref(), Some("rerank-2.5-lite"));
        assert_eq!(p.inner.rerank_instructions.as_deref(), Some("Prefer prose."));

        // The value parser rejects an out-of-set placement.
        assert!(
            Probe::try_parse_from(["search", "q", "--rerank", "bogus"]).is_err(),
            "values outside auto|local|server|off must be rejected at parse time"
        );
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

    #[test]
    fn build_search_request_rerank_wire_matrix() {
        use mnm_core::config::RerankPlacement;
        use mnm_core::rerank::RerankParam;
        let base = || SearchRequestParts {
            queries: vec![QueryPair {
                text: "q".into(),
                vector: vec![],
                code_vector: vec![],
            }],
            client_embedding_model: "m@1".into(),
            client_code_embedding_model: None,
            limit: 10,
            placement: RerankPlacement::Server,
            rerank_model: RerankParam::Rerank25,
            rerank_instructions: None,
            mode: "hybrid".into(),
            code_mode: None,
            version_match: None,
            filters: SearchFilters::default(),
        };

        // Server placement: server reranks; caller's limit goes on the wire.
        let r = build_search_request(base());
        assert_eq!(r.rerank.as_deref(), Some("rerank-2.5"));
        assert_eq!(r.limit, 10);
        assert!(r.sort_by.is_none());

        // Local placement: tell the server "none", over-fetch, sort by score.
        let mut p = base();
        p.placement = RerankPlacement::Local;
        let r = build_search_request(p);
        assert_eq!(r.rerank.as_deref(), Some("none"));
        assert_eq!(r.limit, RERANK_FETCH);
        assert_eq!(r.sort_by.as_deref(), Some("score"));

        // Off: "none", caller's limit, no over-fetch.
        let mut p = base();
        p.placement = RerankPlacement::Off;
        let r = build_search_request(p);
        assert_eq!(r.rerank.as_deref(), Some("none"));
        assert_eq!(r.limit, 10);

        // Lite model + instructions ride along on the server path.
        let mut p = base();
        p.rerank_model = RerankParam::Rerank25Lite;
        p.rerank_instructions = Some("Prefer prose.".into());
        let r = build_search_request(p);
        assert_eq!(r.rerank.as_deref(), Some("rerank-2.5-lite"));
        assert_eq!(r.rerank_instructions.as_deref(), Some("Prefer prose."));
    }

    #[test]
    fn server_rerank_outcome_parses_server_metadata_shapes() {
        use mnm_core::config::RerankPlacement;
        let meta = |v: serde_json::Value| server_rerank_outcome(RerankPlacement::Server, Some(&v));

        // Well-formed applied=true with a known reason omitted.
        let o = meta(serde_json::json!({ "rerank": { "applied": true } }));
        assert!(o.applied);
        assert_eq!(o.reason, None);
        assert_eq!(o.billed_tokens, None, "server tracks its own token metrics");

        // Well-formed degrade: applied=false + a documented reason.
        let o = meta(serde_json::json!({
            "rerank": { "applied": false, "reason": "token_budget_exhausted" }
        }));
        assert!(!o.applied);
        assert_eq!(o.reason.as_deref(), Some("token_budget_exhausted"));

        // Missing `rerank` key: degrade silently to not-applied / no-reason.
        let o = meta(serde_json::json!({ "other": 1 }));
        assert!(!o.applied);
        assert_eq!(o.reason, None);

        // Missing `applied`: treated as not-applied.
        let o = meta(serde_json::json!({ "rerank": { "reason": "disabled" } }));
        assert!(!o.applied);
        assert_eq!(o.reason.as_deref(), Some("disabled"));

        // Non-bool `applied`: not coerced — treated as not-applied.
        let o = meta(serde_json::json!({ "rerank": { "applied": "yes" } }));
        assert!(!o.applied);

        // Unknown / arbitrary reason text is dropped by the privacy allow-list
        // (only the closed `mnm_core::rerank::RERANK_REASONS` set survives).
        let o = meta(serde_json::json!({
            "rerank": { "applied": false, "reason": "rate limited: token=eyJabc" }
        }));
        assert_eq!(o.reason, None, "free-form server reason must not reach the event");

        // No metadata at all on the server path: not-applied / no-reason.
        let o = server_rerank_outcome(RerankPlacement::Server, None);
        assert!(!o.applied);
        assert_eq!(o.reason, None);
    }

    #[test]
    fn server_rerank_outcome_off_ignores_metadata() {
        use mnm_core::config::RerankPlacement;
        // Off opted out client-side; it must NOT trust the server echo, so even
        // a present `applied:true` / reason is ignored (matches the MCP client,
        // keeping `reason` comparable across clients for the same situation).
        let meta = serde_json::json!({
            "rerank": { "applied": true, "reason": "not_requested" }
        });
        let o = server_rerank_outcome(RerankPlacement::Off, Some(&meta));
        assert!(!o.applied);
        assert_eq!(o.reason, None);
        assert_eq!(o.billed_tokens, None);
    }

    #[test]
    fn rerank_event_payload_matrix() {
        use mnm_core::config::RerankPlacement;
        use mnm_core::rerank::RerankParam;

        // Off: no model, not applied, no reason/tokens — regardless of outcome.
        let p = rerank_event(RerankPlacement::Off, RerankParam::None, None);
        match p {
            EventPayload::Rerank {
                placement,
                model,
                applied,
                reason,
                billed_tokens,
            } => {
                assert_eq!(placement, "off");
                assert_eq!(model, None);
                assert!(!applied);
                assert_eq!(reason, None);
                assert_eq!(billed_tokens, None);
            }
            _ => panic!("expected Rerank payload"),
        }

        // Local applied: model named, applied=true, billed tokens carried.
        let outcome = RerankOutcome {
            applied: true,
            reason: None,
            billed_tokens: Some(321),
        };
        let p = rerank_event(RerankPlacement::Local, RerankParam::Rerank25, Some(&outcome));
        match p {
            EventPayload::Rerank {
                placement,
                model,
                applied,
                billed_tokens,
                ..
            } => {
                assert_eq!(placement, "local");
                assert_eq!(model.as_deref(), Some("rerank-2.5"));
                assert!(applied);
                assert_eq!(billed_tokens, Some(321));
            }
            _ => panic!("expected Rerank payload"),
        }

        // Server degrade: model named, applied=false with a documented reason.
        let outcome = RerankOutcome {
            applied: false,
            reason: Some("provider_error".to_owned()),
            billed_tokens: None,
        };
        let p = rerank_event(RerankPlacement::Server, RerankParam::Rerank25Lite, Some(&outcome));
        match p {
            EventPayload::Rerank {
                placement,
                model,
                applied,
                reason,
                ..
            } => {
                assert_eq!(placement, "server");
                assert_eq!(model.as_deref(), Some("rerank-2.5-lite"));
                assert!(!applied);
                assert_eq!(reason.as_deref(), Some("provider_error"));
            }
            _ => panic!("expected Rerank payload"),
        }

        // Search failed before rerank (outcome=None): reported as not applied.
        let p = rerank_event(RerankPlacement::Server, RerankParam::Rerank25, None);
        match p {
            EventPayload::Rerank {
                applied, reason, billed_tokens, ..
            } => {
                assert!(!applied);
                assert_eq!(reason, None);
                assert_eq!(billed_tokens, None);
            }
            _ => panic!("expected Rerank payload"),
        }
    }
}

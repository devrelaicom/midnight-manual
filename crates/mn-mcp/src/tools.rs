//! MCP tool registry and per-tool handlers.
//!
//! Thirteen tools, four categories:
//!
//! - `status` — diagnostics; the assembler lives in [`crate::status`] (cloud
//!   `/readyz` + `/v1/me` probes, VoyageAI key validity, local reranker
//!   state). This module only contributes the reranker-loaded marker.
//! - `search` / `advanced_search` — embed via VoyageAI (BYOK or server-proxy),
//!   post to the cloud `/v1/search`, optionally rerank with the local
//!   cross-encoder. `search` is the simple 90% surface (`{query, mode?,
//!   limit?}`); `advanced_search` exposes multi-query fusion, facet filters,
//!   and the rerank toggle.
//! - All other tools (`get_chunks` / `get_chunk_next` / `get_chunk_prev` /
//!   `get_chunk_neighbors` / `get_chunk_parents` / `get_document` /
//!   `get_document_chunks` / `list_sources`) —
//!   pass-through to the cloud's read endpoints, returning the response JSON
//!   verbatim. `get_chunks` batches 1-20 ids into one `/v1/chunks?ids=` call;
//!   `get_chunk_neighbors` is the only one that fans out to three
//!   cloud endpoints concurrently and bundles the results.
//! - Local install: `install_search_skill` (writes the advanced-search
//!   `SKILL.md` into the user's AI harness(es)).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mn_core::scoring::normalize_rerank;
use mn_core::scoring_policy::ScoringPolicy;
use mn_embedding::{client as embed_client, reranker, reranker_catalog, voyage, LoadedReranker};
use serde_json::json;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::cloud_client::{CloudClient, CloudError, QueryPair, SearchRequest};
use crate::protocol::{ToolAnnotations, ToolDescription, ToolsListResult};
use crate::server::ServerConfig;

/// Build the static tool manifest sent in response to `tools/list`.
///
/// All thirteen tools declared in spec.md US5 / contracts/mcp-tools.json, in
/// canonical registration order (search pair, chunk reads, document reads,
/// corpus discovery, diagnostics, local install). Schemas here are kept in
/// sync with the canonical document by way of the contract tests in `tests/`.
#[must_use]
// A flat manifest of `ToolDescription` literals: length is inherent to the
// data (one entry per tool), so splitting it would hurt readability without
// reducing any real complexity.
#[allow(clippy::too_many_lines)]
pub fn list() -> ToolsListResult {
    ToolsListResult {
        tools: vec![
            ToolDescription {
                name: "search",
                description:
                    "Search the Midnight Network documentation and code corpus (docs, SDK references, Compact language material, code examples). Returns ranked excerpts with confidence scores and source attribution. Use it whenever you need facts about Midnight, Compact, or the Midnight SDK. For multi-query strategies, facet filters, or rerank control, use advanced_search.",
                input_schema: search_input_schema(),
                output_schema: Some(crate::schemas::search_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "advanced_search",
                description:
                    "Full-control search over the Midnight corpus: fuse multiple queries (HyDE, expansion, step-back), restrict by facet filters, switch retrieval mode, and toggle reranking. Use when basic search comes up short or when the midnight-advanced-search skill prescribes a pattern. Call facets first to discover valid filter values.",
                input_schema: advanced_search_input_schema(),
                output_schema: Some(crate::schemas::search_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "get_chunks",
                description:
                    "Fetch the full content of one or more chunks by id, typically ids returned by search. Use this to read the actual text behind search results.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "ids": { "type": "array", "minItems": 1, "maxItems": 20,
                            "items": { "type": "string", "format": "uuid" },
                            "description": "Chunk ids to fetch, 1-20 per call. One id is a one-element array." }
                    },
                    "required": ["ids"],
                    "additionalProperties": false
                }),
                output_schema: Some(crate::schemas::chunks_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "get_chunk_next",
                description:
                    "Fetch chunks that immediately follow a given chunk in its document's reading order. Use to continue reading past the end of a chunk you already have.",
                input_schema: chunk_nav_schema(),
                output_schema: Some(crate::schemas::chunk_list_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "get_chunk_prev",
                description:
                    "Fetch chunks that immediately precede a given chunk in its document's reading order. Use to read the context leading up to a chunk you already have.",
                input_schema: chunk_nav_schema(),
                output_schema: Some(crate::schemas::chunk_list_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "get_chunk_neighbors",
                description:
                    "Fetch the chunks immediately before and after a given chunk in one call. Use when a search hit needs surrounding context to be understood.",
                input_schema: chunk_neighbors_schema(),
                output_schema: Some(crate::schemas::neighbors_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "get_chunk_parents",
                description:
                    "Show where a chunk sits in its source's structure: the chain of containing nodes (document, folders) up to the source root. Use to orient a chunk within its source and find its containing document.",
                input_schema: id_only_schema(),
                output_schema: Some(crate::schemas::parents_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "get_document",
                description:
                    "Fetch a document's metadata plus an ordered skeleton of its chunks (ids, positions, token counts — no bodies). Use to size up a document before reading it with get_document_chunks.",
                input_schema: id_only_schema(),
                output_schema: Some(crate::schemas::document_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "get_document_chunks",
                description:
                    "Read a window of a document's chunk bodies by position. Use after get_document to read a document section by section.",
                input_schema: document_chunks_schema(),
                output_schema: Some(crate::schemas::document_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "list_sources",
                description:
                    "List the sources that make up the corpus (paginated). Use to discover what material exists and to get source slugs for advanced_search filters.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "cursor": { "type": "string", "description": "Opaque pagination token from a previous response's next_cursor." },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                        "created_after": { "type": "string", "format": "date-time", "description": "Only sources registered after this RFC3339 instant." },
                        "created_before": { "type": "string", "format": "date-time", "description": "Only sources registered before this RFC3339 instant." },
                        "kind": { "type": "string", "enum": ["docs_site", "code_repo", "standalone", "mixed"] },
                        "retired": { "type": "boolean", "default": false, "description": "Include retired sources." }
                    },
                    "additionalProperties": false,
                }),
                output_schema: Some(crate::schemas::sources_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "facets",
                description:
                    "Discover the filter dimensions available to advanced_search and the values present in the corpus. Call without arguments for an overview; pass a facet name to page through all values of one dimension.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "facet": { "type": "string", "enum": ["source_slug", "language", "tags", "package"],
                            "description": "Drill into one open-set facet's full value list. Omit for the overview." },
                        "cursor": { "type": "string", "description": "Opaque token from a previous drill-down response." },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
                    },
                    "additionalProperties": false,
                }),
                output_schema: Some(crate::schemas::facets_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "status",
                description:
                    "Diagnose the retrieval setup: cloud reachability, authentication and rate-limit state, VoyageAI key validity, and reranker readiness. Call when searches fail, return errors, or before starting a long session.",
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                output_schema: Some(crate::schemas::status_output_schema()),
                annotations: ToolAnnotations::read_only(),
            },
            ToolDescription {
                name: "install_search_skill",
                description:
                    "Install (or update) the midnight-advanced-search skill — a retrieval playbook teaching effective corpus search patterns — into the user's AI harness(es). Use when search results are poor or the user asks for better search guidance.",
                input_schema: install_search_skill_schema(),
                output_schema: Some(crate::schemas::install_output_schema()),
                annotations: ToolAnnotations::idempotent_writer(),
            },
        ],
    }
}

fn id_only_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
        },
        "additionalProperties": false,
    })
}

fn chunk_nav_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid",
                "description": "Anchor chunk id (from search results or another chunk tool)." },
            "count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 5,
                "description": "Number of chunks to return; must be in [1, 100] (defaults to 5). Out-of-range values are rejected before any network call. Calling past the document edge returns an empty list, not an error." },
        },
        "additionalProperties": false,
    })
}

fn chunk_neighbors_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid",
                "description": "Anchor chunk id (from search results or another chunk tool)." },
            "count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 2,
                "description": "Chunks to fetch on each side of the anchor; must be in [1, 100] (defaults to 2). Out-of-range values are rejected before any network call. A side past the document edge comes back empty, not as an error." },
        },
        "additionalProperties": false,
    })
}

fn document_chunks_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid",
                "description": "Document id (from search results or get_document)." },
            "from": { "type": "integer", "minimum": 0, "default": 0,
                "description": "Zero-based chunk position to start from; must be >= 0 (defaults to 0). A position past the end returns an empty window with accurate total_chunks, not an error." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20,
                "description": "Number of chunk bodies to return; must be in [1, 100] (defaults to 20)." },
        },
        "additionalProperties": false,
    })
}

/// Input schema for the basic `search` tool: the simple 90% surface.
fn search_input_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "minLength": 1,
                "description": "What you want to find, as natural language or code terms." },
            "mode": { "type": "string", "enum": ["hybrid", "vector", "fts"], "default": "hybrid",
                "description": "hybrid (default) fuses keyword + semantic; fts is keyword-only (lowest latency); vector is semantic-only." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10,
                "description": "Max results returned." }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

/// Input schema for `advanced_search`: multi-query fusion, facet filters,
/// mode switch, and the rerank toggle.
fn advanced_search_input_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "queries": { "type": "array", "minItems": 1, "maxItems": 10,
                "items": { "type": "string", "minLength": 1 },
                "description": "1-10 query variants fused with RRF (HyDE, expansion, step-back). One query = one-element array. Rate-limit cost is one token per distinct query." },
            "mode": { "type": "string", "enum": ["hybrid", "vector", "fts"], "default": "hybrid",
                "description": "hybrid (default) fuses keyword + semantic; fts is keyword-only; vector is semantic-only." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10,
                "description": "Max results returned." },
            "rerank": { "type": "boolean", "default": true,
                "description": "Apply cross-encoder reranking against the first query. Disable for lowest latency." },
            "filters": filters_schema()
        },
        "required": ["queries"],
        "additionalProperties": false
    })
}

/// The per-facet `filters` schema, referenced only by `advanced_search`.
#[allow(clippy::too_many_lines)]
fn filters_schema() -> serde_json::Value {
    use mn_retrieval::facets;
    let set_of = |values: Option<&[&str]>| {
        let items = values.map_or_else(
            || json!({ "type": "string" }),
            |v| json!({ "type": "string", "enum": v }),
        );
        json!({
            "type": "object",
            "properties": { "any_of": { "type": "array", "items": items }, "none_of": { "type": "array", "items": items } },
            "additionalProperties": false,
        })
    };
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
                    "kind":         set_of(Some(facets::KIND_VALUES)),
                    "source_kind":  set_of(Some(facets::SOURCE_KIND_VALUES)),
                    "attribution":  set_of(Some(facets::ATTRIBUTION_VALUES)),
                    "content_type": set_of(Some(facets::CONTENT_TYPE_VALUES)),
                    "language":     set_of(None),
                    "tags":         set_of(None),
                    "source_slug":  set_of(None),
                    "heading_path": set_of(None),
                    "verified":   { "type": "boolean" },
                    "deprecated": { "type": "boolean" },
                    "symbol": { "type": "object", "properties": { "any_of": { "type": "array", "items": {
                        "type": "object", "properties": { "kind": { "type": "string" }, "name": { "type": "string" } },
                        "additionalProperties": false } }, "none_of": { "type": "array", "items": {
                        "type": "object", "properties": { "kind": { "type": "string" }, "name": { "type": "string" } },
                        "additionalProperties": false } } }, "additionalProperties": false },
                    "package": { "type": "object", "properties": { "any_of": { "type": "array", "items": {
                        "type": "object", "required": ["kind","name"], "properties": { "kind": { "type": "string", "enum": facets::PACKAGE_KIND_VALUES }, "name": { "type": "string" } },
                        "additionalProperties": false } }, "none_of": { "type": "array", "items": {
                        "type": "object", "required": ["kind","name"], "properties": { "kind": { "type": "string", "enum": facets::PACKAGE_KIND_VALUES }, "name": { "type": "string" } },
                        "additionalProperties": false } } }, "additionalProperties": false },
                    "language_target": { "type": "object", "additionalProperties": false, "properties": { "any_of": { "type": "array", "items": {
                        "type": "object", "required": ["name"], "additionalProperties": false,
                        "properties": { "name": { "type": "string" }, "version_satisfies": { "type": "string" } } } } } },
                    "sdk_dependency": { "type": "object", "additionalProperties": false, "properties": { "any_of": { "type": "array", "items": {
                        "type": "object", "required": ["kind", "name"], "additionalProperties": false,
                        "properties": { "kind": { "type": "string" }, "name": { "type": "string" }, "version_satisfies": { "type": "string" } } } } } },
                    "ingested_at": { "type": "object", "properties": { "after": { "type": "string", "format": "date" }, "before": { "type": "string", "format": "date" } }, "additionalProperties": false },
                    "source_modified_at": { "type": "object", "properties": { "after": { "type": "string", "format": "date" }, "before": { "type": "string", "format": "date" } }, "additionalProperties": false },
                    "token_count": { "type": "object", "properties": { "min": { "type": "integer" }, "max": { "type": "integer" } }, "additionalProperties": false }
        },
        "description": "Per-facet filters. AND across keys, OR within any_of, exclude none_of. See the `facets` tool for corpus-derived values."
    })
}

fn install_search_skill_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "harness": {
                "type": "array",
                "items": { "type": "string", "enum": ["claude-code", "codex", "opencode", "cursor"] },
                "description": "Harnesses to install for. Omit to auto-detect."
            },
            "scope": {
                "type": "string",
                "enum": ["user", "project"],
                "default": "user",
                "description": "Install scope."
            }
        },
        "additionalProperties": false,
    })
}

/// Parse the tool arguments and run the install against the real process
/// environment. Returns the JSON report as a string on success, or an
/// `(ErrorCode, message)` pair the dispatcher turns into a JSON-RPC error.
///
/// # Errors
///
/// Returns `InvalidParams` for a bad `harness`/`scope`, `ToolFailed` for a
/// filesystem failure or no-harness-detected.
pub fn run_install_search_skill(
    args: &serde_json::Value,
) -> Result<String, (crate::protocol::ErrorCode, String)> {
    run_install_search_skill_in(args, &mn_skills::StdSkillEnv)
}

/// Inner form that takes the [`mn_skills::SkillEnv`] explicitly, so tests can
/// inject a fake home/cwd instead of mutating the global `HOME`.
///
/// # Errors
///
/// As [`run_install_search_skill`].
pub(crate) fn run_install_search_skill_in(
    args: &serde_json::Value,
    env: &impl mn_skills::SkillEnv,
) -> Result<String, (crate::protocol::ErrorCode, String)> {
    use crate::protocol::ErrorCode;
    use mn_skills::{Harness, Scope};
    use std::str::FromStr as _;

    let scope = match args.get("scope") {
        None => Scope::User,
        Some(serde_json::Value::String(s)) => Scope::from_str(s)
            .map_err(|bad| (ErrorCode::InvalidParams, format!("unknown scope `{bad}`")))?,
        Some(_) => return Err((ErrorCode::InvalidParams, "scope must be a string".to_owned())),
    };

    let explicit: Option<Vec<Harness>> = match args.get("harness") {
        None => None,
        Some(serde_json::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let s = item.as_str().ok_or_else(|| {
                    (ErrorCode::InvalidParams, "harness entries must be strings".to_owned())
                })?;
                let h = Harness::from_str(s).map_err(|bad| {
                    (ErrorCode::InvalidParams, format!("unknown harness `{bad}`"))
                })?;
                if !out.contains(&h) {
                    out.push(h);
                }
            }
            if out.is_empty() {
                return Err((ErrorCode::InvalidParams, "harness array was empty".to_owned()));
            }
            Some(out)
        }
        Some(_) => return Err((ErrorCode::InvalidParams, "harness must be an array".to_owned())),
    };

    let report = mn_skills::install(explicit.as_deref(), scope, env)
        .map_err(|e| (ErrorCode::ToolFailed, e.to_string()))?;
    serde_json::to_string(&report)
        .map_err(|e| (ErrorCode::ToolFailed, format!("serialize report: {e}")))
}

// ---------------------------------------------------------------------------
// status (reranker-loaded marker; report assembly lives in crate::status)
// ---------------------------------------------------------------------------

/// Whether a reranker has been loaded into this process (coarse "rerank
/// capability is warm" signal consumed by the `status` report assembler).
pub(crate) fn reranker_loaded() -> bool {
    LOADED_MARKERS.load_relaxed_reranker()
}

mod markers {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Process-wide marker tracking whether *a* reranker has been loaded —
    /// whichever reranker the configured catalog id selects on the first
    /// reranking `search` (local fastembed or remote Voyage). It is a coarse
    /// "rerank capability is warm" signal for `status`, not a model identity.
    pub struct LoadedMarkers {
        reranker: AtomicBool,
    }

    impl LoadedMarkers {
        pub const fn new() -> Self {
            Self {
                reranker: AtomicBool::new(false),
            }
        }

        pub fn mark_reranker(&self) {
            self.reranker.store(true, Ordering::Release);
        }

        pub fn load_relaxed_reranker(&self) -> bool {
            self.reranker.load(Ordering::Acquire)
        }
    }
}

use markers::LoadedMarkers;

pub(crate) static LOADED_MARKERS: LoadedMarkers = LoadedMarkers::new();

/// Process-wide cache for the configured reranker. The reranker can be heavy
/// (a native ONNX cross-encoder downloading ~270 MB) so we load it once per
/// process and reuse it across every reranking `search` — mirroring the prior
/// `reranker::global` behaviour, but for whichever catalog id the config
/// selects. [`LoadedReranker`] is not `Clone`, so we cache it behind an `Arc`.
///
/// CAVEAT: the reranker config is read once, on the first reranking search of
/// the process. A later config change (or a different `MIDNIGHT_MANUAL_RERANKER`
/// env) does NOT take effect until the process restarts — the same single-read
/// semantics the old `bge` singleton had.
static LOADED_RERANKER: OnceCell<Arc<LoadedReranker>> = OnceCell::const_new();

/// Resolve + load the configured reranker, caching it process-wide on first
/// call. Subsequent calls return the cached [`Arc`] regardless of the arguments
/// (the reranker config is read once, on the first reranking search of the
/// process — the same single-read semantics the old `bge` singleton had). On a
/// successful first load this also flips the `status` "reranker loaded"
/// marker.
///
/// `reranker_id` is the catalog id (resolved by the caller via
/// [`mn_core::config::resolve_reranker`]); `reranker_path` backs the `custom`
/// id; `voyage_key` is required for Voyage ids; `voyage_base_url` optionally
/// redirects the Voyage endpoint (self-host / proxy / test mock) and is the
/// env-free seam from PART C. `cache_dir` is the on-disk fastembed model cache.
///
/// Exposed (rather than private) so the reranker selection-and-load contract is
/// unit-testable without driving the whole `search` tool through the
/// environment-reading config layer.
///
/// # Errors
///
/// Returns [`SearchError::Cloud`] if the id is not in the catalog, `custom`
/// lacks a path, or the model fails to load (download / ONNX / missing Voyage
/// key).
pub async fn load_configured_reranker(
    reranker_id: &str,
    reranker_path: Option<&Path>,
    voyage_key: Option<&str>,
    voyage_base_url: Option<&str>,
    cache_dir: &Path,
) -> Result<Arc<LoadedReranker>, SearchError> {
    let loaded = LOADED_RERANKER
        .get_or_try_init(|| async {
            let spec = reranker_catalog::resolve(reranker_id, reranker_path)
                .map_err(|e| SearchError::Cloud(format!("reranker `{reranker_id}`: {e}")))?;
            let loaded =
                LoadedReranker::load(spec, cache_dir.to_path_buf(), voyage_key, voyage_base_url)
                    .await
                    .map_err(|e| {
                        SearchError::Cloud(format!("reranker `{reranker_id}` load failed: {e}"))
                    })?;
            Ok::<_, SearchError>(Arc::new(loaded))
        })
        .await?;
    LOADED_MARKERS.mark_reranker();
    Ok(Arc::clone(loaded))
}

// ---------------------------------------------------------------------------
// search (cloud + local embed + optional local rerank)
// ---------------------------------------------------------------------------

/// Errors `run_search` can produce. Distinguished so the server layer can map
/// them to the right MCP error code.
#[derive(Debug)]
pub enum SearchError {
    /// Cloud returned an embedding-model mismatch — the JSON-RPC error layer
    /// turns this into a typed `EMBEDDING_MODEL_MISMATCH` response carrying
    /// the cloud-provided remediation.
    Mismatch {
        /// Corpus's active `{name}@{revision}`.
        corpus_model: String,
        /// What the client sent.
        client_model: String,
        /// Operator-facing message.
        message: String,
        /// Concrete next step.
        remediation: String,
    },
    /// Catchall cloud / decode / transport failure.
    Cloud(String),
}

const DEFAULT_LIMIT: u32 = 10;
const MAX_LIMIT: u32 = 50;
/// `advanced_search` accepts at most this many query variants (RRF fusion).
const MAX_QUERIES: usize = 10;
/// When rerank is on, we always fetch up to this many candidates from the
/// cloud so the cross-encoder has signal to work with.
const RERANK_FETCH: u32 = 50;

/// Dispatch the `search` / `advanced_search` tools.
///
/// Takes already-parsed arguments (see [`parse_basic_search_args`] /
/// [`parse_advanced_search_args`]; the dispatcher picks the parser by tool
/// name). Embeds each query locally (except in `fts` mode, which skips
/// embedding entirely), posts the resulting `{text, vector}` pairs to the
/// cloud's `/v1/search`, optionally reranks the returned chunks against the
/// first query, and truncates to the caller's `limit`. When rerank is on each
/// returned result gains a `rerank_score` field.
///
/// # Errors
///
/// See [`SearchError`].
pub async fn run_search(
    parsed: &ParsedSearchArgs,
    cfg: &ServerConfig,
    cloud: &Arc<CloudClient>,
) -> Result<serde_json::Value, SearchError> {
    // Resolve the Voyage API key + the reranker selection from env / config
    // (MCP has no CLI flag, so every `flag` is `None`). The base-url override is
    // read through the same `ConfigEnv` abstraction as the other
    // `MIDNIGHT_MANUAL_*` vars — no `std::env` in library code.
    let cfg_env = mn_core::config::StdEnv;
    let (core_cfg, _) = mn_core::config::Config::discover(None, &cfg_env).unwrap_or_default();
    let voyage_key = mn_core::config::resolve_voyage_api_key(None, &core_cfg.models, &cfg_env);
    let rerank_sel = resolve_reranker_selection(&core_cfg.models, &cfg_env);

    // fts mode skips embedding entirely (its whole point): send text-only query
    // pairs with empty vectors and no model label — the cloud ignores both when
    // mode=fts (needs_vector is false server-side). hybrid/vector embed locally
    // (BYOK Voyage or the cloud's /v1/embeddings proxy) and label the request
    // with the corpus's active {name}@{revision}.
    let (pairs, client_embedding_model): (Vec<QueryPair>, String) = if parsed.mode == "fts" {
        let pairs = parsed
            .queries
            .iter()
            .map(|text| QueryPair {
                text: text.clone(),
                vector: Vec::new(),
            })
            .collect();
        (pairs, String::new())
    } else {
        let vectors =
            embed_queries(&parsed.queries, &core_cfg.models, voyage_key.as_deref(), cfg, cloud)
                .await?;
        let pairs = parsed
            .queries
            .iter()
            .zip(vectors.into_iter())
            .map(|(text, vector)| QueryPair { text: text.clone(), vector })
            .collect();
        let model = cloud
            .fetch_active_model()
            .await
            .map_err(|e| SearchError::Cloud(e.to_string()))?;
        (pairs, model)
    };

    // Send to cloud. If rerank is on, ask for a fixed top-K so the reranker
    // has a useful candidate pool independent of the caller's limit.
    let cloud_limit = if parsed.rerank {
        RERANK_FETCH
    } else {
        parsed.limit
    };
    let req = SearchRequest {
        queries: pairs,
        client_embedding_model: client_embedding_model.clone(),
        limit: cloud_limit,
        filters: parsed.filters.clone(),
        // When reranking, fetch the candidate pool in RRF/relevance order so the
        // cross-encoder sees the most relevant chunks (not the cloud's
        // confidence-first default, which could drop relevant-but-low-trust
        // candidates before we rerank). Pass-through (rerank=false) keeps the
        // cloud's confidence ordering.
        sort_by: if parsed.rerank { Some("score") } else { None },
        mode: Some(parsed.mode),
    };
    let cloud_resp = match cloud.search(&req).await {
        Ok(v) => v,
        Err(CloudError::EmbeddingModelMismatch {
            corpus_model,
            client_model,
            message,
            remediation,
        }) => {
            return Err(SearchError::Mismatch {
                corpus_model,
                client_model,
                message,
                remediation,
            });
        }
        Err(e) => return Err(SearchError::Cloud(e.to_string())),
    };

    // Decompose cloud response. `results` is the only field we touch — every
    // other field is passed through verbatim so the response stays additive
    // as the cloud surface evolves.
    let mut envelope = cloud_resp;
    let results = envelope
        .get_mut("results")
        .and_then(|r| r.as_array_mut())
        .map(std::mem::take)
        .unwrap_or_default();

    let final_results = if parsed.rerank && !results.is_empty() {
        rerank_results(
            &parsed.queries,
            results,
            RerankConfig {
                reranker_id: &rerank_sel.reranker_id,
                reranker_path: rerank_sel.reranker_path.as_deref(),
                voyage_key: voyage_key.as_deref(),
                voyage_base_url: rerank_sel.voyage_base_url.as_deref(),
                cache_dir: &cfg.cache_dir,
            },
            parsed.limit,
        )
        .await?
    } else {
        let mut r = results;
        r.truncate(parsed.limit as usize);
        r
    };

    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("results".to_owned(), serde_json::Value::Array(final_results));
        obj.insert(
            "corpus_embedding_model".to_owned(),
            serde_json::Value::String(client_embedding_model),
        );
    }
    Ok(envelope)
}

/// Embed `queries` via VoyageAI, returning one vector per query in order.
/// Uses BYOK (the caller's `voyage_key`, direct to Voyage) when a key is
/// present, else the cloud server's `/v1/embeddings` proxy. There is no local
/// embedder; the only local model is the reranker.
///
/// # Errors
///
/// Returns [`SearchError::Cloud`] on any embedding failure.
async fn embed_queries(
    queries: &[String],
    models: &mn_core::config::ModelsConfig,
    voyage_key: Option<&str>,
    cfg: &ServerConfig,
    cloud: &Arc<CloudClient>,
) -> Result<Vec<Vec<f32>>, SearchError> {
    let embedded = if let Some(key) = voyage_key {
        let v = voyage::VoyageEmbedder::new(
            key,
            &models.embedding,
            models.voyage_output_dimension,
            &models.voyage_output_dtype,
        );
        embed_client::embed(
            queries.to_vec(),
            voyage::InputType::Query,
            embed_client::EmbedSource::Byok(&v),
        )
        .await
    } else {
        embed_client::embed(
            queries.to_vec(),
            voyage::InputType::Query,
            embed_client::EmbedSource::Server {
                base_url: &cfg.cloud_url,
                bearer: cloud.bearer(),
                // Search never opts out of the global cap (read path, not ingest).
                no_global_limit: false,
            },
        )
        .await
    }
    .map_err(|e| SearchError::Cloud(format!("embed failed: {e}")))?;
    Ok(embedded.vectors)
}

/// Validated arguments for `search` / `advanced_search`, produced by
/// [`parse_basic_search_args`] / [`parse_advanced_search_args`] and consumed
/// by [`run_search`].
#[derive(Debug, Clone)]
pub struct ParsedSearchArgs {
    /// 1-10 query texts (basic search always has exactly one).
    pub queries: Vec<String>,
    /// Max results returned to the caller (1..=50).
    pub limit: u32,
    /// Whether to apply cross-encoder reranking (basic search: always true).
    pub rerank: bool,
    /// Pre-validated per-facet filters (basic search: always `None`).
    pub filters: Option<serde_json::Value>,
    /// Retrieval mode (`hybrid` | `vector` | `fts`).
    pub mode: &'static str,
}

/// Parse arguments for the basic `search` tool: `{query, mode?, limit?}`.
/// Advanced-only keys (`queries`, `rerank`, `filters`) are rejected with a
/// pointer at `advanced_search`. `rerank` is fixed to `true`; `filters` is
/// always `None`.
///
/// # Errors
///
/// A human-readable message on any malformed or advanced-only argument.
pub fn parse_basic_search_args(v: &serde_json::Value) -> Result<ParsedSearchArgs, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "arguments must be a JSON object".to_owned())?;

    for key in ["queries", "rerank", "filters"] {
        if obj.contains_key(key) {
            return Err(format!(
                "`{key}` is not supported by `search` — use `advanced_search` for multi-query \
                 fusion, facet filters, and rerank control"
            ));
        }
    }

    let query = match obj.get("query") {
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                return Err("`query` must not be empty".to_owned());
            }
            s.clone()
        }
        Some(_) => return Err("`query` must be a string".to_owned()),
        None => return Err("`query` (string) is required".to_owned()),
    };

    Ok(ParsedSearchArgs {
        queries: vec![query],
        limit: parse_limit_arg(obj)?,
        rerank: true,
        filters: None,
        mode: parse_mode_arg(obj)?,
    })
}

/// Parse arguments for `advanced_search`:
/// `{queries[1-10], mode?, limit?, rerank?, filters?}`. The single-query
/// `query` key is rejected (one query = one-element `queries` array).
///
/// # Errors
///
/// A human-readable message on any malformed argument or invalid filter.
pub fn parse_advanced_search_args(v: &serde_json::Value) -> Result<ParsedSearchArgs, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "arguments must be a JSON object".to_owned())?;

    if obj.contains_key("query") {
        return Err(
            "`query` is not supported by `advanced_search` — pass `queries` (an array of 1-10 \
             strings; one query = one-element array)"
                .to_owned(),
        );
    }

    let queries: Vec<String> = match obj.get("queries") {
        Some(serde_json::Value::Array(arr)) => {
            if arr.is_empty() {
                return Err("`queries` must not be empty".to_owned());
            }
            if arr.len() > MAX_QUERIES {
                return Err(format!("`queries` length must be <= {MAX_QUERIES}"));
            }
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let Some(s) = item.as_str() else {
                    return Err(format!("`queries[{i}]` must be a string"));
                };
                if s.is_empty() {
                    return Err(format!("`queries[{i}]` must not be empty"));
                }
                out.push(s.to_owned());
            }
            out
        }
        Some(_) => return Err("`queries` must be an array of strings".to_owned()),
        None => return Err("`queries` (array of 1-10 strings) is required".to_owned()),
    };

    let rerank = obj
        .get("rerank")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let filters = obj.get("filters").cloned();

    // Validate the filters object against the registry before forwarding (fail fast).
    if let Some(fv) = &filters {
        let parsed: mn_retrieval::filters::SearchFilters =
            serde_json::from_value(fv.clone()).map_err(|e| format!("invalid filters: {e}"))?;
        parsed
            .validate()
            .map_err(|e| format!("invalid filter `{}`: {}", e.facet, e.message))?;
    }

    Ok(ParsedSearchArgs {
        queries,
        limit: parse_limit_arg(obj)?,
        rerank,
        filters,
        mode: parse_mode_arg(obj)?,
    })
}

/// Honour an omitted `limit` as the default; reject any present-but-not-integer
/// value rather than quietly defaulting (silent-default would let callers ship
/// a typo like `limit: "five"` and never notice).
fn parse_limit_arg(obj: &serde_json::Map<String, serde_json::Value>) -> Result<u32, String> {
    match obj.get("limit") {
        None => Ok(DEFAULT_LIMIT),
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err("`limit` must be an integer".to_owned());
            };
            if !(1..=i64::from(MAX_LIMIT)).contains(&n) {
                return Err(format!("`limit` must be 1..={MAX_LIMIT}"));
            }
            Ok(u32::try_from(n).expect("validated above"))
        }
    }
}

/// Parse the shared `mode` argument (`hybrid` default).
fn parse_mode_arg(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Result<&'static str, String> {
    match obj.get("mode") {
        None => Ok("hybrid"),
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "hybrid" => Ok("hybrid"),
            "vector" => Ok("vector"),
            "fts" => Ok("fts"),
            other => Err(format!("unknown mode `{other}` (expected hybrid|vector|fts)")),
        },
        Some(_) => Err("`mode` must be a string".to_owned()),
    }
}

/// The owned reranker selection resolved from config + env, held by
/// `run_search` and borrowed into [`RerankConfig`] at the rerank call site.
struct ResolvedReranker {
    /// Catalog id (flag > env > config; MCP passes `flag = None`).
    reranker_id: String,
    /// Backing dir for the `custom` catalog id; `None` otherwise.
    reranker_path: Option<PathBuf>,
    /// Optional Voyage base-url override (self-host / proxy / test mock).
    voyage_base_url: Option<String>,
}

/// Resolve the reranker catalog id, its `custom` path, and the optional Voyage
/// base-url override from config + env. MCP has no CLI flag, so the id
/// precedence is `MIDNIGHT_MANUAL_RERANKER` env > config. The base-url override
/// reads `MIDNIGHT_MANUAL_VOYAGE_BASE_URL` through the same [`ConfigEnv`] seam
/// as every other `MIDNIGHT_MANUAL_*` var — no `std::env` in library code.
///
/// [`ConfigEnv`]: mn_core::config::ConfigEnv
fn resolve_reranker_selection(
    models: &mn_core::config::ModelsConfig,
    env: &impl mn_core::config::ConfigEnv,
) -> ResolvedReranker {
    ResolvedReranker {
        reranker_id: mn_core::config::resolve_reranker(None, models, env),
        reranker_path: models.reranker_path.clone(),
        voyage_base_url: env
            .var("MIDNIGHT_MANUAL_VOYAGE_BASE_URL")
            .filter(|s| !s.is_empty()),
    }
}

/// The reranker selection threaded from `run_search` into [`rerank_results`].
/// Grouped into a struct so [`rerank_results`] stays under the argument-count
/// lint while still carrying everything [`load_configured_reranker`] needs.
struct RerankConfig<'a> {
    /// Catalog id (resolved via [`mn_core::config::resolve_reranker`]).
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

async fn rerank_results(
    queries: &[String],
    results: Vec<serde_json::Value>,
    cfg: RerankConfig<'_>,
    limit: u32,
) -> Result<Vec<serde_json::Value>, SearchError> {
    // Load (or reuse the process-wide cache of) whichever reranker the config
    // selects — local fastembed (native / onnx / custom) or remote Voyage —
    // instead of the hardcoded `bge` singleton.
    let reranker = load_configured_reranker(
        cfg.reranker_id,
        cfg.reranker_path,
        cfg.voyage_key,
        cfg.voyage_base_url,
        cfg.cache_dir,
    )
    .await?;

    // Use the first query as the rerank pivot. Multi-query / HyDE typically
    // wants the most "user-facing" question to anchor the rerank.
    let pivot = queries.first().map_or(String::new(), String::clone);
    let docs: Vec<String> = results
        .iter()
        .map(|r| {
            r.get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        })
        .collect();
    let scores = reranker
        .rerank(pivot, docs)
        .await
        .map_err(|e| SearchError::Cloud(format!("rerank failed: {e}")))?;

    Ok(rerank_postprocess(results, &scores, limit))
}

/// Attach `rerank_score`, recompute trust-aware `confidence` from the
/// sigmoid-normalized reranker logit, re-sort, and truncate (US6 #8/#12).
///
/// For each reranked result we substitute the normalized cross-encoder score
/// for the relevance term, blend it with the cloud's `trust_score` using the
/// compiled-in default policy weights, and record the substitution in
/// `confidence_factors.relevance_source = "rerank"`. Results are then ordered
/// by the recomputed confidence (descending). Results that carry no cloud
/// `trust_score` (e.g. an older cloud, or `include_scores=false`) keep their
/// confidence and fall back to ordering by the reranker score, so the function
/// degrades gracefully. Pure (no model/IO) so it is unit-testable.
fn rerank_postprocess(
    mut results: Vec<serde_json::Value>,
    scores: &[reranker::RerankResult],
    limit: u32,
) -> Vec<serde_json::Value> {
    let policy = ScoringPolicy::default();
    // Dedupe by index in case the model ever returns the same source index
    // twice (defensive — fastembed shouldn't, but a future swap could).
    let mut seen = std::collections::HashSet::new();
    let mut indexed: Vec<(f64, serde_json::Value)> = scores
        .iter()
        .filter_map(|s| {
            let idx = s.index;
            if idx >= results.len() || !seen.insert(idx) {
                return None;
            }
            let relevance = normalize_rerank(f64::from(s.score));
            let mut taken = std::mem::take(&mut results[idx]);
            let sort_key = recompute_confidence(&mut taken, &policy, f64::from(s.score), relevance);
            Some((sort_key, taken))
        })
        .collect();
    // total_cmp gives a strict total order even with NaN inputs (a NaN would
    // otherwise collapse to Ordering::Equal and produce a non-deterministic
    // sort).
    indexed.sort_by(|a, b| b.0.total_cmp(&a.0));
    indexed.truncate(limit as usize);
    indexed.into_iter().map(|(_, v)| v).collect()
}

/// Patch one result in place: attach the raw `rerank_score` logit, and when the
/// cloud supplied a `scores.trust_score`, recompute `scores.confidence` from
/// `relevance` and stamp `relevance_source`/`relevance_multiplier` into
/// `confidence_factors`. Returns the value to sort by (the recomputed
/// confidence, else the normalized relevance when no trust is available).
fn recompute_confidence(
    result: &mut serde_json::Value,
    policy: &ScoringPolicy,
    raw_logit: f64,
    relevance: f64,
) -> f64 {
    let Some(obj) = result.as_object_mut() else {
        return relevance;
    };
    obj.insert("rerank_score".to_owned(), serde_json::Value::from(raw_logit));

    let trust = obj
        .get("scores")
        .and_then(|s| s.get("trust_score"))
        .and_then(serde_json::Value::as_f64);
    let Some(trust) = trust else {
        // No cloud trust to blend with — leave confidence untouched and order
        // by relevance (monotonic in the reranker logit).
        return relevance;
    };
    let confidence = policy.confidence(trust, relevance);
    if let Some(scores) = obj
        .get_mut("scores")
        .and_then(serde_json::Value::as_object_mut)
    {
        scores.insert("confidence".to_owned(), serde_json::Value::from(confidence));
        if let Some(factors) = scores
            .get_mut("confidence_factors")
            .and_then(serde_json::Value::as_object_mut)
        {
            factors.insert("relevance_source".to_owned(), serde_json::Value::from("rerank"));
            factors.insert("relevance_multiplier".to_owned(), serde_json::Value::from(relevance));
        }
    }
    confidence
}

// ---------------------------------------------------------------------------
// pass-through tools (cloud GET only)
// ---------------------------------------------------------------------------

/// Which cloud endpoint a pass-through tool should hit.
#[derive(Debug, Clone, Copy)]
pub enum PassthroughKind {
    /// `/v1/chunks/:id/parents`
    Parents,
    /// `/v1/documents/:id`
    Document,
}

/// Errors for the chunk pass-through tools.
#[derive(Debug)]
pub enum PassthroughError {
    /// `id` arg missing or malformed.
    InvalidInput(String),
    /// Cloud returned 404.
    NotFound(String),
    /// Cloud / transport / decode failure.
    Cloud(String),
}

/// Direction for `run_chunk_nav` — selects `/next` or `/prev`.
#[derive(Debug, Clone, Copy)]
pub enum ChunkNavDirection {
    /// `/v1/chunks/:id/next`
    Next,
    /// `/v1/chunks/:id/prev`
    Prev,
}

const CHUNK_NAV_DEFAULT_COUNT: u32 = 5;
const CHUNK_NAV_MAX_COUNT: u32 = 100;

/// Dispatch `get_chunk_next` / `get_chunk_prev`. Parses `{id, count?}` and
/// rejects out-of-range or non-integer `count` as `InvalidInput` before the
/// wire call.
///
/// # Errors
///
/// See [`PassthroughError`].
pub async fn run_chunk_nav(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
    dir: ChunkNavDirection,
) -> Result<serde_json::Value, PassthroughError> {
    let obj = args.as_object().ok_or_else(|| {
        PassthroughError::InvalidInput("arguments must be a JSON object".to_owned())
    })?;
    let id_str = obj
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PassthroughError::InvalidInput("`id` (string) is required".to_owned()))?;
    Uuid::parse_str(id_str)
        .map_err(|e| PassthroughError::InvalidInput(format!("`id` is not a valid UUID: {e}")))?;

    let count = match obj.get("count") {
        None => CHUNK_NAV_DEFAULT_COUNT,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(PassthroughError::InvalidInput(
                    "`count` must be an integer".to_owned(),
                ));
            };
            if !(1..=i64::from(CHUNK_NAV_MAX_COUNT)).contains(&n) {
                return Err(PassthroughError::InvalidInput(format!(
                    "`count` must be 1..={CHUNK_NAV_MAX_COUNT}"
                )));
            }
            u32::try_from(n).expect("validated above")
        }
    };

    let r = match dir {
        ChunkNavDirection::Next => cloud.get_chunk_next(id_str, count).await,
        ChunkNavDirection::Prev => cloud.get_chunk_prev(id_str, count).await,
    };
    r.map_err(|e| match e {
        CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
        other => PassthroughError::Cloud(other.to_string()),
    })
}

/// Default chunks on each side of the anchor for `get_chunk_neighbors`. Two is
/// the same default `mnm chunks neighbors` uses on the CLI side — small enough
/// to keep payloads compact, big enough to surround a search hit with context.
const CHUNK_NEIGHBORS_DEFAULT_COUNT: u32 = 2;

/// Dispatch `get_chunk_neighbors`. Parses `{id, count?}` (same shape as
/// `get_chunk_next`/`get_chunk_prev`, but with a smaller default), validates
/// the UUID and range, then asks the cloud client to fan out three parallel
/// requests.
///
/// `count` is applied symmetrically to prev and next.
///
/// # Errors
///
/// See [`PassthroughError`].
pub async fn run_chunk_neighbors(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
) -> Result<serde_json::Value, PassthroughError> {
    let obj = args.as_object().ok_or_else(|| {
        PassthroughError::InvalidInput("arguments must be a JSON object".to_owned())
    })?;
    let id_str = obj
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PassthroughError::InvalidInput("`id` (string) is required".to_owned()))?;
    Uuid::parse_str(id_str)
        .map_err(|e| PassthroughError::InvalidInput(format!("`id` is not a valid UUID: {e}")))?;

    let count = match obj.get("count") {
        None => CHUNK_NEIGHBORS_DEFAULT_COUNT,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(PassthroughError::InvalidInput(
                    "`count` must be an integer".to_owned(),
                ));
            };
            if !(1..=i64::from(CHUNK_NAV_MAX_COUNT)).contains(&n) {
                return Err(PassthroughError::InvalidInput(format!(
                    "`count` must be 1..={CHUNK_NAV_MAX_COUNT}"
                )));
            }
            u32::try_from(n).expect("validated above")
        }
    };

    cloud
        .get_chunk_neighbors(id_str, count, count)
        .await
        .map_err(|e| match e {
            CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
            other => PassthroughError::Cloud(other.to_string()),
        })
}

const DOCUMENT_CHUNKS_DEFAULT_FROM: u32 = 0;
const DOCUMENT_CHUNKS_DEFAULT_LIMIT: u32 = 20;
const DOCUMENT_CHUNKS_MAX_LIMIT: u32 = 100;

/// Dispatch `get_document_chunks`. Parses `{id, from?, limit?}`. `from`
/// must be `>= 0`; `limit` must be in `[1, 100]`. Out-of-range or wrong-type
/// values are rejected as `InvalidInput` before the wire call.
///
/// # Errors
///
/// See [`PassthroughError`].
pub async fn run_document_chunks(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
) -> Result<serde_json::Value, PassthroughError> {
    let obj = args.as_object().ok_or_else(|| {
        PassthroughError::InvalidInput("arguments must be a JSON object".to_owned())
    })?;
    let id_str = obj
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PassthroughError::InvalidInput("`id` (string) is required".to_owned()))?;
    Uuid::parse_str(id_str)
        .map_err(|e| PassthroughError::InvalidInput(format!("`id` is not a valid UUID: {e}")))?;

    let from = match obj.get("from") {
        None => DOCUMENT_CHUNKS_DEFAULT_FROM,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(PassthroughError::InvalidInput("`from` must be an integer".to_owned()));
            };
            if n < 0 {
                return Err(PassthroughError::InvalidInput("`from` must be >= 0".to_owned()));
            }
            u32::try_from(n).map_err(|_| {
                PassthroughError::InvalidInput("`from` exceeds 32-bit range".to_owned())
            })?
        }
    };

    let limit = match obj.get("limit") {
        None => DOCUMENT_CHUNKS_DEFAULT_LIMIT,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(PassthroughError::InvalidInput(
                    "`limit` must be an integer".to_owned(),
                ));
            };
            if !(1..=i64::from(DOCUMENT_CHUNKS_MAX_LIMIT)).contains(&n) {
                return Err(PassthroughError::InvalidInput(format!(
                    "`limit` must be 1..={DOCUMENT_CHUNKS_MAX_LIMIT}"
                )));
            }
            u32::try_from(n).expect("validated above")
        }
    };

    cloud
        .get_document_chunks(id_str, from, limit)
        .await
        .map_err(|e| match e {
            CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
            other => PassthroughError::Cloud(other.to_string()),
        })
}

/// Dispatch the single-id pass-through tools (`get_chunk_parents` /
/// `get_document`). Returns the cloud's JSON verbatim.
///
/// # Errors
///
/// See [`PassthroughError`].
pub async fn run_passthrough_id(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
    kind: PassthroughKind,
) -> Result<serde_json::Value, PassthroughError> {
    let id_str = args
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PassthroughError::InvalidInput("`id` (string) is required".to_owned()))?;
    Uuid::parse_str(id_str)
        .map_err(|e| PassthroughError::InvalidInput(format!("`id` is not a valid UUID: {e}")))?;
    let r = match kind {
        PassthroughKind::Parents => cloud.get_chunk_parents(id_str).await,
        PassthroughKind::Document => cloud.get_document(id_str).await,
    };
    r.map_err(|e| match e {
        CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
        other => PassthroughError::Cloud(other.to_string()),
    })
}

/// Maximum number of ids accepted by one `get_chunks` call.
const GET_CHUNKS_MAX_IDS: usize = 20;

/// Dispatch `get_chunks`. Parses `{ids: [uuid, ..]}` — an array of 1 to
/// `GET_CHUNKS_MAX_IDS` UUID strings — and calls the cloud batch endpoint
/// (`GET /v1/chunks?ids=`). Returns the cloud's `{chunks, missing}` envelope
/// verbatim; unknown ids land in `missing` (the cloud answers 200, not 404).
///
/// # Errors
///
/// See [`PassthroughError`]. All shape violations (missing/empty/oversized
/// array, non-string entry, malformed UUID) are rejected as `InvalidInput`
/// before any wire call.
pub async fn run_get_chunks(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
) -> Result<serde_json::Value, PassthroughError> {
    let arr = args
        .get("ids")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            PassthroughError::InvalidInput(
                "`ids` (array of 1-20 UUID strings) is required".to_owned(),
            )
        })?;
    if arr.is_empty() {
        return Err(PassthroughError::InvalidInput("`ids` must contain at least 1 id".to_owned()));
    }
    if arr.len() > GET_CHUNKS_MAX_IDS {
        return Err(PassthroughError::InvalidInput(format!(
            "`ids` accepts at most {GET_CHUNKS_MAX_IDS} ids per call, got {}",
            arr.len()
        )));
    }
    let mut ids = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let s = v.as_str().ok_or_else(|| {
            PassthroughError::InvalidInput(format!("`ids[{i}]` must be a string"))
        })?;
        Uuid::parse_str(s).map_err(|e| {
            PassthroughError::InvalidInput(format!("`ids[{i}]` is not a valid UUID: {e}"))
        })?;
        ids.push(s.to_owned());
    }
    cloud.get_chunks(&ids).await.map_err(|e| match e {
        CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
        other => PassthroughError::Cloud(other.to_string()),
    })
}

/// Dispatch `list_sources`. Forwards the pagination/filter arguments
/// (`cursor`, `limit`, `created_after`, `created_before`, `kind`, `retired`)
/// to `GET /v1/sources` as query params — only keys present in `args` are
/// sent. Non-string JSON scalars are rendered in their query-string form
/// (`true`, `20`); the input schema forbids object/array values, so no
/// further validation happens here.
///
/// # Errors
///
/// Propagates any [`CloudError`] from the transport / status mapping.
pub async fn run_list_sources(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
) -> Result<serde_json::Value, CloudError> {
    let mut params: Vec<(&str, String)> = Vec::new();
    for key in [
        "cursor",
        "limit",
        "created_after",
        "created_before",
        "kind",
        "retired",
    ] {
        if let Some(v) = args.get(key) {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            params.push((key, s));
        }
    }
    cloud.list_sources(&params).await
}

/// Dispatch `facets`. Forwards the drill-down arguments (`facet`, `cursor`,
/// `limit`) to `GET /v1/facets` as query params — only keys present in
/// `args` are sent, so a no-argument call yields the overview shape.
/// Non-string JSON scalars are rendered in their query-string form (`50`);
/// the input schema forbids object/array values, so no further validation
/// happens here.
///
/// # Errors
///
/// Propagates any [`CloudError`] from the transport / status mapping.
pub async fn run_facets(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
) -> Result<serde_json::Value, CloudError> {
    let mut params: Vec<(&str, String)> = Vec::new();
    for key in ["facet", "cursor", "limit"] {
        if let Some(v) = args.get(key) {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            params.push((key, s));
        }
    }
    cloud.get_facets(&params).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_has_13_tools_with_annotations() {
        let m = list();
        // Exact-order equality pins the canonical registration order AND the
        // exact membership (so retired names like `get_chunk` / `pull_models`
        // cannot reappear without failing here).
        let names: Vec<&str> = m.tools.iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "search",
                "advanced_search",
                "get_chunks",
                "get_chunk_next",
                "get_chunk_prev",
                "get_chunk_neighbors",
                "get_chunk_parents",
                "get_document",
                "get_document_chunks",
                "list_sources",
                "facets",
                "status",
                "install_search_skill",
            ]
        );
        for t in &m.tools {
            let v = serde_json::to_value(t).unwrap();
            assert!(
                v["annotations"]["readOnlyHint"].is_boolean(),
                "{} missing annotations",
                t.name
            );
            // Description hygiene: what/when prose only — no repo paths, no
            // spec/decision numbers (mechanical constraints live in schemas).
            assert!(
                !t.description.contains("docs/"),
                "{} description references a repo path",
                t.name
            );
            assert!(
                !t.description.contains("FR-"),
                "{} description references a spec number",
                t.name
            );
        }
        let install = m
            .tools
            .iter()
            .find(|t| t.name == "install_search_skill")
            .unwrap();
        let v = serde_json::to_value(install).unwrap();
        assert_eq!(v["annotations"]["readOnlyHint"], false);
        assert_eq!(v["annotations"]["idempotentHint"], true);
        assert_eq!(v["annotations"]["destructiveHint"], false);
    }

    #[test]
    fn new_navigation_tools_have_object_schemas() {
        let m = list();
        for name in [
            "get_chunk_next",
            "get_chunk_prev",
            "get_chunk_neighbors",
            "get_document",
            "get_document_chunks",
        ] {
            let tool = m
                .tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("missing tool: {name}"));
            assert_eq!(tool.input_schema["type"], "object", "{name} schema must be object-typed");
            assert_eq!(
                tool.input_schema["additionalProperties"], false,
                "{name} schema must reject additional properties"
            );
        }
    }

    #[test]
    fn parse_basic_accepts_query_mode_limit() {
        let v = json!({ "query": "hello", "limit": 5, "mode": "fts" });
        let p = parse_basic_search_args(&v).unwrap();
        assert_eq!(p.queries, vec!["hello".to_owned()]);
        assert_eq!(p.limit, 5);
        assert_eq!(p.mode, "fts");
        assert!(p.rerank, "basic search must fix rerank to true");
        assert!(p.filters.is_none(), "basic search must carry no filters");
    }

    #[test]
    fn parse_basic_rejects_advanced_only_keys() {
        for args in [
            json!({ "query": "x", "queries": ["y"] }),
            json!({ "queries": ["y"] }),
            json!({ "query": "x", "rerank": false }),
            json!({ "query": "x", "filters": { "kind": { "any_of": ["code"] } } }),
        ] {
            let err = parse_basic_search_args(&args).unwrap_err();
            assert!(err.contains("advanced_search"), "error must point at advanced_search: {err}");
        }
    }

    #[test]
    fn parse_basic_rejects_empty_or_missing_query() {
        assert!(parse_basic_search_args(&json!({})).is_err());
        assert!(parse_basic_search_args(&json!({"query": ""})).is_err());
        assert!(parse_basic_search_args(&json!({"query": 5})).is_err());
    }

    #[test]
    fn parse_advanced_accepts_multi_query() {
        let v = json!({ "queries": ["a", "b", "c"], "rerank": false });
        let p = parse_advanced_search_args(&v).unwrap();
        assert_eq!(p.queries.len(), 3);
        assert_eq!(p.limit, DEFAULT_LIMIT);
        assert!(!p.rerank);
    }

    #[test]
    fn parse_advanced_rerank_defaults_true() {
        let p = parse_advanced_search_args(&json!({ "queries": ["a"] })).unwrap();
        assert!(p.rerank);
    }

    #[test]
    fn parse_advanced_rejects_query_key() {
        let err = parse_advanced_search_args(&json!({ "query": "a" })).unwrap_err();
        assert!(err.contains("queries"), "error must point at `queries`: {err}");
        assert!(parse_advanced_search_args(&json!({ "query": "a", "queries": ["b"] })).is_err());
    }

    #[test]
    fn parse_advanced_accepts_ten_queries_rejects_eleven() {
        let ten: Vec<String> = (0..10).map(|i| format!("q{i}")).collect();
        assert!(parse_advanced_search_args(&json!({ "queries": ten })).is_ok());
        let eleven: Vec<String> = (0..11).map(|i| format!("q{i}")).collect();
        assert!(parse_advanced_search_args(&json!({ "queries": eleven })).is_err());
    }

    #[test]
    fn parse_advanced_rejects_empty() {
        assert!(parse_advanced_search_args(&json!({})).is_err());
        assert!(parse_advanced_search_args(&json!({"queries": []})).is_err());
        assert!(parse_advanced_search_args(&json!({"queries": [""]})).is_err());
        assert!(parse_advanced_search_args(&json!({"queries": [5]})).is_err());
    }

    #[test]
    fn parse_limit_rejects_out_of_range_in_both_parsers() {
        assert!(parse_basic_search_args(&json!({ "query": "x", "limit": 0 })).is_err());
        assert!(parse_basic_search_args(&json!({ "query": "x", "limit": 51 })).is_err());
        assert!(parse_advanced_search_args(&json!({ "queries": ["x"], "limit": 0 })).is_err());
        assert!(parse_advanced_search_args(&json!({ "queries": ["x"], "limit": 51 })).is_err());
    }

    #[test]
    fn search_schemas_are_strict_objects() {
        let basic = search_input_schema();
        assert_eq!(basic["type"], "object");
        assert_eq!(basic["required"], json!(["query"]));
        assert_eq!(basic["additionalProperties"], false);
        assert!(basic["properties"].get("filters").is_none(), "basic must not expose filters");
        assert!(basic["properties"].get("rerank").is_none(), "basic must not expose rerank");

        let advanced = advanced_search_input_schema();
        assert_eq!(advanced["type"], "object");
        assert_eq!(advanced["required"], json!(["queries"]));
        assert_eq!(advanced["additionalProperties"], false);
        assert_eq!(advanced["properties"]["queries"]["maxItems"], MAX_QUERIES);
        assert!(advanced["properties"]["filters"].is_object());
        assert!(advanced["properties"].get("query").is_none(), "advanced must not expose query");

        // The advertised limit bounds must track the parser's constants.
        for schema in [&basic, &advanced] {
            assert_eq!(schema["properties"]["limit"]["maximum"], MAX_LIMIT);
            assert_eq!(schema["properties"]["limit"]["default"], DEFAULT_LIMIT);
        }
    }

    #[test]
    fn parse_rejects_unknown_mode_and_bad_filter() {
        let bad_mode = serde_json::json!({ "query": "x", "mode": "fuzzy" });
        assert!(parse_basic_search_args(&bad_mode).is_err());
        let non_string_mode = serde_json::json!({ "query": "x", "mode": 5 });
        assert!(parse_basic_search_args(&non_string_mode).is_err());
        let bad_filter = serde_json::json!({ "queries": ["x"], "filters": { "kind": { "any_of": ["binary"] } } });
        assert!(parse_advanced_search_args(&bad_filter).is_err());
        let ok = serde_json::json!({ "queries": ["x"], "mode": "fts", "filters": { "kind": { "any_of": ["code"] } } });
        assert!(parse_advanced_search_args(&ok).is_ok());
    }

    fn result_with_trust(chunk: &str, trust: f64) -> serde_json::Value {
        json!({
            "chunk_id": chunk,
            "content": format!("content for {chunk}"),
            "scores": {
                "rrf_score": 0.5,
                "trust_score": trust,
                "confidence": 0.4,
                "confidence_factors": { "relevance_source": "rrf", "relevance_multiplier": 0.4 },
            },
        })
    }

    #[test]
    fn rerank_recomputes_confidence_and_marks_source() {
        // #8/#12: confidence is recomputed from the sigmoid-normalized logit and
        // the substitution is recorded.
        let results = vec![result_with_trust("a", 0.9)];
        let scores = vec![reranker::RerankResult { index: 0, score: 2.0 }];
        let out = rerank_postprocess(results, &scores, 10);
        assert_eq!(out.len(), 1);
        let s = &out[0]["scores"];
        assert_eq!(s["confidence_factors"]["relevance_source"], "rerank");
        // relevance_multiplier == sigmoid(2.0) ≈ 0.8808.
        let rel = s["confidence_factors"]["relevance_multiplier"]
            .as_f64()
            .unwrap();
        assert!((rel - 0.880_797).abs() < 1e-4, "relevance was {rel}");
        // confidence recomputed away from the cloud's 0.4 placeholder.
        let conf = s["confidence"].as_f64().unwrap();
        assert!((conf - 0.4).abs() > 1e-6 && (0.0..=1.0).contains(&conf));
        assert!(out[0]["rerank_score"].as_f64().unwrap() > 1.99);
    }

    #[test]
    fn rerank_orders_by_recomputed_confidence_not_logit() {
        // High-trust chunk with a slightly lower logit should still outrank a
        // low-trust chunk with a slightly higher logit, because confidence
        // blends trust in.
        let results = vec![
            result_with_trust("low_trust", 0.05),
            result_with_trust("high_trust", 0.99),
        ];
        let scores = vec![
            reranker::RerankResult { index: 0, score: 1.2 }, // low trust, higher logit
            reranker::RerankResult { index: 1, score: 1.0 }, // high trust, lower logit
        ];
        let out = rerank_postprocess(results, &scores, 10);
        assert_eq!(out[0]["chunk_id"], "high_trust", "trust must lift the top result");
        assert_eq!(out[1]["chunk_id"], "low_trust");
    }

    #[test]
    fn rerank_passes_through_without_trust() {
        // A result missing scores.trust_score keeps its confidence and just
        // gains a rerank_score (graceful degradation).
        let results = vec![json!({ "chunk_id": "x", "content": "c" })];
        let scores = vec![reranker::RerankResult { index: 0, score: 0.5 }];
        let out = rerank_postprocess(results, &scores, 10);
        assert_eq!(out.len(), 1);
        assert!(out[0].get("scores").is_none());
        assert!(out[0]["rerank_score"].is_number());
    }

    #[test]
    fn rerank_truncates_to_limit() {
        let results = vec![
            result_with_trust("a", 0.8),
            result_with_trust("b", 0.7),
            result_with_trust("c", 0.6),
        ];
        let scores = vec![
            reranker::RerankResult { index: 0, score: 0.1 },
            reranker::RerankResult { index: 1, score: 0.9 },
            reranker::RerankResult { index: 2, score: 0.5 },
        ];
        let out = rerank_postprocess(results, &scores, 2);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn every_tool_advertises_output_schema() {
        for t in list().tools {
            assert!(t.output_schema.is_some(), "tool {} missing outputSchema", t.name);
        }
    }
}

#[cfg(test)]
mod install_skill_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn manifest_includes_install_search_skill() {
        assert!(list()
            .tools
            .iter()
            .any(|t| t.name == "install_search_skill"));
    }

    #[test]
    fn install_rejects_bad_scope() {
        let args = json!({ "scope": "global" });
        let err = run_install_search_skill(&args).unwrap_err();
        assert!(matches!(err.0, crate::protocol::ErrorCode::InvalidParams));
    }

    #[test]
    fn install_rejects_unknown_harness() {
        let args = json!({ "harness": ["windsurf"] });
        let err = run_install_search_skill(&args).unwrap_err();
        assert!(matches!(err.0, crate::protocol::ErrorCode::InvalidParams));
    }

    #[test]
    fn install_with_no_harness_detected_gives_tool_failed() {
        struct EmptyEnv {
            home: std::path::PathBuf,
        }
        impl mn_skills::SkillEnv for EmptyEnv {
            fn home_dir(&self) -> Option<std::path::PathBuf> {
                Some(self.home.clone())
            }
            fn current_dir(&self) -> Option<std::path::PathBuf> {
                Some(self.home.clone())
            }
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let env = EmptyEnv { home: tmp.path().to_path_buf() };
        // No markers under the temp home -> auto-detect finds nothing ->
        // mn_skills::install returns NoHarnessDetected -> mapped to ToolFailed.
        let err = run_install_search_skill_in(&json!({}), &env).unwrap_err();
        assert!(matches!(err.0, crate::protocol::ErrorCode::ToolFailed));
    }

    #[test]
    fn install_writes_into_injected_fake_home() {
        // No global env mutation: inject a fake SkillEnv pointing at a tempdir.
        struct FakeEnv {
            home: std::path::PathBuf,
        }
        impl mn_skills::SkillEnv for FakeEnv {
            fn home_dir(&self) -> Option<std::path::PathBuf> {
                Some(self.home.clone())
            }
            fn current_dir(&self) -> Option<std::path::PathBuf> {
                Some(self.home.clone())
            }
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf() };
        let text =
            run_install_search_skill_in(&json!({ "harness": ["cursor"], "scope": "user" }), &env)
                .expect("install ok");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["installed"][0]["harness"], "cursor");
        assert!(tmp
            .path()
            .join(".cursor/skills/midnight-advanced-search/SKILL.md")
            .exists());
    }
}

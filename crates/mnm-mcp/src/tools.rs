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

use std::sync::Arc;

use mnm_core::scoring_policy::ScoringPolicy;
use mnm_embedding::{client as embed_client, contextualized, reranker, voyage};
use serde_json::json;
use uuid::Uuid;

use crate::cloud_client::{CloudClient, CloudError, QueryPair, SearchRequest};
use crate::protocol::{ToolAnnotations, ToolDescription, ToolsListResult};
use crate::server::ServerConfig;

/// Build the static tool manifest sent in response to `tools/list`.
///
/// All thirteen tools, mirrored by the `tests/contract/mcp-tools.json`
/// snapshot, in canonical registration order (search pair, chunk reads,
/// document reads,
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
                    "Search the Midnight Network documentation and code corpus (docs, SDK references, Compact language material, code examples). Returns ranked excerpts with confidence scores and source attribution. Use it whenever you need facts about Midnight, Compact, or the Midnight SDK. Code-heavy queries (function names, API signatures, error strings from code) benefit from code_mode=exclusive; conceptual queries should keep the default. For multi-query strategies, facet filters, or rerank control, use advanced_search.",
                input_schema: search_input_schema(),
                output_schema: Some(crate::schemas::search_output_schema()),
                annotations: ToolAnnotations::read_only().with_title("Search corpus"),
            },
            ToolDescription {
                name: "advanced_search",
                description:
                    "Full-control search over the Midnight corpus: fuse multiple queries (HyDE, expansion, step-back), restrict by facet filters, switch retrieval mode, and toggle reranking. Use when basic search comes up short or when the midnight-advanced-search skill prescribes a pattern. Call facets first to discover valid filter values.",
                input_schema: advanced_search_input_schema(),
                output_schema: Some(crate::schemas::search_output_schema()),
                annotations: ToolAnnotations::read_only().with_title("Advanced search"),
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
                annotations: ToolAnnotations::read_only().with_title("Fetch chunks"),
            },
            ToolDescription {
                name: "get_chunk_next",
                description:
                    "Fetch chunks that immediately follow a given chunk in its document's reading order. Use to continue reading past the end of a chunk you already have.",
                input_schema: chunk_nav_schema(),
                output_schema: Some(crate::schemas::chunk_list_output_schema()),
                annotations: ToolAnnotations::read_only().with_title("Next chunks"),
            },
            ToolDescription {
                name: "get_chunk_prev",
                description:
                    "Fetch chunks that immediately precede a given chunk in its document's reading order. Use to read the context leading up to a chunk you already have.",
                input_schema: chunk_nav_schema(),
                output_schema: Some(crate::schemas::chunk_list_output_schema()),
                annotations: ToolAnnotations::read_only().with_title("Previous chunks"),
            },
            ToolDescription {
                name: "get_chunk_neighbors",
                description:
                    "Fetch the chunks immediately before and after a given chunk in one call. Use when a search hit needs surrounding context to be understood.",
                input_schema: chunk_neighbors_schema(),
                output_schema: Some(crate::schemas::neighbors_output_schema()),
                annotations: ToolAnnotations::read_only().with_title("Surrounding chunks"),
            },
            ToolDescription {
                name: "get_chunk_parents",
                description:
                    "Show where a chunk sits in its source's structure: the chain of containing nodes (document, folders) up to the source root. Use to orient a chunk within its source and find its containing document.",
                input_schema: id_only_schema(),
                output_schema: Some(crate::schemas::parents_output_schema()),
                annotations: ToolAnnotations::read_only().with_title("Chunk ancestry"),
            },
            ToolDescription {
                name: "get_document",
                description:
                    "Fetch a document's metadata plus an ordered skeleton of its chunks (ids, positions, token counts — no bodies). Use to size up a document before reading it with get_document_chunks.",
                input_schema: id_only_schema(),
                output_schema: Some(crate::schemas::document_output_schema()),
                annotations: ToolAnnotations::read_only().with_title("Document overview"),
            },
            ToolDescription {
                name: "get_document_chunks",
                description:
                    "Read a window of a document's chunk bodies by position. Use after get_document to read a document section by section.",
                input_schema: document_chunks_schema(),
                output_schema: Some(crate::schemas::document_window_output_schema()),
                annotations: ToolAnnotations::read_only().with_title("Read document"),
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
                annotations: ToolAnnotations::read_only().with_title("List sources"),
            },
            ToolDescription {
                name: "facets",
                description:
                    "Discover the filter dimensions available to advanced_search and the values present in the corpus. Call without arguments for an overview; pass a facet name to page through all values of one dimension.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "facet": { "type": "string", "enum": ["source_slug", "language", "tags", "package", "language_target", "sdk_dependency"],
                            "description": "Drill into one open-set facet's full value list. Omit for the overview." },
                        "within": { "type": "string", "description": "Second drill level: enumerate the declared version constraints within one name (language_target/sdk_dependency) or one package name. These are the values you supply to advanced_search via a filter's version_satisfies field, where they are matched as a semver requirement against the declared constraint." },
                        "cursor": { "type": "string", "description": "Opaque token from a previous drill-down response." },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
                    },
                    "additionalProperties": false,
                }),
                output_schema: Some(crate::schemas::facets_output_schema()),
                annotations: ToolAnnotations::read_only().with_title("Discover facets"),
            },
            ToolDescription {
                name: "status",
                description:
                    "Diagnose the retrieval setup: cloud reachability, authentication and rate-limit state, VoyageAI key validity, and rerank configuration. Call when searches fail, return errors, or before starting a long session.",
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                output_schema: Some(crate::schemas::status_output_schema()),
                annotations: ToolAnnotations::read_only().with_title("Diagnostics"),
            },
            ToolDescription {
                name: "install_search_skill",
                description:
                    "Install (or update) the midnight-advanced-search skill — a retrieval playbook teaching effective corpus search patterns — into the user's AI harness(es). Use when search results are poor or the user asks for better search guidance.",
                input_schema: install_search_skill_schema(),
                output_schema: Some(crate::schemas::install_output_schema()),
                annotations: ToolAnnotations::idempotent_writer().with_title("Install search skill"),
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
            "code_mode": code_mode_schema(),
            "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10,
                "description": "Max results returned." }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

/// The shared `code_mode` property schema (dual embeddings), referenced by
/// both `search` and `advanced_search`.
fn code_mode_schema() -> serde_json::Value {
    json!({ "type": "string", "enum": ["on", "off", "exclusive"],
        "description": "Code-vector fusion (dual embeddings): on (default for hybrid/vector) fuses a voyage-code-3 ranked list alongside the general results; off = general retrieval only; exclusive = code vectors replace the general vector list (best for API-shaped / code-identifier queries). Incompatible with mode=fts." })
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
            "code_mode": code_mode_schema(),
            "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10,
                "description": "Max results returned." },
            "rerank": { "type": "boolean", "default": true,
                "description": "Apply VoyageAI reranking against the first query (server-side, or locally with your own VOYAGE_API_KEY). Disable for lowest latency." },
            "version_match": { "type": "string", "enum": ["strict", "permissive"], "default": "permissive",
                "description": "Version-filter semantics: permissive (default) biases ranking and drops only breaking mismatches among version-declaring content; strict hard-filters to satisfying content only." },
            "rerank_instructions": {
                "type": "string",
                "maxLength": 400,
                "description": "Optional rerank instruction (max 400 chars). Guides relevance: emphasize aspects, filter document kinds, or disambiguate terms. Replaces the derived default instruction. Keep it terse — instruction tokens are multiplied by the candidate-pool size. See the midnight-advanced-search skill for guidance."
            },
            "filters": filters_schema()
        },
        "required": ["queries"],
        "additionalProperties": false
    })
}

/// The per-facet `filters` schema, referenced only by `advanced_search`.
#[allow(clippy::too_many_lines)]
fn filters_schema() -> serde_json::Value {
    use mnm_retrieval::facets;
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
    run_install_search_skill_in(args, &mnm_skills::StdSkillEnv)
}

/// Inner form that takes the [`mnm_skills::SkillEnv`] explicitly, so tests can
/// inject a fake home/cwd instead of mutating the global `HOME`.
///
/// # Errors
///
/// As [`run_install_search_skill`].
pub(crate) fn run_install_search_skill_in(
    args: &serde_json::Value,
    env: &impl mnm_skills::SkillEnv,
) -> Result<String, (crate::protocol::ErrorCode, String)> {
    use crate::protocol::ErrorCode;
    use mnm_skills::{Harness, Scope};
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

    let report = mnm_skills::install(explicit.as_deref(), scope, env)
        .map_err(|e| (ErrorCode::ToolFailed, e.to_string()))?;
    serde_json::to_string(&report)
        .map_err(|e| (ErrorCode::ToolFailed, format!("serialize report: {e}")))
}

// ---------------------------------------------------------------------------
// status (reranker-loaded marker; report assembly lives in crate::status)
// ---------------------------------------------------------------------------

/// Whether reranking has been exercised in this process (coarse "rerank
/// capability is warm" signal consumed by the `status` report assembler).
pub(crate) fn reranker_loaded() -> bool {
    LOADED_MARKERS.load_relaxed_reranker()
}

mod markers {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Process-wide marker tracking whether reranking has been *exercised* —
    /// flipped on the first successful local `VoyageAI` rerank in a `search`.
    /// It is a coarse "rerank capability is warm" signal for `status`, not a
    /// model identity.
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

/// Resolve the effective rerank placement, model, and Voyage base-url override
/// for one search, applying the local-without-key guard.
///
/// Placement resolves with precedence env > config (MCP has no flag); `auto`
/// picks local BYOK with a Voyage key, else server (D6). The tool's
/// `rerank: false` forces [`RerankPlacement::Off`] regardless of placement. A
/// `local` placement with no key cannot rerank, so it is surfaced as a
/// [`SearchError::Cloud`] before any embedding / network work (auto never lands
/// here — it resolves to server without a key). The base-url override reads
/// `MIDNIGHT_MANUAL_VOYAGE_BASE_URL` through the same `ConfigEnv` seam as every
/// other `MIDNIGHT_MANUAL_*` var — no `std::env` in library code.
///
/// [`RerankPlacement::Off`]: mnm_core::config::RerankPlacement::Off
///
/// # Errors
///
/// Returns [`SearchError::Cloud`] when placement is `local` but no Voyage key
/// is configured.
fn resolve_rerank_for_search(
    parsed: &ParsedSearchArgs,
    rerank_cfg: &mnm_core::config::RerankConfig,
    models_cfg: &mnm_core::config::ModelsConfig,
    voyage_key: Option<&str>,
    env: &impl mnm_core::config::ConfigEnv,
) -> Result<
    (mnm_core::config::RerankPlacement, mnm_core::rerank::RerankParam, Option<String>),
    SearchError,
> {
    use mnm_core::config::RerankPlacement;
    let placement =
        mnm_core::config::resolve_rerank_placement(None, rerank_cfg, env, voyage_key.is_some())
            .map_err(|e| SearchError::Cloud(e.to_string()))?;
    let rerank_model = mnm_core::config::resolve_rerank_model(None, rerank_cfg, env)
        .map_err(|e| SearchError::Cloud(e.to_string()))?;
    let voyage_base_url = mnm_core::config::resolve_voyage_base_url(models_cfg, env);
    let effective = if parsed.rerank {
        placement
    } else {
        RerankPlacement::Off
    };
    if matches!(effective, RerankPlacement::Local) && voyage_key.is_none() {
        return Err(SearchError::Cloud(
            "rerank location is 'local' but no Voyage API key is configured".to_owned(),
        ));
    }
    Ok((effective, rerank_model, voyage_base_url))
}

/// What the rerank stage did, for the FR-109 `Rerank` telemetry event
/// (spec §6). Carried out of [`run_search`] alongside the response envelope so
/// the dispatcher can emit one event per search.
#[derive(Debug, Clone, Default)]
pub struct RerankFacts {
    /// Resolved placement wire string (`"local"` | `"server"` | `"off"`).
    pub placement: &'static str,
    /// Model attempted/applied; `None` on the `off` placement.
    pub model: Option<String>,
    /// Whether a rerank was actually applied to the result set (local pass ran,
    /// or the server reported `search_metadata.rerank.applied`).
    pub applied: bool,
    /// Degrade reason when not applied (server path only; mirrors
    /// `search_metadata.rerank.reason`).
    pub reason: Option<String>,
    /// Billed-equivalent tokens for a local rerank (`total_tokens` through
    /// [`mnm_core::rerank::RerankParam::billed_tokens`]); `None` on the server /
    /// off paths (the server tracks its own metrics).
    pub billed_tokens: Option<u64>,
}

/// A successful [`run_search`] result: the response envelope plus the rerank
/// facts the dispatcher needs for the `Rerank` telemetry event.
#[derive(Debug)]
pub struct SearchSuccess {
    /// The `/v1/search` response envelope (results + passthrough metadata).
    pub envelope: serde_json::Value,
    /// What the rerank stage did (spec §6).
    pub rerank: RerankFacts,
}

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
/// Returns the response envelope plus the [`RerankFacts`] for the `Rerank`
/// telemetry event (spec §6).
///
/// # Errors
///
/// See [`SearchError`].
pub async fn run_search(
    parsed: &ParsedSearchArgs,
    cfg: &ServerConfig,
    cloud: &Arc<CloudClient>,
) -> Result<SearchSuccess, SearchError> {
    // Resolve the Voyage API key from env / config (MCP has no CLI flag, so
    // every `flag` is `None`).
    let cfg_env = mnm_core::config::StdEnv;
    let (core_cfg, _) = mnm_core::config::Config::discover(None, &cfg_env).unwrap_or_default();
    let voyage_key = mnm_core::config::resolve_voyage_api_key(None, &core_cfg.models, &cfg_env);

    // Resolve the rerank placement/model/base-url and apply the local-without-key
    // guard before any embedding / network work.
    let (effective, rerank_model, voyage_base_url) = resolve_rerank_for_search(
        parsed,
        &core_cfg.rerank,
        &core_cfg.models,
        voyage_key.as_deref(),
        &cfg_env,
    )?;

    // fts mode skips embedding entirely (its whole point): send text-only query
    // pairs with empty vectors and no model label — the cloud ignores both when
    // mode=fts (needs_vector is false server-side). hybrid/vector embed locally
    // (BYOK Voyage or the cloud's /v1/embeddings proxy) and label the request
    // with the corpus's active {name}@{revision}.
    let (pairs, client_embedding_model, code_wire): (Vec<QueryPair>, String, Option<String>) =
        if parsed.mode == "fts" {
            let pairs = parsed
                .queries
                .iter()
                .map(|text| QueryPair {
                    text: text.clone(),
                    vector: Vec::new(),
                    code_vector: Vec::new(),
                })
                .collect();
            (pairs, String::new(), None)
        } else {
            build_embedded_pairs(parsed, &core_cfg.models, voyage_key.as_deref(), cfg, cloud)
                .await?
        };

    // Size the candidate pool + the `rerank` wire parameter for the resolved
    // placement (see [`build_search_request`]). `local` selects the client-side
    // rerank path below.
    let local = matches!(effective, mnm_core::config::RerankPlacement::Local);
    let req = build_search_request(
        parsed,
        effective,
        rerank_model,
        pairs,
        client_embedding_model.clone(),
        code_wire.clone(),
    );
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

    // Only the Local placement reranks client-side; Server already reranked
    // inline, Off didn't rerank at all — both just truncate to the limit. The
    // rerank facts feed the FR-109 `Rerank` event (spec §6): the Local path
    // knows `applied` + `billed_tokens` first-hand; the Server path reads the
    // cloud's `search_metadata.rerank` outcome; Off is never applied.
    let (final_results, rerank) = if local && !results.is_empty() {
        let key = voyage_key.as_deref().ok_or_else(|| {
            SearchError::Cloud("local rerank reached without a Voyage key".to_owned())
        })?;
        let (reranked, total_tokens) = rerank_results(
            parsed,
            results,
            key,
            // `resolve_rerank_model` never yields `RerankParam::None` (placement
            // owns "off"), so `model_name()` is always `Some` here — the single
            // source of truth for the default stays in `mnm_core::rerank`.
            rerank_model
                .model_name()
                .expect("resolve_rerank_model never yields RerankParam::None"),
            voyage_base_url.as_deref(),
            parsed.limit,
        )
        .await?;
        let facts = RerankFacts {
            placement: effective.wire(),
            model: rerank_model.model_name().map(str::to_owned),
            applied: true,
            reason: None,
            billed_tokens: Some(rerank_model.billed_tokens(total_tokens)),
        };
        (reranked, facts)
    } else {
        let mut r = results;
        r.truncate(parsed.limit as usize);
        (r, no_local_rerank_facts(effective, rerank_model, &envelope))
    };

    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("results".to_owned(), serde_json::Value::Array(final_results));
        obj.insert(
            "corpus_embedding_model".to_owned(),
            serde_json::Value::String(client_embedding_model),
        );
        // Report the code model only when code search actually ran.
        if let Some(code) = code_wire {
            obj.insert("corpus_code_embedding_model".to_owned(), serde_json::Value::String(code));
        }
    }
    Ok(SearchSuccess { envelope, rerank })
}

/// Build the [`RerankFacts`] for a search that did *not* rerank client-side
/// (Server placement, Off placement, or Local with an empty result set). On the
/// Server placement the cloud performed the rerank, so `applied` / `reason` are
/// read from the response's `search_metadata.rerank` (spec §6); otherwise the
/// rerank was not applied. `billed_tokens` is always `None` here — only the
/// local path knows Voyage's reported tokens.
fn no_local_rerank_facts(
    effective: mnm_core::config::RerankPlacement,
    rerank_model: mnm_core::rerank::RerankParam,
    envelope: &serde_json::Value,
) -> RerankFacts {
    use mnm_core::config::RerankPlacement;
    let (applied, reason) = if matches!(effective, RerankPlacement::Server) {
        let meta = envelope
            .get("search_metadata")
            .and_then(|m| m.get("rerank"));
        let applied = meta
            .and_then(|r| r.get("applied"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        // Route the server reason through the closed allow-list so only the
        // documented set can land in a `Rerank` event (privacy invariant);
        // arbitrary server text is dropped.
        let reason = meta
            .and_then(|r| r.get("reason"))
            .and_then(serde_json::Value::as_str)
            .and_then(mnm_core::rerank::known_reason)
            .map(str::to_owned);
        (applied, reason)
    } else {
        (false, None)
    };
    RerankFacts {
        placement: effective.wire(),
        model: rerank_event_model(effective, rerank_model),
        applied,
        reason,
        billed_tokens: None,
    }
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

/// Build the outgoing `/v1/search` body, sizing the candidate pool and the
/// `rerank` wire parameter for the resolved placement.
///
/// The Local path widens the cloud `limit` to [`RERANK_FETCH`] in relevance
/// order (`sort_by = "score"`) so the client-side reranker can *promote* a chunk
/// the cloud ranked below the caller's limit — not merely reorder the top-N
/// (mirrors the CLI); [`rerank_results`] later truncates back to the limit.
/// Server/Off use the caller's limit with the cloud's confidence ordering.
/// Server sends the resolved model name (+ instructions); Local/Off send `none`
/// (exactly one rerank pass, structurally — Local reranks client-side).
fn build_search_request(
    parsed: &ParsedSearchArgs,
    effective: mnm_core::config::RerankPlacement,
    rerank_model: mnm_core::rerank::RerankParam,
    queries: Vec<QueryPair>,
    client_embedding_model: String,
    code_wire: Option<String>,
) -> SearchRequest {
    use mnm_core::config::RerankPlacement;
    let local = matches!(effective, RerankPlacement::Local);
    let (cloud_limit, sort_by) = if local {
        (RERANK_FETCH, Some("score"))
    } else {
        (parsed.limit, None)
    };
    let (rerank, rerank_instructions) = match effective {
        RerankPlacement::Server => {
            (rerank_model.model_name().map(str::to_owned), parsed.rerank_instructions.clone())
        }
        RerankPlacement::Local | RerankPlacement::Off => (Some("none".to_owned()), None),
    };
    SearchRequest {
        queries,
        client_embedding_model,
        limit: cloud_limit,
        filters: parsed.filters.clone(),
        sort_by,
        mode: Some(parsed.mode),
        // Forward only an explicit caller choice; `None` lets the cloud apply
        // its mode-derived default (on for hybrid/vector, off for fts).
        code_mode: parsed.code_mode,
        client_code_embedding_model: code_wire,
        rerank,
        rerank_instructions,
        version_match: parsed.version_match.clone(),
    }
}

/// Embed the parsed queries for hybrid/vector modes and assemble the
/// `QueryPair`s. Dual embeddings (§11.2): the general (voyage-context-3) half
/// is skipped when `code_mode=exclusive` and the code (voyage-code-3) half
/// when `code_mode=off` — mirroring the cloud's effective-mode rules without
/// sniffing queries. Returns `(pairs, general_wire_id, code_wire_id)`, where
/// the code wire id is `Some` exactly when code search ran.
///
/// # Errors
///
/// Returns [`SearchError::Cloud`] on any embedding or active-model failure.
async fn build_embedded_pairs(
    parsed: &ParsedSearchArgs,
    models: &mnm_core::config::ModelsConfig,
    voyage_key: Option<&str>,
    cfg: &ServerConfig,
    cloud: &Arc<CloudClient>,
) -> Result<(Vec<QueryPair>, String, Option<String>), SearchError> {
    use mnm_core::embedder_identity::{derive, ActiveModelIdentity, FallbackIdentity};

    let embed_code_q = parsed.code_mode != Some("off");
    let embed_general_q = parsed.code_mode != Some("exclusive");
    let n = parsed.queries.len();

    // Fetch the corpus's active model FIRST so the embedders are built from the
    // SAME source that labels the resulting vectors (cross-element drift fix).
    let active = cloud
        .fetch_active_model()
        .await
        .map_err(|e| SearchError::Cloud(e.to_string()))?;

    // Derive embedder identities from the active fetch (the authority); config
    // is only the logged fallback. The active fetch always succeeds here (we
    // `?`-returned above), so these always take the active path.
    let general_id = derive(
        "general",
        Some(&ActiveModelIdentity {
            name: active.general.name.clone(),
            dim: active.general.dim,
            dtype: active.general.dtype.clone(),
        }),
        &FallbackIdentity {
            name: &models.embedding,
            dim: models.voyage_output_dimension,
            dtype: &models.voyage_output_dtype,
        },
    );
    let code_active = active.code.as_ref().map(|c| ActiveModelIdentity {
        name: c.name.clone(),
        dim: c.dim,
        dtype: c.dtype.clone(),
    });
    let code_id = derive(
        "code",
        code_active.as_ref(),
        &FallbackIdentity {
            name: &models.code_embedding,
            dim: models.voyage_output_dimension,
            dtype: &models.voyage_output_dtype,
        },
    );

    let general_vectors = if embed_general_q {
        embed_general_queries(&parsed.queries, &general_id, voyage_key, cfg, cloud).await?
    } else {
        vec![Vec::new(); n]
    };
    let code_vectors = if embed_code_q {
        embed_code_queries(&parsed.queries, &code_id, voyage_key, cfg, cloud).await?
    } else {
        vec![Vec::new(); n]
    };
    // The code wire id pins code_vectors to a model. Prefer the
    // corpus-reported id; fall back to the config-pinned model at revision 1
    // when the server didn't report one (the cloud then rejects the request
    // with a precise mismatch if it disagrees).
    let code_wire = embed_code_q.then(|| {
        active
            .code
            .as_ref()
            .map_or_else(|| format!("{}@1", models.code_embedding), |c| c.wire.clone())
    });
    let pairs = parsed
        .queries
        .iter()
        .zip(general_vectors)
        .zip(code_vectors)
        .map(|((text, vector), code_vector)| QueryPair {
            text: text.clone(),
            vector,
            code_vector,
        })
        .collect();
    Ok((pairs, active.general.wire, code_wire))
}

/// Embed `queries` with the GENERAL model (voyage-context-3), returning one
/// vector per query in order. Uses BYOK (the caller's `voyage_key`, direct to
/// Voyage's contextualized endpoint) when a key is present, else the cloud
/// server's `/v1/embeddings` proxy with `type=general`. There is no local
/// embedder; the only local model is the reranker.
///
/// The embedder `name`/`dim`/`dtype` come from `identity`, derived from the
/// corpus's active model (see [`build_embedded_pairs`]) so the model computing
/// these vectors matches the wire id labelling them.
///
/// # Errors
///
/// Returns [`SearchError::Cloud`] on any embedding failure.
async fn embed_general_queries(
    queries: &[String],
    identity: &mnm_core::embedder_identity::EmbedderIdentity,
    voyage_key: Option<&str>,
    cfg: &ServerConfig,
    cloud: &Arc<CloudClient>,
) -> Result<Vec<Vec<f32>>, SearchError> {
    let embedded = if let Some(key) = voyage_key {
        let e = contextualized::ContextualizedVoyageEmbedder::new(
            key,
            &identity.name,
            identity.dim,
            &identity.dtype,
        );
        embed_client::embed_general(
            queries.to_vec(),
            voyage::InputType::Query,
            embed_client::GeneralEmbedSource::Byok(&e),
        )
        .await
    } else {
        embed_client::embed_general(
            queries.to_vec(),
            voyage::InputType::Query,
            embed_client::GeneralEmbedSource::Server {
                base_url: &cfg.cloud_url,
                bearer: cloud.bearer(),
                // Search never opts out of the global cap (read path, not ingest).
                no_global_limit: false,
            },
        )
        .await
    }
    .map_err(|e| SearchError::Cloud(format!("embed general failed: {e}")))?;
    Ok(embedded.vectors)
}

/// Embed `queries` with the CODE model (voyage-code-3, flat endpoint),
/// returning one vector per query in order. BYOK hits Voyage's flat endpoint
/// directly; otherwise the cloud's `/v1/embeddings` proxy with `type=code`.
///
/// The embedder `name`/`dim`/`dtype` come from `identity`, derived from the
/// active model's `code` half (see [`build_embedded_pairs`]) so the model
/// computing these vectors matches the code wire id labelling them.
///
/// # Errors
///
/// Returns [`SearchError::Cloud`] on any embedding failure.
async fn embed_code_queries(
    queries: &[String],
    identity: &mnm_core::embedder_identity::EmbedderIdentity,
    voyage_key: Option<&str>,
    cfg: &ServerConfig,
    cloud: &Arc<CloudClient>,
) -> Result<Vec<Vec<f32>>, SearchError> {
    let embedded = if let Some(key) = voyage_key {
        let v = voyage::VoyageEmbedder::new(key, &identity.name, identity.dim, &identity.dtype);
        embed_client::embed_code(
            queries.to_vec(),
            voyage::InputType::Query,
            embed_client::EmbedSource::Byok(&v),
        )
        .await
    } else {
        embed_client::embed_code(
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
    .map_err(|e| SearchError::Cloud(format!("embed code failed: {e}")))?;
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
    /// Code-vector fusion mode (`on` | `off` | `exclusive`). `None` = caller
    /// didn't choose; the cloud applies its mode-derived default (on for
    /// hybrid/vector, off for fts).
    pub code_mode: Option<&'static str>,
    /// Agent-supplied rerank instruction (max 400 chars), validated at parse
    /// time. Replaces the derived default on whichever placement reranks.
    /// `None` (always, for basic search) defers to the derived default.
    pub rerank_instructions: Option<String>,
    /// Version-matching mode (`strict` | `permissive`), validated at parse time
    /// (basic search: always `None`). `None` defers to the server default
    /// (`permissive`).
    pub version_match: Option<String>,
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

    let mode = parse_mode_arg(obj)?;
    Ok(ParsedSearchArgs {
        queries: vec![query],
        limit: parse_limit_arg(obj)?,
        rerank: true,
        filters: None,
        mode,
        code_mode: parse_code_mode_arg(obj, mode)?,
        rerank_instructions: None,
        version_match: None,
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

    // Parse + validate the optional rerank instruction against the shared 400-
    // char cap (the validator's message names the cap). Reject a present-but-
    // not-string value rather than silently dropping it.
    let rerank_instructions = match obj.get("rerank_instructions") {
        None => None,
        Some(serde_json::Value::String(s)) => {
            mnm_core::rerank::validate_instruction(s)?;
            Some(s.clone())
        }
        Some(_) => return Err("`rerank_instructions` must be a string".to_owned()),
    };

    // Parse + validate the optional version-matching mode. Reject any value
    // outside the two documented modes (and any non-string) with a message that
    // names both, so a typo never silently degrades to the server default.
    let version_match = match obj.get("version_match") {
        None => None,
        Some(serde_json::Value::String(s)) if s == "strict" || s == "permissive" => Some(s.clone()),
        Some(serde_json::Value::String(s)) => {
            return Err(format!("`version_match` must be `strict` or `permissive` (got `{s}`)"));
        }
        Some(_) => {
            return Err("`version_match` must be a string (`strict` or `permissive`)".to_owned())
        }
    };

    let filters = obj.get("filters").cloned();

    // Validate the filters object against the registry before forwarding (fail fast).
    if let Some(fv) = &filters {
        let parsed: mnm_retrieval::filters::SearchFilters =
            serde_json::from_value(fv.clone()).map_err(|e| format!("invalid filters: {e}"))?;
        parsed
            .validate()
            .map_err(|e| format!("invalid filter `{}`: {}", e.facet, e.message))?;
    }

    let mode = parse_mode_arg(obj)?;
    Ok(ParsedSearchArgs {
        queries,
        limit: parse_limit_arg(obj)?,
        rerank,
        filters,
        mode,
        code_mode: parse_code_mode_arg(obj, mode)?,
        rerank_instructions,
        version_match,
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

/// Parse the shared optional `code_mode` argument, failing fast on the fts
/// incompatibility (mirrors the cloud's 400, D5/D6: fts forces code_mode off)
/// before any embedding or wire call.
fn parse_code_mode_arg(
    obj: &serde_json::Map<String, serde_json::Value>,
    mode: &'static str,
) -> Result<Option<&'static str>, String> {
    let code_mode = match obj.get("code_mode") {
        None => None,
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "on" => Some("on"),
            "off" => Some("off"),
            "exclusive" => Some("exclusive"),
            other => {
                return Err(format!("unknown code_mode `{other}` (expected on|off|exclusive)"));
            }
        },
        Some(_) => return Err("`code_mode` must be a string".to_owned()),
    };
    if mode == "fts" && matches!(code_mode, Some("on" | "exclusive")) {
        return Err(
            "code_mode on/exclusive is incompatible with mode=fts (drop code_mode, or use \
             mode=hybrid/vector)"
                .to_owned(),
        );
    }
    Ok(code_mode)
}

/// Rerank `results` client-side against the first query via `VoyageAI`'s
/// `/v1/rerank` (BYOK), then re-order + truncate. Mirrors the CLI's local path:
/// the candidate pool was over-fetched (see [`RERANK_FETCH`]) and the instruction
/// precedence matches the server so placement doesn't change results — an
/// agent-supplied `rerank_instructions` wins; otherwise the same derived default
/// the server uses (from `code_mode == exclusive` and the first `language_target`
/// filter's `(name, version_satisfies)`).
///
/// On a successful rerank this flips the `status` "rerank exercised" marker.
///
/// Returns the reordered results alongside Voyage's reported `total_tokens`
/// (the caller applies the model's billing rate for the `Rerank` event).
///
/// # Errors
///
/// Returns [`SearchError::Cloud`] on a Voyage rerank failure.
async fn rerank_results(
    parsed: &ParsedSearchArgs,
    results: Vec<serde_json::Value>,
    voyage_key: &str,
    model: &'static str,
    voyage_base_url: Option<&str>,
    limit: u32,
) -> Result<(Vec<serde_json::Value>, u64), SearchError> {
    let mut reranker = voyage::VoyageReranker::new(voyage_key, model);
    if let Some(base) = voyage_base_url {
        reranker = reranker.with_base_url(base);
    }

    // Use the first query as the rerank pivot. Multi-query / HyDE typically
    // wants the most "user-facing" question to anchor the rerank.
    let pivot = parsed.queries.first().map_or(String::new(), String::clone);

    // Agent instruction wins; else the same derived default the server uses
    // (code_mode exclusive / version filter), so placement doesn't change
    // results. `default_instruction` is pure + cheap, so deriving it even when
    // an agent instruction is present (then preferring the agent's) is clearer
    // than a borrow-juggling `if let`/`else`.
    let code_exclusive = parsed.code_mode == Some("exclusive");
    let version = first_language_target_version(parsed.filters.as_ref());
    let derived = mnm_core::rerank::default_instruction(
        code_exclusive,
        version.as_ref().map(|(n, v)| (n.as_str(), v.as_str())),
    );
    let instr = parsed.rerank_instructions.as_deref().or(derived.as_deref());
    let composed = mnm_core::rerank::compose_rerank_query(&pivot, instr);

    let docs: Vec<String> = results
        .iter()
        .map(|r| {
            r.get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        })
        .collect();
    let out = reranker
        .rerank(composed, docs, None)
        .await
        .map_err(|e| SearchError::Cloud(format!("rerank failed: {e}")))?;

    LOADED_MARKERS.mark_reranker();
    Ok((rerank_postprocess(results, &out.results, limit), out.total_tokens))
}

/// Extract the first `language_target` filter's `(name, version_satisfies)` from
/// the raw MCP `filters` JSON, when both are present — the version signal the
/// derived rerank default uses. Mirrors the CLI's
/// `filters.language_target.any_of.first()` expression over typed filters.
fn first_language_target_version(filters: Option<&serde_json::Value>) -> Option<(String, String)> {
    let lt = filters?
        .get("language_target")?
        .get("any_of")?
        .as_array()?
        .first()?;
    let name = lt.get("name")?.as_str()?.to_owned();
    let version = lt.get("version_satisfies")?.as_str()?.to_owned();
    Some((name, version))
}

/// Attach `rerank_score`, recompute trust-aware `confidence` from the
/// Voyage relevance score (already 0–1), re-sort, and truncate (US6 #8/#12).
///
/// For each reranked result we substitute the Voyage relevance score
/// (clamped to `[0, 1]`) for the relevance term, blend it with the cloud's
/// `trust_score` using the compiled-in default policy weights, and record the
/// substitution in `confidence_factors.relevance_source = "rerank"`. Results
/// are then ordered by the recomputed confidence (descending). Results that
/// carry no cloud `trust_score` (e.g. an older cloud, or `include_scores=false`)
/// keep their confidence and fall back to ordering by the reranker score, so
/// the function degrades gracefully. Pure (no model/IO) so it is unit-testable.
fn rerank_postprocess(
    mut results: Vec<serde_json::Value>,
    scores: &[reranker::RerankResult],
    limit: u32,
) -> Vec<serde_json::Value> {
    let policy = ScoringPolicy::default();
    // Dedupe by index in case the model ever returns the same source index
    // twice (defensive — Voyage shouldn't, but a future swap could).
    let mut seen = std::collections::HashSet::new();
    let mut indexed: Vec<(f64, serde_json::Value)> = scores
        .iter()
        .filter_map(|s| {
            let idx = s.index;
            if idx >= results.len() || !seen.insert(idx) {
                return None;
            }
            let relevance = f64::from(s.score).clamp(0.0, 1.0);
            let mut taken = std::mem::take(&mut results[idx]);
            let sort_key = recompute_confidence(&mut taken, &policy, f64::from(s.score), relevance);
            // `raw_score` and `relevance` coincide now that Voyage returns a 0–1
            // score directly (no sigmoid); the only difference is the clamp.
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

/// Patch one result in place: attach the raw `rerank_score` (the Voyage
/// relevance score, already 0–1), and when the cloud supplied a
/// `scores.trust_score`, recompute `scores.confidence` from `relevance` and
/// stamp `relevance_source`/`relevance_multiplier` into `confidence_factors`.
/// Returns the value to sort by (the recomputed confidence, else the relevance
/// when no trust is available).
fn recompute_confidence(
    result: &mut serde_json::Value,
    policy: &ScoringPolicy,
    raw_score: f64,
    relevance: f64,
) -> f64 {
    let Some(obj) = result.as_object_mut() else {
        return relevance;
    };
    obj.insert("rerank_score".to_owned(), serde_json::Value::from(raw_score));

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

/// Dispatch `facets`. Forwards the drill-down arguments (`facet`, `within`,
/// `cursor`, `limit`) to `GET /v1/facets` as query params — only keys present in
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
    for key in ["facet", "within", "cursor", "limit"] {
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
    fn every_advertised_output_schema_has_object_root_type() {
        // structuredContent is always a JSON object, and strict MCP clients
        // require each advertised outputSchema to carry a root `type: "object"`.
        // A root-level combinator (e.g. `oneOf`) without `type` makes those
        // clients reject the whole `tools/list` response — guard against it.
        for t in list().tools {
            let schema = t
                .output_schema
                .expect("every tool advertises an outputSchema");
            assert_eq!(
                schema.get("type").and_then(serde_json::Value::as_str),
                Some("object"),
                "tool `{}` outputSchema must have a root `type: object`",
                t.name
            );
        }
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
    fn both_search_input_schemas_have_code_mode_enum() {
        for s in [search_input_schema(), advanced_search_input_schema()] {
            let cm = &s["properties"]["code_mode"];
            assert_eq!(cm["type"], "string");
            assert_eq!(cm["enum"], json!(["on", "off", "exclusive"]));
            assert!(
                cm["description"].as_str().unwrap_or("").contains("fts"),
                "code_mode description must document the fts incompatibility"
            );
        }
    }

    #[test]
    fn search_description_mentions_code_mode_guidance() {
        let m = list();
        let t = m
            .tools
            .iter()
            .find(|t| t.name == "search")
            .expect("search tool");
        assert!(
            t.description.contains("code_mode=exclusive"),
            "search description should steer code-shaped queries at code_mode=exclusive"
        );
    }

    #[test]
    fn both_parsers_accept_code_mode_values() {
        for v in ["on", "off", "exclusive"] {
            let b = parse_basic_search_args(&json!({ "query": "x", "code_mode": v })).unwrap();
            assert_eq!(b.code_mode, Some(v));
            let a =
                parse_advanced_search_args(&json!({ "queries": ["x"], "code_mode": v })).unwrap();
            assert_eq!(a.code_mode, Some(v));
        }
        // Absent means "let the server apply its mode-derived default".
        let b = parse_basic_search_args(&json!({ "query": "x" })).unwrap();
        assert_eq!(b.code_mode, None);
        let a = parse_advanced_search_args(&json!({ "queries": ["x"] })).unwrap();
        assert_eq!(a.code_mode, None);
    }

    #[test]
    fn both_parsers_reject_bad_code_mode() {
        assert!(parse_basic_search_args(&json!({ "query": "x", "code_mode": "auto" })).is_err());
        assert!(parse_basic_search_args(&json!({ "query": "x", "code_mode": 1 })).is_err());
        assert!(
            parse_advanced_search_args(&json!({ "queries": ["x"], "code_mode": "auto" })).is_err()
        );
        assert!(parse_advanced_search_args(&json!({ "queries": ["x"], "code_mode": 1 })).is_err());
    }

    #[test]
    fn both_parsers_reject_fts_with_code_mode_on_or_exclusive() {
        // Mirrors the server's 400: fts forces code_mode off (D5).
        for v in ["on", "exclusive"] {
            assert!(
                parse_basic_search_args(&json!({ "query": "x", "mode": "fts", "code_mode": v }))
                    .is_err(),
                "basic: fts + code_mode={v} must be rejected client-side"
            );
            assert!(
                parse_advanced_search_args(
                    &json!({ "queries": ["x"], "mode": "fts", "code_mode": v })
                )
                .is_err(),
                "advanced: fts + code_mode={v} must be rejected client-side"
            );
        }
        assert!(parse_basic_search_args(
            &json!({ "query": "x", "mode": "fts", "code_mode": "off" })
        )
        .is_ok());
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

    #[test]
    fn version_match_parsed_and_forwarded() {
        let args = serde_json::json!({
            "queries": ["q"],
            "version_match": "strict",
            "filters": { "language_target": { "any_of": [{ "name": "compact", "version_satisfies": ">=0.23" }] } }
        });
        let parsed = parse_advanced_search_args(&args).unwrap();
        assert_eq!(parsed.version_match.as_deref(), Some("strict"));
        // range syntax now validates (was a 400 under concrete-only semantics)

        // Omitted defers to the server default (permissive).
        let none = parse_advanced_search_args(&serde_json::json!({ "queries": ["q"] })).unwrap();
        assert_eq!(none.version_match, None);

        // Only the two documented values are accepted; the error names them.
        let err = parse_advanced_search_args(
            &serde_json::json!({ "queries": ["q"], "version_match": "loose" }),
        )
        .unwrap_err();
        assert!(err.contains("strict"), "error names `strict`: {err}");
        assert!(err.contains("permissive"), "error names `permissive`: {err}");
        // Non-string values are rejected too.
        assert!(parse_advanced_search_args(
            &serde_json::json!({ "queries": ["q"], "version_match": 1 })
        )
        .is_err());
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
        // #8/#12: confidence is recomputed from the Voyage relevance score
        // (already 0–1, no sigmoid) and the substitution is recorded.
        let results = vec![result_with_trust("a", 0.9)];
        let scores = vec![reranker::RerankResult { index: 0, score: 0.88 }];
        let out = rerank_postprocess(results, &scores, 10);
        assert_eq!(out.len(), 1);
        let s = &out[0]["scores"];
        assert_eq!(s["confidence_factors"]["relevance_source"], "rerank");
        // relevance_multiplier == the raw Voyage score (0.88), NOT sigmoid(0.88).
        let rel = s["confidence_factors"]["relevance_multiplier"]
            .as_f64()
            .unwrap();
        assert!((rel - 0.88).abs() < 1e-4, "relevance was {rel}");
        // confidence recomputed away from the cloud's 0.4 placeholder.
        let conf = s["confidence"].as_f64().unwrap();
        assert!((conf - 0.4).abs() > 1e-6 && (0.0..=1.0).contains(&conf));
        assert!((out[0]["rerank_score"].as_f64().unwrap() - 0.88).abs() < 1e-4);
    }

    #[test]
    fn rerank_orders_by_recomputed_confidence_not_relevance() {
        // High-trust chunk with a slightly lower Voyage relevance should still
        // outrank a low-trust chunk with a slightly higher relevance, because
        // confidence blends trust in.
        let results = vec![
            result_with_trust("low_trust", 0.05),
            result_with_trust("high_trust", 0.99),
        ];
        let scores = vec![
            reranker::RerankResult { index: 0, score: 0.92 }, // low trust, higher relevance
            reranker::RerankResult { index: 1, score: 0.90 }, // high trust, lower relevance
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
    fn rerank_postprocess_uses_voyage_scores_directly() {
        use reranker::RerankResult;
        // Voyage relevance_score is already 0–1: NO sigmoid. trust=1.0 and the
        // default policy make confidence == relevance^relevance_weight; the key
        // assertion is relevance_multiplier == the raw score, not sigmoid(score).
        let results = vec![
            serde_json::json!({"content": "a", "scores": {"trust_score": 1.0,
                "confidence": 0.5, "confidence_factors": {"relevance_source": "rrf",
                "relevance_multiplier": 0.5}}}),
            serde_json::json!({"content": "b", "scores": {"trust_score": 1.0,
                "confidence": 0.5, "confidence_factors": {"relevance_source": "rrf",
                "relevance_multiplier": 0.5}}}),
        ];
        let scores = vec![
            RerankResult { index: 1, score: 0.9 },
            RerankResult { index: 0, score: 0.2 },
        ];
        let out = rerank_postprocess(results, &scores, 10);
        assert_eq!(out[0]["content"], "b"); // 0.9 outranks 0.2
        let f = &out[0]["scores"]["confidence_factors"];
        assert_eq!(f["relevance_source"], "rerank");
        let rm = f["relevance_multiplier"].as_f64().unwrap();
        // 1e-6 (not 1e-9): the score is an f32, so 0.9_f32 -> f64 is
        // 0.899999976…; the sigmoid bug would land ~0.711, far outside 1e-6.
        assert!((rm - 0.9).abs() < 1e-6, "expected raw 0.9, got {rm} (sigmoid bug?)");
        assert!((out[0]["rerank_score"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    }

    #[test]
    fn advanced_search_parses_rerank_instructions_and_caps_length() {
        // Use the real parser; arg shape mirrors the manifest schema.
        let ok = serde_json::json!({"queries": ["q"], "rerank_instructions": "Prefer code."});
        let parsed = parse_advanced_search_args(&ok).unwrap();
        assert_eq!(parsed.rerank_instructions.as_deref(), Some("Prefer code."));

        let too_long = serde_json::json!({"queries": ["q"],
            "rerank_instructions": "x".repeat(401)});
        let err = parse_advanced_search_args(&too_long).unwrap_err();
        // Whatever the parser's error type, the message must name the 400 cap.
        assert!(format!("{err:?}").contains("400"));
    }

    #[test]
    fn every_tool_advertises_output_schema() {
        for t in list().tools {
            assert!(t.output_schema.is_some(), "tool {} missing outputSchema", t.name);
        }
    }

    // --- placement → wire mapping (build_search_request) -------------------
    // These pure helpers carry the highest-value new branches but are never hit
    // by the search_voyage integration tests (which all drive rerank:false, so
    // resolve_rerank_for_search short-circuits to Off). Cover them directly.

    use std::collections::HashMap;

    /// Minimal `ParsedSearchArgs` for the wire-mapping tests.
    fn parsed_args(rerank: bool) -> ParsedSearchArgs {
        ParsedSearchArgs {
            queries: vec!["q".to_owned()],
            limit: 7,
            rerank,
            filters: None,
            mode: "hybrid",
            code_mode: None,
            rerank_instructions: None,
            version_match: None,
        }
    }

    #[test]
    fn build_search_request_local_overfetches_and_sends_none() {
        // Local: widen the cloud pool to RERANK_FETCH in score order so the
        // client-side reranker can promote a below-limit chunk; tell the cloud
        // `none` (Local reranks client-side — exactly one pass).
        use mnm_core::config::RerankPlacement;
        use mnm_core::rerank::RerankParam;
        let req = build_search_request(
            &parsed_args(true),
            RerankPlacement::Local,
            RerankParam::Rerank25,
            Vec::new(),
            "voyage-context-3@1".to_owned(),
            None,
        );
        assert_eq!(req.limit, RERANK_FETCH);
        assert_eq!(req.sort_by, Some("score"));
        assert_eq!(req.rerank.as_deref(), Some("none"));
        assert!(req.rerank_instructions.is_none());
    }

    #[test]
    fn build_search_request_server_forwards_model_and_instructions() {
        // Server: keep the caller's limit + cloud confidence ordering, and
        // forward the resolved model name plus any agent rerank_instructions.
        use mnm_core::config::RerankPlacement;
        use mnm_core::rerank::RerankParam;
        let mut parsed = parsed_args(true);
        parsed.rerank_instructions = Some("Prefer code.".to_owned());
        let req = build_search_request(
            &parsed,
            RerankPlacement::Server,
            RerankParam::Rerank25Lite,
            Vec::new(),
            "voyage-context-3@1".to_owned(),
            None,
        );
        assert_eq!(req.limit, 7);
        assert_eq!(req.sort_by, None);
        assert_eq!(req.rerank.as_deref(), Some("rerank-2.5-lite"));
        assert_eq!(req.rerank_instructions.as_deref(), Some("Prefer code."));
    }

    #[test]
    fn build_search_request_off_sends_none_and_no_instructions() {
        // Off: caller's limit + cloud ordering, `none` on the wire, and the
        // agent instruction is dropped (no rerank runs, so it would be a no-op).
        use mnm_core::config::RerankPlacement;
        use mnm_core::rerank::RerankParam;
        let mut parsed = parsed_args(false);
        parsed.rerank_instructions = Some("ignored".to_owned());
        let req = build_search_request(
            &parsed,
            RerankPlacement::Off,
            RerankParam::Rerank25,
            Vec::new(),
            "voyage-context-3@1".to_owned(),
            None,
        );
        assert_eq!(req.limit, 7);
        assert_eq!(req.sort_by, None);
        assert_eq!(req.rerank.as_deref(), Some("none"));
        assert!(req.rerank_instructions.is_none());
    }

    // --- resolve_rerank_for_search (guard + override) ----------------------

    #[derive(Default)]
    struct FakeEnv(HashMap<String, String>);

    impl FakeEnv {
        fn set(mut self, k: &str, v: &str) -> Self {
            self.0.insert(k.into(), v.into());
            self
        }
    }

    impl mnm_core::config::ConfigEnv for FakeEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn resolve_rerank_local_without_key_is_an_error() {
        // location=local but no Voyage key: cannot rerank — surface as a Cloud
        // error before any embedding / network work.
        use mnm_core::config::RerankConfig;
        let cfg = RerankConfig {
            location: Some("local".to_owned()),
            model: None,
        };
        let env = FakeEnv::default();
        let err = resolve_rerank_for_search(
            &parsed_args(true),
            &cfg,
            &mnm_core::config::ModelsConfig::default(),
            None,
            &env,
        )
        .expect_err("local without a key must error");
        let SearchError::Cloud(msg) = err else {
            panic!("expected SearchError::Cloud, got {err:?}");
        };
        assert!(msg.contains("local") && msg.contains("Voyage"), "message was {msg}");
    }

    #[test]
    fn resolve_rerank_local_with_key_resolves_local() {
        // The same config with a key present resolves cleanly to Local.
        use mnm_core::config::{RerankConfig, RerankPlacement};
        let cfg = RerankConfig {
            location: Some("local".to_owned()),
            model: None,
        };
        let env = FakeEnv::default();
        let (placement, model, _) = resolve_rerank_for_search(
            &parsed_args(true),
            &cfg,
            &mnm_core::config::ModelsConfig::default(),
            Some("vk"),
            &env,
        )
        .unwrap();
        assert_eq!(placement, RerankPlacement::Local);
        assert_eq!(model.model_name(), Some("rerank-2.5"));
    }

    #[test]
    fn resolve_rerank_false_forces_off_over_any_placement() {
        // rerank:false wins over a `local` placement (and a present key): the
        // tool toggle short-circuits to Off, so the local-without-key guard is
        // never even reached.
        use mnm_core::config::{RerankConfig, RerankPlacement};
        let cfg = RerankConfig {
            location: Some("local".to_owned()),
            model: None,
        };
        let env = FakeEnv::default();
        // No key AND rerank:false: must NOT error (Off, not the local guard).
        let (placement, _, _) = resolve_rerank_for_search(
            &parsed_args(false),
            &cfg,
            &mnm_core::config::ModelsConfig::default(),
            None,
            &env,
        )
        .unwrap();
        assert_eq!(placement, RerankPlacement::Off);
    }

    #[test]
    fn resolve_rerank_reads_voyage_base_url_override() {
        // The base-url override threads through the ConfigEnv seam (no std::env).
        use mnm_core::config::RerankConfig;
        let cfg = RerankConfig::default();
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_VOYAGE_BASE_URL", "https://proxy.test");
        let (_, _, base) = resolve_rerank_for_search(
            &parsed_args(true),
            &cfg,
            &mnm_core::config::ModelsConfig::default(),
            Some("vk"),
            &env,
        )
        .unwrap();
        assert_eq!(base.as_deref(), Some("https://proxy.test"));
    }

    #[test]
    fn no_local_rerank_facts_server_parses_metadata_shapes() {
        use mnm_core::config::RerankPlacement;
        use mnm_core::rerank::RerankParam;
        let facts = |env: serde_json::Value| {
            no_local_rerank_facts(RerankPlacement::Server, RerankParam::Rerank25, &env)
        };

        // Well-formed applied=true (no reason).
        let f = facts(serde_json::json!({ "search_metadata": { "rerank": { "applied": true } } }));
        assert_eq!(f.placement, "server");
        assert_eq!(f.model.as_deref(), Some("rerank-2.5"));
        assert!(f.applied);
        assert_eq!(f.reason, None);
        assert_eq!(f.billed_tokens, None);

        // Well-formed degrade: applied=false + documented reason.
        let f = facts(serde_json::json!({
            "search_metadata": { "rerank": { "applied": false, "reason": "provider_error" } }
        }));
        assert!(!f.applied);
        assert_eq!(f.reason.as_deref(), Some("provider_error"));

        // Missing `rerank` key: not-applied / no-reason.
        let f = facts(serde_json::json!({ "search_metadata": { "other": 1 } }));
        assert!(!f.applied);
        assert_eq!(f.reason, None);

        // Missing `applied`: not-applied (reason still surfaced if known).
        let f = facts(serde_json::json!({
            "search_metadata": { "rerank": { "reason": "disabled" } }
        }));
        assert!(!f.applied);
        assert_eq!(f.reason.as_deref(), Some("disabled"));

        // Non-bool `applied`: not coerced.
        let f = facts(serde_json::json!({
            "search_metadata": { "rerank": { "applied": 1 } }
        }));
        assert!(!f.applied);

        // Arbitrary reason text dropped by the privacy allow-list.
        let f = facts(serde_json::json!({
            "search_metadata": { "rerank": { "applied": false, "reason": "secret=eyJabc" } }
        }));
        assert_eq!(f.reason, None, "free-form server reason must not reach the event");

        // No search_metadata at all on the server path: not-applied.
        let f = facts(serde_json::json!({}));
        assert!(!f.applied);
        assert_eq!(f.reason, None);
    }

    #[test]
    fn no_local_rerank_facts_off_ignores_metadata() {
        use mnm_core::config::RerankPlacement;
        use mnm_core::rerank::RerankParam;
        // Off opted out client-side: model is None, applied=false, reason=None,
        // even when the server echo claims otherwise.
        let env = serde_json::json!({
            "search_metadata": { "rerank": { "applied": true, "reason": "not_requested" } }
        });
        let f = no_local_rerank_facts(RerankPlacement::Off, RerankParam::None, &env);
        assert_eq!(f.placement, "off");
        assert_eq!(f.model, None);
        assert!(!f.applied);
        assert_eq!(f.reason, None);
        assert_eq!(f.billed_tokens, None);
    }

    #[test]
    fn rerank_event_model_matrix() {
        use mnm_core::config::RerankPlacement;
        use mnm_core::rerank::RerankParam;
        // Local / Server name the resolved model; Off names none.
        assert_eq!(
            rerank_event_model(RerankPlacement::Local, RerankParam::Rerank25).as_deref(),
            Some("rerank-2.5")
        );
        assert_eq!(
            rerank_event_model(RerankPlacement::Server, RerankParam::Rerank25Lite).as_deref(),
            Some("rerank-2.5-lite")
        );
        assert_eq!(rerank_event_model(RerankPlacement::Off, RerankParam::None), None);
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
        impl mnm_skills::SkillEnv for EmptyEnv {
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
        // mnm_skills::install returns NoHarnessDetected -> mapped to ToolFailed.
        let err = run_install_search_skill_in(&json!({}), &env).unwrap_err();
        assert!(matches!(err.0, crate::protocol::ErrorCode::ToolFailed));
    }

    #[test]
    fn install_writes_into_injected_fake_home() {
        // No global env mutation: inject a fake SkillEnv pointing at a tempdir.
        struct FakeEnv {
            home: std::path::PathBuf,
        }
        impl mnm_skills::SkillEnv for FakeEnv {
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

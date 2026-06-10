//! MCP tool registry and per-tool handlers.
//!
//! Thirteen tools, four categories:
//!
//! - `status` / `pull_models` — local-only; talk to the reranker model cache.
//!   The corpus embedder is VoyageAI (remote), so there is no local embedder to
//!   load here. No cloud round-trip.
//! - `search` — embed via VoyageAI (BYOK or server-proxy), post to the cloud
//!   `/v1/search`, optionally rerank with the local cross-encoder.
//! - All other tools (`get_chunk` / `get_chunk_next` / `get_chunk_prev` /
//!   `get_chunk_neighbors` / `get_chunk_parents` / `get_document` /
//!   `get_document_chunks` / `list_sources`) —
//!   pass-through to the cloud's read endpoints, returning the response JSON
//!   verbatim. `get_chunk_neighbors` is the only one that fans out to three
//!   cloud endpoints concurrently and bundles the results.
//! - Local install: `install_search_skill` (writes the advanced-search
//!   `SKILL.md` into the user's AI harness(es)).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use mn_core::scoring::normalize_rerank;
use mn_core::scoring_policy::ScoringPolicy;
use mn_embedding::{client as embed_client, reranker, reranker_catalog, voyage, LoadedReranker};
use serde::Serialize;
use serde_json::json;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::cloud_client::{CloudClient, CloudError, QueryPair, SearchRequest};
use crate::protocol::{ToolDescription, ToolsListResult};
use crate::server::ServerConfig;

/// Build the static tool manifest sent in response to `tools/list`.
///
/// All thirteen tools declared in spec.md US5 / contracts/mcp-tools.json.
/// Schemas here are kept in sync with the canonical document by way of the
/// contract tests in `tests/`.
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
                    "Hybrid (FTS + vector) retrieval over the Midnight corpus, with optional cross-encoder reranking and trust-aware confidence scoring. Provide the single-query `{query, vector}` form, or a `queries` array of 1-10 `{text, vector}` pairs that RRF fuses across. The caller embeds each text (except in `fts` mode). Rate-limit cost is max(1, distinct queries) tokens (D25).\n\nPatterns (full worked examples in docs/cookbook/query-enhancement.md):\n- hyde: send the question plus a hypothetical answer as a second query, e.g. queries=[\"<the user's question>\", \"<a 1-2 sentence hypothetical answer the agent drafts>\"]. Lifts recall when the question is short or jargon-light.\n- multi_query: send 2-3 paraphrases varying vocabulary and breadth, e.g. queries=[\"compile a contract\", \"build source into a deployable artifact\", \"smart-contract build step\"]. Helps when synonyms matter.\n- step_back: send the question plus a more abstract framing, e.g. queries=[\"why did this specific call fail?\", \"how does the platform validate calls?\"]. Helps when the question is over-specific.",
                input_schema: search_input_schema(),
                output_schema: Some(crate::schemas::search_output_schema()),
            },
            ToolDescription {
                name: "get_chunk",
                description:
                    "Fetch one chunk by id. Returns the chunk row (id, content, chunk_index, total_chunks, content_hash, embedding_model_id, heading_path, symbol_path, start_byte, end_byte, token_count, status, created_at, document_id, source_version_id, node_id) plus a small `document` sub-object (id, source_path, published_url, source_url, language, kind, provenance) and a `source` sub-object (slug + display_name). For the chunk's parent chain call get_chunk_parents; for adjacent chunks call get_chunk_next/get_chunk_prev.",
                input_schema: id_only_schema(),
                output_schema: Some(crate::schemas::chunk_output_schema()),
            },
            ToolDescription {
                name: "get_chunk_next",
                description:
                    "Fetch up to `count` chunks immediately following the given chunk in chunk_index order, scoped to the same document. Returns `{chunks: ChunkWithContext[]}` sorted ascending. Returns `{chunks: []}` (not 404) when called on the last chunk. `embed_failed` chunks are skipped, so the returned chunk_index sequence may have gaps. count defaults to 5 and must be in [1, 100]; out-of-range values are rejected as InvalidParams before the call reaches the cloud.",
                input_schema: chunk_nav_schema(),
                output_schema: Some(crate::schemas::chunk_list_output_schema()),
            },
            ToolDescription {
                name: "get_chunk_prev",
                description:
                    "Fetch up to `count` chunks immediately preceding the given chunk in chunk_index order, scoped to the same document. Returns `{chunks: ChunkWithContext[]}` sorted ascending (reading order). Returns `{chunks: []}` (not 404) when called on the first chunk. `embed_failed` chunks are skipped, so the returned chunk_index sequence may have gaps. count defaults to 5 and must be in [1, 100]; out-of-range values are rejected as InvalidParams before the call reaches the cloud.",
                input_schema: chunk_nav_schema(),
                output_schema: Some(crate::schemas::chunk_list_output_schema()),
            },
            ToolDescription {
                name: "get_chunk_neighbors",
                description:
                    "Bundle `get_chunk_prev` + `get_chunk` + `get_chunk_next` in one round-trip. Returns `{prev: {chunks: ChunkWithContext[]}, chunk: ChunkWithContext, next: {chunks: ChunkWithContext[]}}` where `prev`/`next` are the same envelopes the standalone tools return (so an empty corpus edge yields `chunks: []`, not 404). The three cloud calls are issued in parallel, so latency is roughly that of the slowest leg. `count` defaults to 2 (chunks on each side) and must be in [1, 100]; out-of-range values are rejected as InvalidParams before any wire call. A 404 on the anchor chunk surfaces the same not-found envelope a plain `get_chunk` would.",
                input_schema: chunk_neighbors_schema(),
                output_schema: Some(crate::schemas::neighbors_output_schema()),
            },
            ToolDescription {
                name: "get_chunk_parents",
                description:
                    "Walk the parent chain from a chunk up to its source-version root. Returns nodes from immediate parent to root.",
                input_schema: id_only_schema(),
                output_schema: Some(crate::schemas::parents_output_schema()),
            },
            ToolDescription {
                name: "get_document",
                description:
                    "Document overview: metadata (id, source_version_id, node_id, source_path, published_url, source_url, language, kind, content_hash, char_count, token_count, source_modified_at, created_at, frontmatter, provenance, package_id), the source `{slug}`, and an ordered `chunks` skeleton array of every ready chunk (`{id, chunk_index, token_count}`). No chunk bodies. Use get_document_chunks for bodies.",
                input_schema: id_only_schema(),
                output_schema: Some(crate::schemas::document_output_schema()),
            },
            ToolDescription {
                name: "get_document_chunks",
                description:
                    "Position-windowed chunk slice of a document. Returns `{chunks: ChunkBody[], from, limit, total_chunks}`. from defaults to 0 (must be >= 0); limit defaults to 20 and must be in [1, 100]. Out-of-range values are rejected as InvalidParams before the call reaches the cloud. `from` past the end returns `chunks: []` with accurate `total_chunks` (not 404). Use to page through a document's chunk bodies or to read a known offset.",
                input_schema: document_chunks_schema(),
                output_schema: Some(crate::schemas::document_output_schema()),
            },
            ToolDescription {
                name: "list_sources",
                description:
                    "Enumerate the corpus's available sources so an agent can narrow filters.",
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                output_schema: Some(crate::schemas::sources_output_schema()),
            },
            ToolDescription {
                name: "facets",
                description:
                    "List the filterable facets for `search`, their types, whether they support exclusion (none_of), and the values present in the active corpus (languages, tags, sources, packages). Call this before constructing a `filters` object to learn valid values. Closed-enum facets (kind, content_type, attribution, source_kind) carry their full value list; high-cardinality sets (tags, package) are top-N with `truncated`/`total`.",
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                output_schema: Some(crate::schemas::facets_output_schema()),
            },
            ToolDescription {
                name: "pull_models",
                description:
                    "Download / load the reranker (bge-reranker-base) into the local model cache. Required on first use of `search` with reranking enabled. The corpus embedder is VoyageAI (remote — BYOK or the server's /v1/embeddings proxy), so no embedder is downloaded.",
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                output_schema: Some(crate::schemas::pull_models_output_schema()),
            },
            ToolDescription {
                name: "status",
                description:
                    "Report health and model state. Works without models loaded.",
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                output_schema: Some(crate::schemas::status_output_schema()),
            },
            ToolDescription {
                name: "install_search_skill",
                description:
                    "Install the midnight-advanced-search Agent Skill (a persistent retrieval playbook) into the user's AI harness(es). Writes the same SKILL.md to each detected harness's native skills directory; re-running updates in place. Returns, per harness, the scope, the exact path written, the action (created/updated/unchanged), and the reload step to relay to the user. Optional `harness` (subset of claude-code/codex/opencode/cursor) forces specific targets; omit to auto-detect. Optional `scope` is user (default) or project.",
                input_schema: install_search_skill_schema(),
                output_schema: Some(crate::schemas::install_output_schema()),
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
            "id": { "type": "string", "format": "uuid" },
            "count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 5 },
        },
        "additionalProperties": false,
    })
}

fn chunk_neighbors_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 2 },
        },
        "additionalProperties": false,
    })
}

fn document_chunks_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "from": { "type": "integer", "minimum": 0, "default": 0 },
            "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
        },
        "additionalProperties": false,
    })
}

#[allow(clippy::too_many_lines)]
fn search_input_schema() -> serde_json::Value {
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
        "properties": {
            "query": { "type": "string", "minLength": 1, "description": "Single-query convenience form (mutually exclusive with queries)." },
            "queries": { "type": "array", "minItems": 1, "maxItems": 50, "items": { "type": "string", "minLength": 1 }, "description": "Multi-query input for HyDE / expansion / step-back patterns." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10, "description": "Max results returned to the caller. Capped at 50." },
            "rerank": { "type": "boolean", "default": true, "description": "Apply local cross-encoder reranking. Disable for ultra-low-latency callers." },
            "mode": { "type": "string", "enum": ["hybrid", "vector", "fts"], "default": "hybrid",
                      "description": "fts skips embedding entirely (lowest latency); vector is semantic-only; hybrid (default) fuses both." },
            "filters": {
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
            }
        },
        "oneOf": [ { "required": ["query"] }, { "required": ["queries"] } ]
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
// status (local)
// ---------------------------------------------------------------------------

/// `status` tool response payload.
#[derive(Debug, Serialize)]
pub struct StatusOutput {
    /// mn-mcp crate version.
    pub server_version: &'static str,
    /// Reranker model identifier. The corpus embedder is VoyageAI (remote), so
    /// no local embedder is reported.
    pub reranker: &'static str,
    /// Current model state.
    pub model_state: ModelState,
    /// Resolved on-disk model cache directory, if any.
    pub cache_dir: Option<String>,
}

/// Coarse model-state values reported by `status`.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelState {
    /// Reranker not yet loaded for this process.
    Missing,
    /// Reranker loaded and ready to use.
    Ready,
}

/// Dispatch the `status` tool.
#[must_use]
pub fn run_status(cache_dir: Option<&PathBuf>) -> StatusOutput {
    StatusOutput {
        server_version: crate::VERSION,
        reranker: mn_embedding::RERANKER_MODEL_NAME,
        // Only the reranker is a local model now; the embedder is remote Voyage.
        model_state: if reranker_loaded() {
            ModelState::Ready
        } else {
            ModelState::Missing
        },
        cache_dir: cache_dir.map(|p| p.display().to_string()),
    }
}

fn reranker_loaded() -> bool {
    LOADED_MARKERS.load_relaxed_reranker()
}

mod markers {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Process-wide marker tracking whether *a* reranker has been loaded — the
    /// local `bge` cross-encoder (`pull_models`) or whichever reranker the
    /// configured catalog id selects on the first reranking `search` (local
    /// fastembed or remote Voyage). It is a coarse "rerank capability is warm"
    /// signal for `status`, not a model identity.
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
/// successful first load this also flips the `status`/`pull_models`
/// "reranker loaded" marker.
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
// pull_models (local)
// ---------------------------------------------------------------------------

/// `pull_models` response payload.
#[derive(Debug, Serialize)]
pub struct PullModelsOutput {
    /// Reranker model identifier. The corpus embedder is VoyageAI (remote), so
    /// `pull_models` only fetches the reranker.
    pub reranker: &'static str,
    /// Whether the reranker was loaded by this call (false = cached).
    pub reranker_loaded: bool,
    /// Total milliseconds spent in this call.
    pub took_ms: u128,
}

/// Dispatch the `pull_models` tool. Fetches the reranker into the local cache;
/// the corpus embedder is VoyageAI (remote) so nothing is downloaded for it.
/// Returns once the reranker `OnceCell` is filled.
///
/// # Errors
///
/// Returns a string error message if the reranker fails to initialize.
pub async fn run_pull_models(cache_dir: PathBuf) -> Result<PullModelsOutput, String> {
    let t0 = Instant::now();
    let reranker_was_loaded = LOADED_MARKERS.load_relaxed_reranker();

    // NOTE (Task 9.4): `pull_models` still pre-fetches the local `bge` reranker
    // via the singleton. Pre-pulling the *configured* catalog reranker (e.g. a
    // Voyage id, which has nothing to download) is intentionally out of scope
    // here; `search` loads whatever the config selects lazily on first use.
    reranker::global(cache_dir)
        .await
        .map_err(|e| format!("reranker init failed: {e}"))?;
    LOADED_MARKERS.mark_reranker();

    Ok(PullModelsOutput {
        reranker: mn_embedding::RERANKER_MODEL_NAME,
        reranker_loaded: !reranker_was_loaded,
        took_ms: t0.elapsed().as_millis(),
    })
}

// ---------------------------------------------------------------------------
// search (cloud + local embed + optional local rerank)
// ---------------------------------------------------------------------------

/// Errors `run_search` can produce. Distinguished so the server layer can map
/// them to the right MCP error code.
#[derive(Debug)]
pub enum SearchError {
    /// Caller-supplied arguments are malformed (oneOf violation, type
    /// mismatch, etc.).
    InvalidInput(String),
    /// Cloud returned an embedding-model mismatch — the JSON-RPC error layer
    /// turns this into a typed response with `data.next_tool = "pull_models"`.
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
/// When rerank is on, we always fetch up to this many candidates from the
/// cloud so the cross-encoder has signal to work with.
const RERANK_FETCH: u32 = 50;

/// Dispatch the `search` tool.
///
/// Embeds each query locally (except in `fts` mode, which skips embedding
/// entirely), posts the resulting `{text, vector}` pairs to the cloud's
/// `/v1/search`, optionally reranks the returned chunks against the first
/// query, and truncates to the caller's `limit`. When rerank is on each
/// returned result gains a `rerank_score` field.
///
/// # Errors
///
/// See [`SearchError`].
pub async fn run_search(
    args: &serde_json::Value,
    cfg: &ServerConfig,
    cloud: &Arc<CloudClient>,
) -> Result<serde_json::Value, SearchError> {
    let parsed = parse_search_args(args).map_err(SearchError::InvalidInput)?;

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

struct ParsedSearchArgs {
    queries: Vec<String>,
    limit: u32,
    rerank: bool,
    filters: Option<serde_json::Value>,
    mode: &'static str,
}

fn parse_search_args(v: &serde_json::Value) -> Result<ParsedSearchArgs, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "arguments must be a JSON object".to_owned())?;

    let queries: Vec<String> = match (obj.get("query"), obj.get("queries")) {
        (Some(_), Some(_)) => {
            return Err("`query` and `queries` are mutually exclusive".to_owned());
        }
        (Some(serde_json::Value::String(s)), None) => {
            if s.is_empty() {
                return Err("`query` must not be empty".to_owned());
            }
            vec![s.clone()]
        }
        (None, Some(serde_json::Value::Array(arr))) => {
            if arr.is_empty() {
                return Err("`queries` must not be empty".to_owned());
            }
            if arr.len() > MAX_LIMIT as usize {
                return Err(format!("`queries` length must be <= {MAX_LIMIT}"));
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
        _ => return Err("supply either `query` (string) or `queries` (array)".to_owned()),
    };

    // Honour omitted `limit` as the default; reject any present-but-not-integer
    // value rather than quietly defaulting (silent-default would let callers
    // ship a typo like `limit: "five"` and never notice).
    let limit = match obj.get("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err("`limit` must be an integer".to_owned());
            };
            if !(1..=i64::from(MAX_LIMIT)).contains(&n) {
                return Err(format!("`limit` must be 1..={MAX_LIMIT}"));
            }
            u32::try_from(n).expect("validated above")
        }
    };

    let rerank = obj
        .get("rerank")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let filters = obj.get("filters").cloned();

    let mode: &'static str = match obj.get("mode") {
        None => "hybrid",
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "hybrid" => "hybrid",
            "vector" => "vector",
            "fts" => "fts",
            other => return Err(format!("unknown mode `{other}` (expected hybrid|vector|fts)")),
        },
        Some(_) => return Err("`mode` must be a string".to_owned()),
    };
    // Validate the filters object against the registry before forwarding (fail fast).
    if let Some(fv) = obj.get("filters") {
        let parsed: mn_retrieval::filters::SearchFilters =
            serde_json::from_value(fv.clone()).map_err(|e| format!("invalid filters: {e}"))?;
        parsed
            .validate()
            .map_err(|e| format!("invalid filter `{}`: {}", e.facet, e.message))?;
    }

    Ok(ParsedSearchArgs {
        queries,
        limit,
        rerank,
        filters,
        mode,
    })
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
    /// `/v1/chunks/:id`
    Chunk,
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
/// `count` is applied symmetrically to prev and next. The `too_many_chunks`
/// envelope is unreachable here (neither `/next`, `/prev`, nor `/:id` raises
/// 412), so we exhaustively match it as an internal-error sanity gate.
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

/// Dispatch any of the `get_chunk*` tools. Returns the cloud's JSON verbatim.
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
        PassthroughKind::Chunk => cloud.get_chunk(id_str).await,
        PassthroughKind::Parents => cloud.get_chunk_parents(id_str).await,
        PassthroughKind::Document => cloud.get_document(id_str).await,
    };
    r.map_err(|e| match e {
        CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
        other => PassthroughError::Cloud(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_has_all_thirteen_tools() {
        let m = list();
        let names: Vec<_> = m.tools.iter().map(|t| t.name).collect();
        for expected in [
            "search",
            "get_chunk",
            "get_chunk_next",
            "get_chunk_prev",
            "get_chunk_neighbors",
            "get_chunk_parents",
            "get_document",
            "get_document_chunks",
            "list_sources",
            "facets",
            "pull_models",
            "status",
            "install_search_skill",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
        assert_eq!(names.len(), 13, "expected 13 tools, got {}", names.len());
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
    fn parse_search_args_accepts_single_query() {
        let v = json!({ "query": "hello", "limit": 5, "rerank": false });
        let p = parse_search_args(&v).unwrap();
        assert_eq!(p.queries, vec!["hello".to_owned()]);
        assert_eq!(p.limit, 5);
        assert!(!p.rerank);
    }

    #[test]
    fn parse_search_args_accepts_multi_query() {
        let v = json!({ "queries": ["a", "b", "c"] });
        let p = parse_search_args(&v).unwrap();
        assert_eq!(p.queries.len(), 3);
        assert_eq!(p.limit, DEFAULT_LIMIT);
        assert!(p.rerank);
    }

    #[test]
    fn parse_search_args_rejects_both_forms() {
        let v = json!({ "query": "a", "queries": ["b"] });
        assert!(parse_search_args(&v).is_err());
    }

    #[test]
    fn parse_search_args_rejects_empty() {
        assert!(parse_search_args(&json!({})).is_err());
        assert!(parse_search_args(&json!({"query": ""})).is_err());
        assert!(parse_search_args(&json!({"queries": []})).is_err());
    }

    #[test]
    fn parse_search_args_clamps_limit() {
        let v = json!({ "query": "x", "limit": 0 });
        assert!(parse_search_args(&v).is_err());
        let v = json!({ "query": "x", "limit": 51 });
        assert!(parse_search_args(&v).is_err());
    }

    #[test]
    fn search_input_schema_is_object_with_oneof() {
        let s = search_input_schema();
        assert_eq!(s["type"], "object");
        assert!(s.get("oneOf").is_some());
    }

    #[test]
    fn parse_rejects_unknown_mode_and_bad_filter() {
        let bad_mode = serde_json::json!({ "query": "x", "mode": "fuzzy" });
        assert!(parse_search_args(&bad_mode).is_err());
        let non_string_mode = serde_json::json!({ "query": "x", "mode": 5 });
        assert!(parse_search_args(&non_string_mode).is_err());
        let bad_filter =
            serde_json::json!({ "query": "x", "filters": { "kind": { "any_of": ["binary"] } } });
        assert!(parse_search_args(&bad_filter).is_err());
        let ok = serde_json::json!({ "query": "x", "mode": "fts", "filters": { "kind": { "any_of": ["code"] } } });
        assert!(parse_search_args(&ok).is_ok());
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
    fn status_reports_models() {
        let s = run_status(None);
        assert_eq!(s.reranker, "bge-reranker-base");
        assert!(matches!(s.model_state, ModelState::Missing | ModelState::Ready));
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

//! Typed event schemas for Gauge telemetry.
//!
//! Each struct implements [`gauge_telemetry::event::Event`] and carries ONLY
//! scalar / enum fields. Free-form strings — anything that could carry a user
//! query, a chunk, a path, or a token — never make it into an event, so the
//! canary suite can verify by type inspection that no forbidden value can
//! structurally reach the wire.

use std::borrow::Cow;

use gauge_telemetry::common::Surface;
use gauge_telemetry::event::Event;
use serde::Serialize;

/// The result tier reported by a tool / command. Coarse-grained on purpose —
/// the privacy contract requires no error message text leaves the originating
/// process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Tool ran and returned a normal result.
    Ok,
    /// Caller-side invalid input.
    InvalidInput,
    /// Cloud / IO / model failure.
    Error,
}

/// State of the local model cache when the event was emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelState {
    /// Models loaded and ready.
    Ready,
    /// Models not yet downloaded.
    Missing,
    /// Models loaded but a different revision than the corpus expects.
    Stale,
    /// Models currently downloading.
    Loading,
    /// Cached model files failed integrity check.
    Corrupt,
}

/// Closed enum of MCP tool names. Adding a new MCP tool requires a coordinated
/// bump here AND in the server-side validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpToolName {
    /// `search` tool.
    Search,
    /// `advanced_search` tool.
    AdvancedSearch,
    /// `get_chunks` tool.
    GetChunks,
    /// `get_chunk_next` tool.
    GetChunkNext,
    /// `get_chunk_prev` tool.
    GetChunkPrev,
    /// `get_chunk_neighbors` tool. Bundles prev + current + next in one call.
    GetChunkNeighbors,
    /// `get_chunk_parents` tool.
    GetChunkParents,
    /// `get_document` tool.
    GetDocument,
    /// `get_document_chunks` tool.
    GetDocumentChunks,
    /// `list_sources` tool.
    ListSources,
    /// `facets` tool.
    Facets,
    /// `status` tool.
    Status,
    /// `install_search_skill` tool.
    InstallSearchSkill,
}

/// Closed enum of CLI subcommand names. Adding a new noun-first subcommand
/// requires a bump here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliCommandName {
    /// `mnm version`
    Version,
    /// `mnm doctor`
    Doctor,
    /// `mnm status`
    Status,
    /// `mnm config` (any sub).
    Config,
    /// `mnm sources` (any sub).
    Sources,
    /// `mnm mcp` (any sub).
    Mcp,
    /// `mnm search` (when implemented).
    Search,
    /// `mnm facets`.
    Facets,
    /// `mnm models` (when implemented).
    Models,
    /// `mnm login` / `mnm auth` (Phase 7).
    Auth,
    /// `mnm telemetry` (any sub).
    Telemetry,
    /// `mnm admin` (any sub).
    Admin,
    /// `mnm ingest` (admin).
    Ingest,
    /// `mnm ratelimits` (admin).
    Ratelimits,
    /// `mnm tokenlimits` (admin).
    Tokenlimits,
    /// `mnm manifest` (any sub).
    Manifest,
    /// `mnm chunks` (any sub).
    Chunks,
    /// `mnm documents` (any sub).
    Documents,
    /// `mnm skills` (any sub).
    Skills,
}

/// One MCP tool invocation completed.
#[derive(Debug, Clone, Serialize)]
pub struct McpToolCall {
    /// Tool name (closed enum of the MCP tools).
    pub tool_name: McpToolName,
    /// End-to-end latency in milliseconds (clamped to `u32::MAX`).
    pub latency_ms: u32,
    /// How many results were returned (always 0 for non-search tools).
    pub result_count: u32,
    /// Local model state at the moment of dispatch.
    pub model_state: ModelState,
    /// Whether the caller requested local reranking on `search`.
    pub rerank_on: bool,
    /// Coarse outcome.
    pub outcome: Outcome,
    /// Corpus embedding model id (search only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corpus_model: Option<String>,
    /// Reranker that actually ran (search only; `None` when rerank was off).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reranker_used: Option<String>,
    /// Coarse confidence bucket of the top result (search only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_confidence: Option<String>,
    /// Attribution tier of the top result (search only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_attribution: Option<String>,
    /// Display name of the top result's source (search only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_source: Option<String>,
    /// Count dropped below the confidence threshold (search only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filtered_by_confidence: Option<u32>,
    /// Count removed by dedup (search only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deduplicated_count: Option<u32>,
}
impl Event for McpToolCall {
    fn name(&self) -> Cow<'_, str> {
        "mcp_tool_call".into()
    }
}

/// One rerank decision: where it ran, with what, and the outcome.
#[derive(Debug, Clone, Serialize)]
pub struct Rerank {
    /// `"local"` | `"server"` | `"off"`.
    pub placement: String,
    /// Model attempted/applied; `None` when placement was `"off"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whether a rerank was actually applied.
    pub applied: bool,
    /// Degrade reason when not applied (mirrors `search_metadata.rerank.reason`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Billed-equivalent tokens. `None` for server placement (the server tracks its own metrics).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billed_tokens: Option<u64>,
    /// Which surface emitted this rerank (the only event emitted by both).
    pub surface: Surface,
}
impl Event for Rerank {
    fn name(&self) -> Cow<'_, str> {
        "rerank".into()
    }
}

/// One top-level CLI subcommand completed.
#[derive(Debug, Clone, Serialize)]
pub struct CliCommand {
    /// Closed enum of the CLI's noun-first subcommand names.
    pub command: CliCommandName,
    /// Total wall-clock duration in milliseconds.
    pub duration_ms: u32,
    /// Coarse outcome.
    pub outcome: Outcome,
}
impl Event for CliCommand {
    fn name(&self) -> Cow<'_, str> {
        "cli_command".into()
    }
}

/// One ingest run finished (admin-side).
#[derive(Debug, Clone, Serialize)]
pub struct IngestComplete {
    /// Documents inserted as new rows (carry-forward miss).
    pub documents_added: u32,
    /// Documents whose carry-forward optimization detected a content change.
    pub documents_updated: u32,
    /// Documents the ingest skipped because their content hash matched.
    pub documents_skipped: u32,
    /// Total ingest wall-clock in milliseconds.
    pub duration_ms: u32,
    /// Final state.
    pub outcome: Outcome,
    /// Number of batches in the upload phase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_count: Option<u32>,
    /// Index of the batch that failed during upload, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_batch_index: Option<u32>,
}
impl Event for IngestComplete {
    fn name(&self) -> Cow<'_, str> {
        "ingest_complete".into()
    }
}

/// `mnm models pull` ran to completion.
#[derive(Debug, Clone, Serialize)]
pub struct PullModels {
    /// Whether the embedder was downloaded by this run (vs. cached).
    pub embedder_downloaded: bool,
    /// Whether the reranker was downloaded by this run.
    pub reranker_downloaded: bool,
    /// Combined wall-clock for both model loads in milliseconds.
    pub duration_ms: u32,
    /// Coarse outcome.
    pub outcome: Outcome,
}
impl Event for PullModels {
    fn name(&self) -> Cow<'_, str> {
        "pull_models".into()
    }
}

/// MCP server bootstrap completed.
#[derive(Debug, Clone, Serialize)]
pub struct McpStartup {
    /// Wall-clock from process start to first `tools/list` ready, in milliseconds.
    pub startup_ms: u32,
    /// Model state at startup (typically `Missing` on first run).
    pub model_state: ModelState,
}
impl Event for McpStartup {
    fn name(&self) -> Cow<'_, str> {
        "mcp_startup".into()
    }
}

/// MCP server shutting down (stdin EOF / SIGINT).
#[derive(Debug, Clone, Serialize)]
pub struct McpShutdown {
    /// Total uptime in seconds, clamped to `u32::MAX`.
    pub uptime_s: u32,
    /// Total tool calls served during this process lifetime.
    pub tools_served: u32,
}
impl Event for McpShutdown {
    fn name(&self) -> Cow<'_, str> {
        "mcp_shutdown".into()
    }
}

/// Concrete union of all event variants.
///
/// Used by [`crate::client::Client`] as a transitional concrete type while the
/// client is being migrated to the new per-struct event model (later task).
/// The wrapper is `Send + Serialize` so it can be queued and serialised by
/// the existing HTTP client without changes to that file.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum AnyEvent {
    /// One MCP tool invocation completed.
    McpToolCall(McpToolCall),
    /// One rerank decision.
    Rerank(Rerank),
    /// One top-level CLI subcommand completed.
    CliCommand(CliCommand),
    /// One ingest run finished.
    IngestComplete(IngestComplete),
    /// `mnm models pull` ran to completion.
    PullModels(PullModels),
    /// MCP server bootstrap completed.
    McpStartup(McpStartup),
    /// MCP server shutting down.
    McpShutdown(McpShutdown),
}

impl Event for AnyEvent {
    fn name(&self) -> Cow<'_, str> {
        match self {
            Self::McpToolCall(e) => e.name(),
            Self::Rerank(e) => e.name(),
            Self::CliCommand(e) => e.name(),
            Self::IngestComplete(e) => e.name(),
            Self::PullModels(e) => e.name(),
            Self::McpStartup(e) => e.name(),
            Self::McpShutdown(e) => e.name(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_bare_and_valid() {
        use gauge_telemetry::event::Event as _;
        assert_eq!(
            CliCommand {
                command: CliCommandName::Search,
                duration_ms: 1,
                outcome: Outcome::Ok
            }
            .name(),
            "cli_command"
        );
        assert_eq!(
            McpStartup {
                startup_ms: 0,
                model_state: ModelState::Missing
            }
            .name(),
            "mcp_startup"
        );
        assert_eq!(McpShutdown { uptime_s: 1, tools_served: 2 }.name(), "mcp_shutdown");
        assert_eq!(
            PullModels {
                embedder_downloaded: false,
                reranker_downloaded: false,
                duration_ms: 1,
                outcome: Outcome::Ok
            }
            .name(),
            "pull_models"
        );
    }

    #[test]
    fn ingest_complete_omits_none_fields() {
        // Gauge rejects `null`; absent options MUST be omitted, not serialized as null.
        let e = IngestComplete {
            documents_added: 1,
            documents_updated: 0,
            documents_skipped: 0,
            duration_ms: 5,
            outcome: Outcome::Ok,
            batch_count: None,
            failed_batch_index: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert!(v.get("batch_count").is_none(), "batch_count must be omitted when None");
        assert!(
            v.get("failed_batch_index").is_none(),
            "failed_batch_index must be omitted when None"
        );
    }

    #[test]
    fn mcp_tool_call_serializes_scalars_only_and_fits_attr_cap() {
        let e = McpToolCall {
            tool_name: McpToolName::Search,
            latency_ms: 12,
            result_count: 3,
            model_state: ModelState::Missing,
            rerank_on: true,
            outcome: Outcome::Ok,
            corpus_model: Some("voyage-code-3".into()),
            reranker_used: Some("rerank-2".into()),
            top_confidence: Some("high".into()),
            top_attribution: Some("official".into()),
            top_source: Some("Midnight Docs".into()),
            filtered_by_confidence: Some(1),
            deduplicated_count: Some(0),
        };
        // to_attributes enforces ≤30 attrs, scalar-only, no null/NaN.
        let attrs = gauge_telemetry::event::to_attributes(&e).expect("valid scalar event");
        assert_eq!(attrs.len(), 13);
    }

    #[test]
    fn rerank_carries_surface() {
        use gauge_telemetry::common::Surface;
        let e = Rerank {
            placement: "local".into(),
            model: Some("rerank-2".into()),
            applied: true,
            reason: None,
            billed_tokens: Some(42),
            surface: Surface::Mcp,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v.get("surface").and_then(|s| s.as_str()), Some("mcp"));
        assert!(v.get("reason").is_none(), "None reason must be omitted");
    }
}

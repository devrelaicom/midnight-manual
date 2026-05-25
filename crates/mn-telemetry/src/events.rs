//! Typed event schemas (FR-109). One enum variant per `event_type` value
//! permitted by migration `0005_telemetry_schema.sql`'s CHECK constraint.
//!
//! The discipline this module enforces is privacy-by-construction: an
//! event's payload type can only carry the documented coarse-grained
//! scalars (numbers, enums, version strings). Free-form strings — anything
//! that could carry a user query, a chunk, a path, or a token — never make
//! it into an `Event`, so the canary suite (FR-112) can verify by type
//! inspection that no forbidden value can structurally reach the wire.

use serde::Serialize;

/// Which long-running component emitted this event. The set is closed by
/// migration `0005`'s CHECK constraint; a future variant requires a
/// migration AND a server-side allowlist bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    /// `midnight-manual` / `mnm` CLI.
    Cli,
    /// `mnm mcp serve` (stdio MCP server).
    Mcp,
    /// `midnight-manual-server` (cloud).
    Server,
}

impl Component {
    /// Stable wire string. Matches the `component` CHECK constraint.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Mcp => "mcp",
            Self::Server => "server",
        }
    }
}

/// The result tier reported by a tool / command. Coarse-grained on purpose —
/// the privacy contract requires no error message text leaves the originating
/// process (Constitution VII / canary set).
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

/// Per-event-type payloads.
///
/// Every variant carries ONLY scalar / enum fields drawn from the v1 spec's
/// documented event-data list. Adding a new field requires a coordinated
/// schema bump on both the client and the server's validator (FR-109).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum EventPayload {
    /// One MCP tool invocation completed.
    McpToolCall {
        /// Tool name (closed enum of the seven tools).
        tool_name: McpToolName,
        /// End-to-end latency in milliseconds (clamped to u32::MAX).
        latency_ms: u32,
        /// How many results were returned (always 0 for non-search tools).
        result_count: u32,
        /// Local model state at the moment of dispatch.
        model_state: ModelState,
        /// Whether the caller requested local reranking on `search`.
        rerank_on: bool,
        /// Coarse outcome.
        outcome: Outcome,
    },
    /// One top-level CLI subcommand completed.
    CliCommand {
        /// Closed enum of the CLI's noun-first subcommand names (D19).
        command: CliCommandName,
        /// Total wall-clock duration.
        duration_ms: u32,
        /// Coarse outcome.
        outcome: Outcome,
    },
    /// One ingest run finished (admin-side).
    IngestComplete {
        /// Documents inserted as new rows (carry-forward miss).
        documents_added: u32,
        /// Documents whose carry-forward optimization detected a content change.
        documents_updated: u32,
        /// Documents the ingest skipped because their content hash matched.
        documents_skipped: u32,
        /// Total ingest wall-clock.
        duration_ms: u32,
        /// Final state.
        outcome: Outcome,
    },
    /// `mnm models pull` / `pull_models` MCP tool ran to completion.
    PullModels {
        /// Whether the embedder was downloaded by this run (vs. cached).
        embedder_downloaded: bool,
        /// Whether the reranker was downloaded by this run.
        reranker_downloaded: bool,
        /// Combined wall-clock for both model loads.
        duration_ms: u32,
        /// Coarse outcome.
        outcome: Outcome,
    },
    /// MCP server bootstrap completed.
    McpStartup {
        /// Wall-clock from process start to first `tools/list` ready.
        startup_ms: u32,
        /// Model state at startup (typically `Missing` on first run).
        model_state: ModelState,
    },
    /// MCP server is shutting down on stdin EOF / SIGINT.
    McpShutdown {
        /// Total uptime in seconds, clamped to u32::MAX.
        uptime_s: u32,
        /// Total tool calls served during this process lifetime.
        tools_served: u32,
    },
}

impl EventPayload {
    /// Stable wire string for this variant; matches the CHECK constraint in
    /// migration `0005_telemetry_schema.sql`.
    #[must_use]
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::McpToolCall { .. } => "mcp_tool_call",
            Self::CliCommand { .. } => "cli_command",
            Self::IngestComplete { .. } => "ingest_complete",
            Self::PullModels { .. } => "pull_models",
            Self::McpStartup { .. } => "mcp_startup",
            Self::McpShutdown { .. } => "mcp_shutdown",
        }
    }
}

/// Closed enum of MCP tool names. Adding a new MCP tool requires a coordinated
/// bump here AND in the server-side validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpToolName {
    /// `search` tool.
    Search,
    /// `get_chunk` tool.
    GetChunk,
    /// `get_chunk_siblings` tool.
    GetChunkSiblings,
    /// `get_chunk_parents` tool.
    GetChunkParents,
    /// `list_sources` tool.
    ListSources,
    /// `pull_models` tool.
    PullModels,
    /// `status` tool.
    Status,
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
    /// `mnm config` (any sub).
    Config,
    /// `mnm sources` (any sub).
    Sources,
    /// `mnm mcp` (any sub).
    Mcp,
    /// `mnm search` (when implemented).
    Search,
    /// `mnm models` (when implemented).
    Models,
    /// `mnm login` / `mnm auth` (Phase 7).
    Auth,
    /// `mnm telemetry` (any sub).
    Telemetry,
    /// `mnm ingest` (admin).
    Ingest,
    /// `mnm ratelimits` (admin).
    Ratelimits,
    /// `mnm manifest` (any sub).
    Manifest,
}

/// The top-level event envelope written to `telemetry_event_raw.fields` (plus
/// the per-row `event_type`, `component`, `version`, `request_id`).
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    /// Which component emitted this event.
    pub component: Component,
    /// Crate version that produced the event (semver string, e.g. `"0.1.0"`).
    pub version: String,
    /// Per-event-type payload.
    pub payload: EventPayload,
    /// Server-correlation id. Optional because CLI / startup events don't have
    /// a request scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl Event {
    /// Convenience constructor; sets `request_id = None`.
    #[must_use]
    pub fn new(component: Component, version: impl Into<String>, payload: EventPayload) -> Self {
        Self {
            component,
            version: version.into(),
            payload,
            request_id: None,
        }
    }

    /// Builder for `request_id`.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_strings_match_migration_check_constraint() {
        // Each variant's `event_type()` MUST exactly match one of the strings
        // listed in 0005_telemetry_schema.sql's CHECK constraint, or rows will
        // be rejected at insertion time on the server side.
        let cases: &[(EventPayload, &str)] = &[
            (
                EventPayload::McpToolCall {
                    tool_name: McpToolName::Search,
                    latency_ms: 1,
                    result_count: 1,
                    model_state: ModelState::Ready,
                    rerank_on: true,
                    outcome: Outcome::Ok,
                },
                "mcp_tool_call",
            ),
            (
                EventPayload::CliCommand {
                    command: CliCommandName::Version,
                    duration_ms: 1,
                    outcome: Outcome::Ok,
                },
                "cli_command",
            ),
            (
                EventPayload::IngestComplete {
                    documents_added: 0,
                    documents_updated: 0,
                    documents_skipped: 0,
                    duration_ms: 0,
                    outcome: Outcome::Ok,
                },
                "ingest_complete",
            ),
            (
                EventPayload::PullModels {
                    embedder_downloaded: false,
                    reranker_downloaded: false,
                    duration_ms: 0,
                    outcome: Outcome::Ok,
                },
                "pull_models",
            ),
            (
                EventPayload::McpStartup {
                    startup_ms: 0,
                    model_state: ModelState::Missing,
                },
                "mcp_startup",
            ),
            (EventPayload::McpShutdown { uptime_s: 0, tools_served: 0 }, "mcp_shutdown"),
        ];
        for (p, expected) in cases {
            assert_eq!(p.event_type(), *expected, "wrong wire string for {p:?}");
        }
    }

    #[test]
    fn cli_command_name_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(CliCommandName::Ratelimits).unwrap(),
            serde_json::Value::String("ratelimits".into())
        );
        assert_eq!(
            serde_json::to_value(CliCommandName::Sources).unwrap(),
            serde_json::Value::String("sources".into())
        );
    }

    #[test]
    fn component_wire_strings_match_migration_check_constraint() {
        assert_eq!(Component::Cli.as_wire(), "cli");
        assert_eq!(Component::Mcp.as_wire(), "mcp");
        assert_eq!(Component::Server.as_wire(), "server");
    }

    #[test]
    fn event_serializes_with_event_type_tag() {
        let e = Event::new(
            Component::Mcp,
            "0.1.0",
            EventPayload::McpStartup {
                startup_ms: 42,
                model_state: ModelState::Missing,
            },
        );
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["component"], "mcp");
        assert_eq!(v["version"], "0.1.0");
        // The internal-tagged enum surfaces the discriminator at the payload level.
        assert_eq!(v["payload"]["event_type"], "mcp_startup");
        assert_eq!(v["payload"]["startup_ms"], 42);
        assert_eq!(v["payload"]["model_state"], "missing");
        assert!(v.get("request_id").is_none(), "absent request_id is skip-serialised");
    }

    #[test]
    fn request_id_is_carried_when_set() {
        let e = Event::new(
            Component::Server,
            "0.1.0",
            EventPayload::McpStartup {
                startup_ms: 0,
                model_state: ModelState::Ready,
            },
        )
        .with_request_id("req-abc");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["request_id"], "req-abc");
    }
}

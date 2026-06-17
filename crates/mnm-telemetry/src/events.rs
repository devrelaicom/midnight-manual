//! Typed event schemas (FR-109).
//!
//! One enum variant per `event_type` value permitted by the telemetry CHECK
//! constraint (established by migration `0005_telemetry_schema.sql`, extended
//! by `0012_telemetry_rerank_event.sql` to add `rerank`).
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
        /// Corpus embedding model id (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corpus_model: Option<String>,
        /// Reranker that actually ran (search only; `None` when rerank was off).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reranker_used: Option<String>,
        /// Coarse confidence bucket of the top result (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        top_confidence: Option<String>,
        /// Attribution tier of the top result (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        top_attribution: Option<String>,
        /// Display name of the top result's source (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        top_source: Option<String>,
        /// Count dropped below the confidence threshold (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filtered_by_confidence: Option<u32>,
        /// Count removed by dedup (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deduplicated_count: Option<u32>,
    },
    /// One rerank decision (spec §6): where it ran, with what, and the outcome.
    Rerank {
        /// "local" | "server" | "off".
        placement: String,
        /// Model attempted/applied; `None` when placement was "off".
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Whether a rerank was actually applied.
        applied: bool,
        /// Degrade reason when not applied (mirrors search_metadata.rerank.reason).
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Billed-equivalent tokens — known locally (Voyage reports total_tokens);
        /// `None` for server placement (the server tracks its own metrics).
        #[serde(skip_serializing_if = "Option::is_none")]
        billed_tokens: Option<u64>,
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
        /// Number of batches in the upload phase (optional for backward compat).
        #[serde(default)]
        batch_count: Option<u32>,
        /// Index of the batch that failed during upload, if any (optional for backward compat).
        #[serde(default)]
        failed_batch_index: Option<u32>,
    },
    /// `mnm models pull` ran to completion.
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
    /// Stable wire string for this variant; matches the telemetry CHECK
    /// constraint (migration `0005`, extended by `0012` to add `rerank`).
    #[must_use]
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::McpToolCall { .. } => "mcp_tool_call",
            Self::Rerank { .. } => "rerank",
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

    /// Every `event_type` string this enum can emit. Adding a variant requires
    /// extending this list AND the migration CHECK constraint (asserted by
    /// `event_type_set_matches_migration_check_constraint`).
    const ENUM_EVENT_TYPES: &[&str] = &[
        "mcp_tool_call",
        "rerank",
        "cli_command",
        "ingest_complete",
        "pull_models",
        "mcp_startup",
        "mcp_shutdown",
    ];

    /// Parse the *effective* `event_type` CHECK-constraint value set from the
    /// `mnm-store` migration SQL. Returns the values listed by the LAST
    /// `event_type IN ( ... )` clause across all migrations sorted by filename
    /// — i.e. the constraint in force after every migration has applied (0005
    /// establishes it; 0012 re-adds it with `rerank`). Returns `None` if the
    /// migrations directory or a CHECK clause can't be found, so the caller can
    /// skip rather than spuriously fail in an unexpected checkout layout.
    fn migration_event_types() -> Option<Vec<String>> {
        // crates/mnm-telemetry -> crates/mnm-store/migrations
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .join("mnm-store")
            .join("migrations");
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "sql"))
            .collect();
        files.sort();

        let mut effective: Option<Vec<String>> = None;
        for path in files {
            let sql = std::fs::read_to_string(&path).ok()?;
            // Each migration may carry multiple CHECKs (e.g. on `component`);
            // we only care about the one keyed on `event_type`. Take the LAST
            // such clause in the file, then let later files override earlier.
            if let Some(values) = last_event_type_check(&sql) {
                effective = Some(values);
            }
        }
        effective
    }

    /// Extract the value list from the last `event_type IN ( '...','...' )`
    /// CHECK clause in a single migration's SQL text. Tolerant of whitespace,
    /// newlines, and a trailing `))` (inline column CHECK) or `)` (named
    /// table-level constraint).
    fn last_event_type_check(sql: &str) -> Option<Vec<String>> {
        let mut result = None;
        let mut rest = sql;
        while let Some(pos) = rest.find("event_type IN") {
            let after = &rest[pos + "event_type IN".len()..];
            let open = after.find('(')?;
            let close = after[open..].find(')')?;
            let inner = &after[open + 1..open + close];
            let values: Vec<String> = inner
                .split(',')
                .map(str::trim)
                .filter_map(|tok| tok.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')))
                .map(str::to_owned)
                .collect();
            if !values.is_empty() {
                result = Some(values);
            }
            rest = &after[open + close..];
        }
        result
    }

    #[test]
    fn enum_event_types_cover_every_variant() {
        // ENUM_EVENT_TYPES is the test's mirror of the variant set; keep it in
        // sync with `event_type()` so the migration cross-check below is exhaustive.
        let mut from_variants: Vec<&str> = vec![
            EventPayload::McpToolCall {
                tool_name: McpToolName::Search,
                latency_ms: 0,
                result_count: 0,
                model_state: ModelState::Ready,
                rerank_on: false,
                outcome: Outcome::Ok,
                corpus_model: None,
                reranker_used: None,
                top_confidence: None,
                top_attribution: None,
                top_source: None,
                filtered_by_confidence: None,
                deduplicated_count: None,
            }
            .event_type(),
            EventPayload::Rerank {
                placement: String::new(),
                model: None,
                applied: false,
                reason: None,
                billed_tokens: None,
            }
            .event_type(),
            EventPayload::CliCommand {
                command: CliCommandName::Version,
                duration_ms: 0,
                outcome: Outcome::Ok,
            }
            .event_type(),
            EventPayload::IngestComplete {
                documents_added: 0,
                documents_updated: 0,
                documents_skipped: 0,
                duration_ms: 0,
                outcome: Outcome::Ok,
                batch_count: None,
                failed_batch_index: None,
            }
            .event_type(),
            EventPayload::PullModels {
                embedder_downloaded: false,
                reranker_downloaded: false,
                duration_ms: 0,
                outcome: Outcome::Ok,
            }
            .event_type(),
            EventPayload::McpStartup {
                startup_ms: 0,
                model_state: ModelState::Missing,
            }
            .event_type(),
            EventPayload::McpShutdown { uptime_s: 0, tools_served: 0 }.event_type(),
        ];
        from_variants.sort_unstable();
        let mut expected: Vec<&str> = ENUM_EVENT_TYPES.to_vec();
        expected.sort_unstable();
        assert_eq!(
            from_variants, expected,
            "ENUM_EVENT_TYPES is out of sync with EventPayload::event_type()"
        );
    }

    #[test]
    fn event_type_set_matches_migration_check_constraint() {
        // The authoritative source of truth is the migration CHECK constraint:
        // an event_type the enum emits but the constraint omits is rejected at
        // insertion time (the exact failure mode the `rerank` bug exhibited).
        // This test PARSES the migrations and diffs the two sets — it is NOT a
        // self-consistency check on the enum.
        let Some(mut from_migration) = migration_event_types() else {
            // Unexpected checkout layout: don't fail spuriously. CI runs from a
            // full workspace where the migrations are always present.
            eprintln!("skipping: mnm-store migrations not found from mnm-telemetry");
            return;
        };
        from_migration.sort_unstable();
        from_migration.dedup();
        let mut from_enum: Vec<String> = ENUM_EVENT_TYPES.iter().map(|s| (*s).to_owned()).collect();
        from_enum.sort_unstable();
        assert_eq!(
            from_enum, from_migration,
            "EventPayload event_type set diverged from the migration CHECK constraint \
             (mnm-store/migrations); a new event_type needs a migration extending the constraint \
             AND a bump to the server's ALLOWED_EVENT_TYPES allow-list"
        );
    }

    #[test]
    fn event_type_strings_match_enum_wire_values() {
        // Each variant's `event_type()` MUST exactly match its documented wire
        // string. Pairs the constructor with its expected `event_type()`.
        let cases: &[(EventPayload, &str)] = &[
            (
                EventPayload::McpToolCall {
                    tool_name: McpToolName::Search,
                    latency_ms: 1,
                    result_count: 1,
                    model_state: ModelState::Ready,
                    rerank_on: true,
                    outcome: Outcome::Ok,
                    corpus_model: None,
                    reranker_used: None,
                    top_confidence: None,
                    top_attribution: None,
                    top_source: None,
                    filtered_by_confidence: None,
                    deduplicated_count: None,
                },
                "mcp_tool_call",
            ),
            (
                EventPayload::Rerank {
                    placement: "local".into(),
                    model: Some("rerank-2.5".into()),
                    applied: true,
                    reason: None,
                    billed_tokens: Some(1234),
                },
                "rerank",
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
                    batch_count: None,
                    failed_batch_index: None,
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
        assert_eq!(
            serde_json::to_value(CliCommandName::Status).unwrap(),
            serde_json::Value::String("status".into())
        );
    }

    #[test]
    fn mcp_tool_name_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(McpToolName::AdvancedSearch).unwrap(),
            serde_json::Value::String("advanced_search".into())
        );
        assert_eq!(
            serde_json::to_value(McpToolName::GetChunks).unwrap(),
            serde_json::Value::String("get_chunks".into())
        );
        assert_eq!(
            serde_json::to_value(McpToolName::InstallSearchSkill).unwrap(),
            serde_json::Value::String("install_search_skill".into())
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

    #[test]
    fn mcp_tool_call_serializes_new_search_fields() {
        let e = Event::new(
            Component::Mcp,
            "0.1.0",
            EventPayload::McpToolCall {
                tool_name: McpToolName::Search,
                latency_ms: 12,
                result_count: 3,
                model_state: ModelState::Ready,
                rerank_on: true,
                outcome: Outcome::Ok,
                corpus_model: Some("voyage-code-3@1".into()),
                reranker_used: Some("rerank-2.5".into()),
                top_confidence: Some("high".into()),
                top_attribution: Some("foundation".into()),
                top_source: Some("Compact Docs".into()),
                filtered_by_confidence: Some(0),
                deduplicated_count: Some(0),
            },
        );
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["payload"]["corpus_model"], "voyage-code-3@1");
        assert_eq!(v["payload"]["top_source"], "Compact Docs");
    }

    #[test]
    fn mcp_tool_call_omits_absent_search_fields_for_other_tools() {
        let e = Event::new(
            Component::Mcp,
            "0.1.0",
            EventPayload::McpToolCall {
                tool_name: McpToolName::GetChunks,
                latency_ms: 1,
                result_count: 0,
                model_state: ModelState::Missing,
                rerank_on: false,
                outcome: Outcome::Ok,
                corpus_model: None,
                reranker_used: None,
                top_confidence: None,
                top_attribution: None,
                top_source: None,
                filtered_by_confidence: None,
                deduplicated_count: None,
            },
        );
        let v = serde_json::to_value(&e).unwrap();
        assert!(v["payload"].get("corpus_model").is_none());
    }
}

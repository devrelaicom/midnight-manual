//! Privacy canary: assert no event type can leak a forbidden substring.
//! Uses Gauge's `assert_no_forbidden` over a representative instance of each
//! event, with the shared forbidden-substring corpus.

pub use gauge_telemetry::FORBIDDEN_SUBSTRINGS as FORBIDDEN;

/// Probe prefix for leak-detection tests.
///
/// A string that appears in [`FORBIDDEN`]. Use it to construct probe inputs
/// (e.g. `format!("{CANARY_PREFIX}my_token")`) — any rendered string
/// containing the prefix will be caught by [`find_first_match`].
///
/// The value is `"@"` (the first entry of `FORBIDDEN_SUBSTRINGS`).
pub const CANARY_PREFIX: &str = "@";

/// Return the first [`FORBIDDEN`] substring found in `s`, or `None`.
///
/// Use this in assertion messages for output leak-detection tests.
pub fn find_first_match(s: &str) -> Option<&'static str> {
    FORBIDDEN
        .iter()
        .copied()
        .find(|&forbidden| s.contains(forbidden))
}

#[cfg(test)]
mod tests {
    use gauge_telemetry::assert_no_forbidden;

    use crate::events::*;
    use crate::Surface;

    #[test]
    fn cli_command_is_clean() {
        assert_no_forbidden(
            &CliCommand {
                command: CliCommandName::Search,
                duration_ms: 9,
                outcome: Outcome::Ok,
            },
            FORBIDDEN_LIST,
        );
    }

    #[test]
    fn pull_models_is_clean() {
        assert_no_forbidden(
            &PullModels {
                embedder_downloaded: true,
                reranker_downloaded: false,
                duration_ms: 9,
                outcome: Outcome::Ok,
            },
            FORBIDDEN_LIST,
        );
    }

    #[test]
    fn ingest_complete_is_clean() {
        assert_no_forbidden(
            &IngestComplete {
                documents_added: 1,
                documents_updated: 2,
                documents_skipped: 3,
                duration_ms: 9,
                outcome: Outcome::Ok,
                batch_count: Some(1),
                failed_batch_index: None,
            },
            FORBIDDEN_LIST,
        );
    }

    #[test]
    fn rerank_is_clean() {
        assert_no_forbidden(
            &Rerank {
                placement: "local".into(),
                model: Some("rerank-2".into()),
                applied: true,
                reason: Some("ok".into()),
                billed_tokens: Some(1),
                surface: Surface::Cli,
            },
            FORBIDDEN_LIST,
        );
    }

    #[test]
    fn mcp_lifecycle_is_clean() {
        assert_no_forbidden(
            &McpStartup {
                startup_ms: 0,
                model_state: ModelState::Missing,
            },
            FORBIDDEN_LIST,
        );
        assert_no_forbidden(&McpShutdown { uptime_s: 1, tools_served: 2 }, FORBIDDEN_LIST);
    }

    #[test]
    fn mcp_tool_call_is_clean() {
        // Free-form fields (corpus_model, top_source, ...) are the real risk:
        // the scalar validator passes them, so the canary is the only backstop.
        assert_no_forbidden(
            &McpToolCall {
                tool_name: McpToolName::Search,
                latency_ms: 1,
                result_count: 1,
                model_state: ModelState::Ready,
                rerank_on: false,
                outcome: Outcome::Ok,
                corpus_model: Some("voyage-code-3".into()),
                reranker_used: None,
                top_confidence: Some("high".into()),
                top_attribution: Some("official".into()),
                top_source: Some("Midnight Docs".into()),
                filtered_by_confidence: Some(0),
                deduplicated_count: Some(0),
            },
            FORBIDDEN_LIST,
        );
    }

    #[test]
    fn mcp_param_alias_rewrite_is_clean() {
        // Two closed enums only (tool + alias) — structurally leak-proof, but
        // pin it against the corpus so a future field can't slip a query in.
        assert_no_forbidden(
            &McpParamAliasRewrite {
                tool_name: McpToolName::AdvancedSearch,
                alias: ParamAlias::QueryToQueries,
            },
            FORBIDDEN_LIST,
        );
    }

    const FORBIDDEN_LIST: &[&str] = super::FORBIDDEN;
}

//! Shapes every tool result into the MCP "summary + structuredContent" form.
//!
//! Success → one `text` content block (`summary` + the trimmed JSON in a fenced
//! ```json block) plus full-fidelity `structuredContent` (with `next_actions`).
//! Failure → an `isError: true` result carrying a shared error envelope.

use serde_json::{json, Value};

use crate::protocol::{ContentBlock, ToolCallResult};

/// A suggested follow-up tool call surfaced to the agent.
#[derive(Debug, Clone)]
pub struct NextAction {
    /// Tool name to call next.
    pub tool: &'static str,
    /// Arguments object for that call.
    pub arguments: Value,
}

impl NextAction {
    fn to_value(&self) -> Value {
        json!({ "tool": self.tool, "arguments": self.arguments })
    }
}

fn next_actions_value(actions: &[NextAction]) -> Value {
    Value::Array(actions.iter().map(NextAction::to_value).collect())
}

/// Retrieval-quality facts the search projector hands to the telemetry emitter.
#[derive(Debug, Clone, Default)]
pub struct SearchTelemetry {
    /// Embedding model used for the corpus that was queried.
    pub corpus_model: Option<String>,
    /// Reranker model applied, if any.
    pub reranker_used: Option<String>,
    /// Confidence bucket of the top result (e.g. `"high"`, `"medium"`, `"low"`).
    pub top_confidence_bucket: Option<&'static str>,
    /// Attribution string of the top result.
    pub top_attribution: Option<String>,
    /// Source identifier of the top result.
    pub top_source: Option<String>,
    /// Number of results filtered out by the confidence threshold.
    pub filtered_by_confidence: Option<u32>,
    /// Number of duplicate results removed by the deduplication step.
    pub deduplicated_count: Option<u32>,
    /// Number of results returned to the caller after all filtering.
    pub result_count: u32,
}

/// A successful tool result, pre-render.
pub struct ToolOutcome {
    /// Concise, agent-facing summary line(s).
    pub summary: String,
    /// Full canonical payload (becomes `structuredContent`; `next_actions` injected at render).
    pub structured: Value,
    /// Essentials-only view embedded as the fenced JSON in the text block.
    pub trimmed: Value,
    /// Suggested follow-ups.
    pub next_actions: Vec<NextAction>,
    /// Optional telemetry facts (search only).
    pub telemetry: Option<SearchTelemetry>,
}

impl ToolOutcome {
    /// Convenience constructor for non-search tools (no telemetry facts).
    pub const fn new(summary: String, structured: Value, trimmed: Value, next_actions: Vec<NextAction>) -> Self {
        Self { summary, structured, trimmed, next_actions, telemetry: None }
    }

    /// Render into the wire `ToolCallResult`.
    pub fn into_result(self) -> ToolCallResult {
        let mut structured = self.structured;
        if let Value::Object(map) = &mut structured {
            map.insert("next_actions".to_owned(), next_actions_value(&self.next_actions));
        }
        let trimmed = serde_json::to_string(&self.trimmed).unwrap_or_else(|_| "{}".to_owned());
        ToolCallResult {
            content: vec![ContentBlock::Text { text: format!("{}\n\n```json\n{trimmed}\n```", self.summary) }],
            structured_content: Some(structured),
            is_error: false,
        }
    }
}

/// Closed set of tool-execution error kinds.
#[derive(Debug, Clone, Copy)]
pub enum ErrorKind {
    /// The caller supplied an argument that failed validation.
    InvalidInput,
    /// The requested resource (chunk, document, source) does not exist.
    NotFound,
    /// The embedding model used by the corpus does not match the local model.
    EmbeddingModelMismatch,
    /// The ingest batch exceeded the maximum allowed chunk count.
    TooManyChunks,
    /// A transient or permanent error was returned by the cloud API.
    CloudError,
    /// The local embedding or reranker model could not be loaded.
    ModelLoadFailed,
    /// The `install_search_skill` tool failed to write the skill file.
    InstallFailed,
}

impl ErrorKind {
    const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "INVALID_INPUT",
            Self::NotFound => "NOT_FOUND",
            Self::EmbeddingModelMismatch => "EMBEDDING_MODEL_MISMATCH",
            Self::TooManyChunks => "TOO_MANY_CHUNKS",
            Self::CloudError => "CLOUD_ERROR",
            Self::ModelLoadFailed => "MODEL_LOAD_FAILED",
            Self::InstallFailed => "INSTALL_FAILED",
        }
    }
    const fn retryable(self) -> bool {
        match self {
            Self::NotFound => false,
            Self::InvalidInput
            | Self::EmbeddingModelMismatch
            | Self::TooManyChunks
            | Self::CloudError
            | Self::ModelLoadFailed
            | Self::InstallFailed => true,
        }
    }
}

/// A tool-execution failure, pre-render (becomes an `isError: true` result).
pub struct ToolFailure {
    /// Canonical error kind (determines `code` and `retryable` in the wire envelope).
    pub kind: ErrorKind,
    /// Human-readable error message included in `structuredContent`.
    pub message: String,
    /// Agent-facing recovery guidance placed in the `text` content block.
    pub guidance: String,
    /// Extra fields merged into the `error` object (e.g. mismatch / too_many_chunks data).
    pub details: Value,
    /// Suggested follow-up tool calls.
    pub next_actions: Vec<NextAction>,
}

impl ToolFailure {
    /// Minimal failure with no extra details and no next actions.
    pub fn simple(kind: ErrorKind, message: impl Into<String>, guidance: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            guidance: guidance.into(),
            details: Value::Null,
            next_actions: Vec::new(),
        }
    }

    /// Render into the wire `ToolCallResult` (`isError: true`).
    pub fn into_result(self) -> ToolCallResult {
        let mut error = json!({
            "code": self.kind.code(),
            "retryable": self.kind.retryable(),
            "message": self.message,
        });
        if let (Value::Object(emap), Value::Object(dmap)) = (&mut error, &self.details) {
            for (k, v) in dmap {
                emap.insert(k.clone(), v.clone());
            }
        }
        let structured = json!({
            "error": error,
            "next_actions": next_actions_value(&self.next_actions),
        });
        let trimmed = json!({ "error": { "code": self.kind.code(), "retryable": self.kind.retryable() } });
        let trimmed = serde_json::to_string(&trimmed).unwrap_or_else(|_| "{}".to_owned());
        ToolCallResult {
            content: vec![ContentBlock::Text { text: format!("{}\n\n```json\n{trimmed}\n```", self.guidance) }],
            structured_content: Some(structured),
            is_error: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_renders_summary_then_fenced_json_and_structured() {
        let o = ToolOutcome::new(
            "Found 1.".into(),
            json!({ "results": [1] }),
            json!({ "match_count": 1 }),
            vec![NextAction { tool: "get_chunk", arguments: json!({ "id": "abc" }) }],
        );
        let r = o.into_result();
        assert!(!r.is_error);
        let text = match &r.content[0] { ContentBlock::Text { text } => text };
        assert!(text.starts_with("Found 1.\n\n```json\n"));
        assert!(text.contains("\"match_count\":1"));
        let sc = r.structured_content.unwrap();
        assert_eq!(sc["results"][0], 1);
        assert_eq!(sc["next_actions"][0]["tool"], "get_chunk");
    }

    #[test]
    fn failure_renders_iserror_with_envelope() {
        let f = ToolFailure::simple(ErrorKind::NotFound, "no chunk abc", "Verify the id from a recent search.");
        let r = f.into_result();
        assert!(r.is_error);
        let sc = r.structured_content.unwrap();
        assert_eq!(sc["error"]["code"], "NOT_FOUND");
        assert_eq!(sc["error"]["retryable"], false);
    }
}

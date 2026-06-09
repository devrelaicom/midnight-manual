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

/// Map a confidence in [0,1] to a coarse bucket label (telemetry-safe; never the raw float).
fn confidence_bucket(c: f64) -> &'static str {
    if c >= 0.85 {
        "high"
    } else if c >= 0.7 {
        "medium"
    } else if c >= 0.5 {
        "low"
    } else {
        "very_low"
    }
}

/// Walk `v` along `path`, returning the final `str` value if every segment exists and is a string.
fn str_field<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str()
}

/// Project the cloud search envelope. `reranker_used` is the local reranker name when local
/// rerank ran, else `None`.
#[allow(clippy::too_many_lines, clippy::option_if_let_else)]
pub fn project_search(envelope: Value, reranker_used: Option<&str>) -> ToolOutcome {
    let corpus_model = envelope
        .get("corpus_embedding_model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let results = envelope
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let result_count = u32::try_from(results.len()).unwrap_or(u32::MAX);
    let filtered = envelope
        .pointer("/search_metadata/filtered_by_confidence")
        .and_then(Value::as_u64);
    let deduped = envelope
        .pointer("/search_metadata/deduplicated_count")
        .and_then(Value::as_u64);

    // Trimmed: per-result essentials, scoring stripped.
    let trimmed_results: Vec<Value> = results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            json!({
                "rank": i + 1,
                "chunk_id": r.get("chunk_id").cloned().unwrap_or(Value::Null),
                "document_id": r.get("document_id").cloned().unwrap_or(Value::Null),
                "source_path": r.get("source_path").cloned().unwrap_or(Value::Null),
                "source_display_name": r.get("source_display_name").cloned().unwrap_or(Value::Null),
                "heading_path": r.get("heading_path").cloned().unwrap_or(json!([])),
                "confidence": r.pointer("/scores/confidence").cloned().unwrap_or(Value::Null),
                "attribution": str_field(r, &["scores", "confidence_factors", "attribution"]).unwrap_or(""),
                "content": r.get("content").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    // Summary from the top result.
    let top = results.first();
    let summary = match top {
        Some(t) => {
            let path = t
                .get("source_path")
                .and_then(Value::as_str)
                .unwrap_or("(unknown)");
            let heading = t
                .get("heading_path")
                .and_then(Value::as_array)
                .map(|h| {
                    h.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" › ")
                })
                .filter(|s| !s.is_empty());
            let attr = str_field(t, &["scores", "confidence_factors", "attribution"])
                .unwrap_or("unknown");
            let conf = t
                .pointer("/scores/confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let chunk_id = t.get("chunk_id").and_then(Value::as_str).unwrap_or("?");
            let model = corpus_model.as_deref().unwrap_or("?");
            let where_ =
                heading.map_or_else(|| path.to_owned(), |h| format!("{path} › {h}"));
            format!(
                "Search: {result_count} matches, corpus {model}. Top: {where_} [{attr} · {conf:.2}] chunk {chunk_id} — fetch with get_chunk."
            )
        }
        None => format!(
            "Search: 0 matches, corpus {}.",
            corpus_model.as_deref().unwrap_or("?")
        ),
    };

    // next_actions from the top result.
    let next_actions = top
        .map(|t| {
            let mut v = Vec::new();
            if let Some(id) = t.get("chunk_id").and_then(Value::as_str) {
                v.push(NextAction {
                    tool: "get_chunk",
                    arguments: json!({ "id": id }),
                });
            }
            if let Some(id) = t.get("document_id").and_then(Value::as_str) {
                v.push(NextAction {
                    tool: "get_document",
                    arguments: json!({ "id": id }),
                });
            }
            v
        })
        .unwrap_or_default();

    // Telemetry facts.
    let telemetry = SearchTelemetry {
        corpus_model,
        reranker_used: reranker_used.map(str::to_owned),
        top_confidence_bucket: top
            .and_then(|t| t.pointer("/scores/confidence").and_then(Value::as_f64))
            .map(confidence_bucket),
        top_attribution: top.and_then(|t| {
            str_field(t, &["scores", "confidence_factors", "attribution"]).map(str::to_owned)
        }),
        top_source: top.and_then(|t| {
            t.get("source_display_name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        filtered_by_confidence: filtered
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
        deduplicated_count: deduped.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
        result_count,
    };

    ToolOutcome {
        summary,
        structured: envelope,
        trimmed: json!({ "results": trimmed_results, "match_count": result_count }),
        next_actions,
        telemetry: Some(telemetry),
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

    fn sample_search_envelope() -> Value {
        json!({
            "corpus_embedding_model": "voyage-code-3@1",
            "results": [{
                "chunk_id": "1f39", "document_id": "7d5c",
                "source_slug": "compact-docs", "source_display_name": "Compact Docs",
                "source_path": "docs/intro.md", "heading_path": ["Compiling", "Witnesses"],
                "symbol_path": [], "content": "withVacantWitnesses ...",
                "scores": { "confidence": 0.81, "trust_score": 1.0,
                            "confidence_factors": { "attribution": "foundation", "verified": true } }
            }],
            "search_metadata": { "filtered_by_confidence": 0, "deduplicated_count": 0 }
        })
    }

    #[test]
    fn project_search_summary_and_telemetry() {
        let o = super::project_search(sample_search_envelope(), Some("bge-reranker-base"));
        assert!(o.summary.contains("docs/intro.md"));
        assert!(o.summary.contains("Compact Docs") || o.summary.contains("foundation"));
        assert!(o.trimmed["results"][0].get("scores").is_none());
        assert_eq!(o.trimmed["results"][0]["source_path"], "docs/intro.md");
        assert!(o.structured["results"][0].get("scores").is_some());
        assert_eq!(o.next_actions[0].tool, "get_chunk");
        assert_eq!(o.next_actions[0].arguments["id"], "1f39");
        let t = o.telemetry.unwrap();
        assert_eq!(t.result_count, 1);
        assert_eq!(t.corpus_model.as_deref(), Some("voyage-code-3@1"));
        assert_eq!(t.top_attribution.as_deref(), Some("foundation"));
        assert_eq!(t.top_source.as_deref(), Some("Compact Docs"));
        assert_eq!(t.reranker_used.as_deref(), Some("bge-reranker-base"));
    }

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
    fn confidence_bucket_thresholds() {
        assert_eq!(super::confidence_bucket(0.85), "high");
        assert_eq!(super::confidence_bucket(0.84), "medium");
        assert_eq!(super::confidence_bucket(0.70), "medium");
        assert_eq!(super::confidence_bucket(0.69), "low");
        assert_eq!(super::confidence_bucket(0.50), "low");
        assert_eq!(super::confidence_bucket(0.49), "very_low");
        assert_eq!(super::confidence_bucket(0.0),  "very_low");
    }

    #[test]
    fn project_search_empty_results() {
        let envelope = json!({
            "corpus_embedding_model": "",
            "results": [],
            "search_metadata": { "filtered_by_confidence": 2, "deduplicated_count": 0 }
        });
        let o = super::project_search(envelope, None);
        assert!(o.summary.contains("0 matches"));
        assert!(!o.summary.contains("corpus . "));   // empty model must not leak into summary
        assert!(o.next_actions.is_empty());
        let t = o.telemetry.unwrap();
        assert_eq!(t.result_count, 0);
        assert!(t.top_confidence_bucket.is_none());
        assert!(t.corpus_model.is_none());           // "" treated as absent
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

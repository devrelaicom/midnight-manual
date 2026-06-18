//! Shapes every tool result into the MCP "summary + structuredContent" form.
//!
//! Success → one `text` content block (`summary` + the trimmed JSON in a fenced
//! ```json block) plus full-fidelity `structuredContent` (with
//! `suggested_next_actions`). Failure → an `isError: true` result carrying a
//! shared error envelope.

use mnm_core::introspect::{MeRateLimit, MeTokenLimits};
use serde_json::{json, Value};

use crate::protocol::{ContentBlock, ToolCallResult};

/// A suggested follow-up surfaced to the agent. `tool: None` describes a user
/// action (e.g. "ask the user to restart the harness") rather than a tool call.
#[derive(Debug, Clone)]
pub struct NextAction {
    /// What this action achieves, as a human-written sentence.
    pub description: String,
    /// Tool name to call next (`None` for user actions).
    pub tool: Option<&'static str>,
    /// Arguments object for that call (`None` for user actions).
    pub arguments: Option<Value>,
}

impl NextAction {
    /// Tool-call action.
    pub fn call(description: impl Into<String>, tool: &'static str, arguments: Value) -> Self {
        Self {
            description: description.into(),
            tool: Some(tool),
            arguments: Some(arguments),
        }
    }

    /// User action (no tool).
    pub fn user(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            tool: None,
            arguments: None,
        }
    }

    fn to_value(&self) -> Value {
        let mut o = json!({ "description": self.description });
        if let Some(t) = self.tool {
            o["tool"] = json!(t);
        }
        if let Some(a) = &self.arguments {
            o["arguments"] = a.clone();
        }
        o
    }
}

fn suggested_next_actions_value(actions: &[NextAction]) -> Value {
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
    /// Full canonical payload (becomes `structuredContent`; `suggested_next_actions`
    /// injected at render).
    pub structured: Value,
    /// Essentials-only view embedded as the fenced JSON in the text block.
    pub trimmed: Value,
    /// Suggested follow-ups.
    pub suggested_next_actions: Vec<NextAction>,
    /// Optional telemetry facts (search only).
    pub telemetry: Option<SearchTelemetry>,
}

impl ToolOutcome {
    /// Convenience constructor for non-search tools (no telemetry facts).
    pub const fn new(
        summary: String,
        structured: Value,
        trimmed: Value,
        suggested_next_actions: Vec<NextAction>,
    ) -> Self {
        Self {
            summary,
            structured,
            trimmed,
            suggested_next_actions,
            telemetry: None,
        }
    }

    /// Render into the wire `ToolCallResult`.
    pub fn into_result(self) -> ToolCallResult {
        let mut structured = self.structured;
        if let Value::Object(map) = &mut structured {
            map.insert(
                "suggested_next_actions".to_owned(),
                suggested_next_actions_value(&self.suggested_next_actions),
            );
        }
        let trimmed = serde_json::to_string(&self.trimmed).unwrap_or_else(|_| "{}".to_owned());
        ToolCallResult {
            content: vec![ContentBlock::Text {
                text: format!("{}\n\n```json\n{trimmed}\n```", self.summary),
            }],
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

/// First ~150 chars of a chunk body on a char boundary, ellipsised.
fn snippet(content: &str) -> String {
    const MAX: usize = 150;
    if content.chars().count() <= MAX {
        return content.to_owned();
    }
    let head: String = content.chars().take(MAX).collect();
    format!("{head}…")
}

/// Trimmed per-chunk entry for multi-chunk text fences. `c` is a chunk-with-context
/// object: chunk fields flattened at top level, `document`/`source` nested.
fn chunk_brief(c: &Value) -> Value {
    json!({
        "id": c.get("id").cloned().unwrap_or(Value::Null),
        "source_path": c.pointer("/document/source_path").cloned().unwrap_or(Value::Null),
        "heading_path": c.get("heading_path").cloned().unwrap_or(json!([])),
        "snippet": c.get("content").and_then(Value::as_str).map(snippet),
    })
}

/// `chunk_brief` over the array at `pointer` (empty array if absent or not an array).
fn chunk_briefs_at(env: &Value, pointer: &str) -> Value {
    Value::Array(
        env.pointer(pointer)
            .and_then(Value::as_array)
            .map(|a| a.iter().map(chunk_brief).collect())
            .unwrap_or_default(),
    )
}

/// How [`project_search`] should render the cloud envelope.
#[derive(Debug, Clone, Default)]
pub struct SearchRenderOpts {
    /// Reranker model name when local rerank ran.
    pub reranker_used: Option<String>,
    /// `true` for advanced_search (keeps matched_queries; basic strips it).
    pub advanced: bool,
    /// Whether the midnight-advanced-search skill is installed locally.
    pub skill_installed: bool,
}

/// Fewer fused candidates than this triggers the "install the skill" nudge
/// (when the skill is not already installed).
const FEW_CANDIDATES_THRESHOLD: u64 = 5;

/// Project the cloud search envelope for `search` / `advanced_search`.
#[allow(clippy::too_many_lines, clippy::option_if_let_else)]
pub fn project_search(envelope: Value, opts: &SearchRenderOpts) -> ToolOutcome {
    let mut envelope = envelope;
    // Basic search hides the multi-query machinery: strip `matched_queries`
    // from each result's `scores` (advanced_search keeps it).
    if !opts.advanced {
        if let Some(results) = envelope.get_mut("results").and_then(Value::as_array_mut) {
            for r in results {
                if let Some(scores) = r.get_mut("scores").and_then(Value::as_object_mut) {
                    scores.remove("matched_queries");
                }
            }
        }
    }
    let corpus_model = envelope
        .get("corpus_embedding_model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let total_candidates = envelope
        .pointer("/search_metadata/total_candidates")
        .and_then(Value::as_u64);
    let results = envelope
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let result_count = u32::try_from(results.len()).unwrap_or(u32::MAX);
    let filtered = envelope
        .pointer("/search_metadata/filtered_by_confidence")
        .and_then(Value::as_u64);
    let deduped = envelope.get("search_metadata").map(|m| {
        let dropped = m
            .get("overlap_dropped_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let trimmed = m
            .get("overlap_trimmed_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        dropped + trimmed
    });

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

    // Summary from the top result. The "(N candidates)" parenthetical is
    // omitted when the cloud's search_metadata lacks `total_candidates`.
    let candidates_note =
        total_candidates.map_or_else(String::new, |c| format!(" ({c} candidates)"));
    let top = results.first();
    let mut summary = match top {
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
            let attr =
                str_field(t, &["scores", "confidence_factors", "attribution"]).unwrap_or("unknown");
            let conf = t
                .pointer("/scores/confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let where_ = heading.map_or_else(|| path.to_owned(), |h| format!("{path} › {h}"));
            format!("{result_count} matches{candidates_note}. Top: {where_} [{attr} · {conf:.2}].")
        }
        None => format!("0 matches{candidates_note}."),
    };

    // suggested_next_actions from the top result + the top-5 batch fetch.
    let top5: Vec<&str> = results
        .iter()
        .take(5)
        .filter_map(|r| r.get("chunk_id").and_then(Value::as_str))
        .collect();
    let mut suggested_next_actions = Vec::new();
    if let Some(t) = top {
        if let Some(id) = t.get("chunk_id").and_then(Value::as_str) {
            suggested_next_actions.push(NextAction::call(
                "Fetch the top-ranked chunk's full content",
                "get_chunks",
                json!({ "ids": [id] }),
            ));
            suggested_next_actions.push(NextAction::call(
                "Read the chunks surrounding the top result for more context",
                "get_chunk_neighbors",
                json!({ "id": id }),
            ));
        }
        if top5.len() > 1 {
            suggested_next_actions.push(NextAction::call(
                "Fetch the top 5 ranked chunks' content in one call",
                "get_chunks",
                json!({ "ids": top5 }),
            ));
        }
        if let Some(d) = t.get("document_id").and_then(Value::as_str) {
            suggested_next_actions.push(NextAction::call(
                "Get the top result's parent document overview and chunk map",
                "get_document",
                json!({ "id": d }),
            ));
        }
    }

    // Low-candidate nudge: the corpus barely matched and the advanced-search
    // skill isn't installed — teach the agent how to get it.
    if total_candidates.unwrap_or(0) < FEW_CANDIDATES_THRESHOLD && !opts.skill_installed {
        summary.push_str(
            "\nFew candidates matched — the midnight-advanced-search skill teaches query \
             patterns that find more (run install_search_skill).",
        );
        suggested_next_actions.push(NextAction::call(
            "Install the midnight-advanced-search skill to learn higher-recall query patterns",
            "install_search_skill",
            json!({}),
        ));
    }

    // Telemetry facts.
    let telemetry = SearchTelemetry {
        corpus_model,
        reranker_used: opts.reranker_used.clone(),
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
        filtered_by_confidence: filtered.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
        deduplicated_count: deduped.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
        result_count,
    };

    // Promote rank/confidence/attribution to the top level of each structured
    // result so structuredContent, the trimmed text fence, and the advertised
    // outputSchema all expose the same fields at the same path (issue #88).
    promote_result_scores(&mut envelope);

    ToolOutcome {
        summary,
        structured: envelope,
        trimmed: json!({ "results": trimmed_results, "match_count": result_count }),
        suggested_next_actions,
        telemetry: Some(telemetry),
    }
}

/// Copy `confidence`, `attribution`, and the 1-based `rank` to the top level of
/// each `results[]` entry, leaving the nested `scores` block in place (additive).
/// The trimmed text fence already foregrounds these (`project_search` above);
/// this brings `structuredContent` into agreement so the two channels and the
/// `search_output_schema` no longer disagree on where the fields live (#88).
fn promote_result_scores(env: &mut Value) {
    let Some(results) = env.get_mut("results").and_then(Value::as_array_mut) else {
        return;
    };
    for (i, r) in results.iter_mut().enumerate() {
        let Some(obj) = r.as_object_mut() else {
            continue;
        };
        let scores = obj.get("scores");
        let confidence = scores
            .and_then(|s| s.get("confidence"))
            .cloned()
            .unwrap_or(Value::Null);
        let attribution = scores
            .and_then(|s| s.get("confidence_factors"))
            .and_then(|f| f.get("attribution"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        obj.insert("rank".to_owned(), json!(i + 1));
        obj.insert("confidence".to_owned(), confidence);
        obj.insert("attribution".to_owned(), Value::String(attribution));
    }
}

/// `get_chunks`: `{ chunks: [ChunkWithContext..], missing: [id..] }`.
/// Single chunk → FULL content in the text fence (legacy text-only clients
/// must receive the payload). Multiple → per-chunk snippets.
pub fn project_chunks(env: Value) -> ToolOutcome {
    let chunks = env
        .get("chunks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let missing: Vec<String> = env
        .get("missing")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let n = chunks.len();
    let missing_note = if missing.is_empty() {
        String::new()
    } else {
        format!(" ({} id(s) not found: {})", missing.len(), missing.join(", "))
    };
    let (summary, trimmed) = if n == 1 {
        let c = &chunks[0];
        let id = c.get("id").and_then(Value::as_str).unwrap_or("?");
        let path = c
            .pointer("/document/source_path")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)");
        let heading = c
            .get("heading_path")
            .and_then(Value::as_array)
            .map(|h| {
                h.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" › ")
            })
            .filter(|s| !s.is_empty());
        let where_ = heading.map_or_else(|| path.to_owned(), |h| format!("{path} › {h}"));
        (
            format!("Chunk {id} — {where_}.{missing_note}"),
            json!({
                "id": id,
                // Text-only clients can't read suggested_next_actions, so the
                // fence carries the document id they'd need to navigate up.
                "document_id": c.get("document_id").cloned().unwrap_or(Value::Null),
                "source_path": path,
                "heading_path": c.get("heading_path").cloned().unwrap_or(json!([])),
                "content": c.get("content").cloned().unwrap_or(Value::Null),
            }),
        )
    } else {
        (
            format!("{n} chunks fetched.{missing_note}"),
            json!({ "count": n, "chunks": chunks.iter().map(chunk_brief).collect::<Vec<_>>() }),
        )
    };
    let mut actions = Vec::new();
    if let Some(first) = chunks.first() {
        if let Some(id) = first.get("id").and_then(Value::as_str) {
            actions.push(NextAction::call(
                "Read the chunks surrounding the first fetched chunk",
                "get_chunk_neighbors",
                json!({ "id": id }),
            ));
        }
        if let Some(d) = first.get("document_id").and_then(Value::as_str) {
            actions.push(NextAction::call(
                "Fetch the first chunk's parent document overview and chunk map",
                "get_document",
                json!({ "id": d }),
            ));
        }
    }
    ToolOutcome::new(summary, env, trimmed, actions)
}

/// `get_chunk_next` / `get_chunk_prev`: `{ chunks: [ChunkWithContext,..] }`. `direction` = "after"/"before".
pub fn project_chunk_list(env: Value, direction: &str) -> ToolOutcome {
    let chunks_len = env
        .get("chunks")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let trimmed = json!({ "count": chunks_len, "chunks": chunk_briefs_at(&env, "/chunks") });
    if chunks_len == 0 {
        return ToolOutcome::new(
            format!("No more chunks {direction} the anchor."),
            env,
            trimmed,
            vec![],
        );
    }
    let first = env
        .pointer("/chunks/0/id")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned();
    // Page from the boundary chunk in the same direction.
    let page_action = if direction == "after" {
        let last_idx = chunks_len - 1;
        let last = env
            .pointer(&format!("/chunks/{last_idx}/id"))
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        NextAction::call(
            "Continue reading past the last returned chunk",
            "get_chunk_next",
            json!({ "id": last }),
        )
    } else {
        NextAction::call(
            "Continue reading before the first returned chunk",
            "get_chunk_prev",
            json!({ "id": first }),
        )
    };
    let summary = format!("{chunks_len} chunk(s) {direction} the anchor (first: {first}).");
    ToolOutcome::new(summary, env, trimmed, vec![page_action])
}

/// `get_chunk_neighbors`: `{ prev: {chunks:[..]}, chunk: <ChunkWithContext>, next: {chunks:[..]} }`.
pub fn project_neighbors(env: Value) -> ToolOutcome {
    let prev = env
        .pointer("/prev/chunks")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let next = env
        .pointer("/next/chunks")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let id = env
        .pointer("/chunk/id")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned();
    let doc_id = env
        .pointer("/chunk/document_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let summary = format!("{} neighbor(s) around {id} ({prev} before, {next} after).", prev + next);
    let trimmed = json!({
        "prev": prev,
        "next": next,
        "chunks": {
            "prev": chunk_briefs_at(&env, "/prev/chunks"),
            "anchor": env.get("chunk").map_or(Value::Null, chunk_brief),
            "next": chunk_briefs_at(&env, "/next/chunks"),
        }
    });
    let suggested_next_actions = doc_id
        .map(|d| {
            vec![NextAction::call(
                "Fetch the parent document's overview and chunk map",
                "get_document",
                json!({ "id": d }),
            )]
        })
        .unwrap_or_default();
    ToolOutcome::new(summary, env, trimmed, suggested_next_actions)
}

/// `get_chunk_parents`: `{ parents: [ParentNode..], source: {slug, display_name} }`,
/// ordered immediate parent → root.
pub fn project_parents(env: Value) -> ToolOutcome {
    let parents = env
        .get("parents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let n = parents.len();
    let source_name = env
        .pointer("/source/display_name")
        .and_then(Value::as_str)
        .or_else(|| env.pointer("/source/slug").and_then(Value::as_str))
        .unwrap_or("(unknown)");
    let mut lines = Vec::with_capacity(n);
    for p in &parents {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let kind = p.get("kind").and_then(Value::as_str).unwrap_or("?");
        let id = p.get("id").and_then(Value::as_str).unwrap_or("?");
        lines.push(format!("  {name} ({kind}) — {id}"));
    }
    let summary =
        format!("{n} ancestor(s), root last — source: {source_name}\n{}", lines.join("\n"));
    let trimmed = json!({
        "count": n,
        "source": env.get("source").cloned().unwrap_or(Value::Null),
        "parents": parents.iter().map(|p| json!({
            "id": p.get("id").cloned().unwrap_or(Value::Null),
            "name": p.get("name").cloned().unwrap_or(Value::Null),
            "kind": p.get("kind").cloned().unwrap_or(Value::Null),
            "document_id": p.get("document_id").cloned().unwrap_or(Value::Null),
        })).collect::<Vec<_>>(),
    });
    // Only the document-kind node maps to a fetchable document.
    let actions = parents
        .iter()
        .find(|p| p.get("kind").and_then(Value::as_str) == Some("document"))
        .and_then(|p| p.get("document_id").and_then(Value::as_str))
        .map(|d| {
            vec![NextAction::call(
                "Fetch the containing document's overview and chunk map",
                "get_document",
                json!({ "id": d }),
            )]
        })
        .unwrap_or_default();
    ToolOutcome::new(summary, env, trimmed, actions)
}

/// Skeleton entries are small (`{id, chunk_index, token_count}`), so the text fence carries
/// up to this many before truncating; the full set is always in `structuredContent`.
const FENCE_SKELETON_CAP: usize = 50;

/// `get_document` (DocumentOverview): Document flattened to top level; `source` nested;
/// metadata + chunk skeleton (`chunks: [{id, chunk_index, token_count}]`).
pub fn project_document(env: Value) -> ToolOutcome {
    let path = env
        .get("source_path")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let name = env
        .pointer("/source/display_name")
        .and_then(Value::as_str)
        .or_else(|| env.pointer("/source/slug").and_then(Value::as_str))
        .unwrap_or("");
    let id = env
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned();
    let skeleton = env
        .get("chunks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let n = skeleton.len();
    let tokens: i64 = skeleton
        .iter()
        .filter_map(|c| c.get("token_count").and_then(Value::as_i64))
        .sum();
    let summary = format!("{path} ({name}): {n} chunks, ~{tokens} tokens.");
    let trimmed = json!({
        "id": id, "source_path": path, "chunk_count": n, "total_tokens": tokens,
        "chunks": skeleton.iter().take(FENCE_SKELETON_CAP).cloned().collect::<Vec<_>>(),
    });
    let suggested_next_actions = vec![NextAction::call(
        "Read the document's chunk bodies from the beginning",
        "get_document_chunks",
        json!({ "id": id, "from": 0 }),
    )];
    ToolOutcome::new(summary, env, trimmed, suggested_next_actions)
}

/// `get_document_chunks` (DocumentChunkWindow): Document flattened; window meta top-level.
/// The text fence carries per-chunk briefs (`{chunk_id, chunk_index, snippet}`); full
/// bodies stay in `structuredContent`.
pub fn project_document_window(env: Value) -> ToolOutcome {
    let path = env
        .get("source_path")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)")
        .to_owned();
    let id = env
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned();
    let from = env.get("from").and_then(Value::as_u64).unwrap_or(0);
    let n = u64::try_from(
        env.get("chunks")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
    )
    .unwrap_or(0);
    let total = env.get("total_chunks").and_then(Value::as_u64).unwrap_or(0);
    let to = from + n;
    let summary = format!("Chunks {from}..{to} of {path} (of {total}).");
    // NOTE: window chunks are ChunkBody (`chunk_id`, no nested `document`), so
    // `chunk_brief` (ChunkWithContext shape) does not apply here.
    let briefs: Vec<Value> = env
        .get("chunks")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|c| {
                    json!({
                        "chunk_id": c.get("chunk_id").cloned().unwrap_or(Value::Null),
                        "chunk_index": c.get("chunk_index").cloned().unwrap_or(Value::Null),
                        "snippet": c.get("content").and_then(Value::as_str).map(snippet),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let trimmed = json!({
        "source_path": path, "from": from, "to": to, "total_chunks": total, "chunks": briefs,
    });
    let mut suggested_next_actions = if to < total {
        vec![NextAction::call(
            "Read the next window of chunk bodies",
            "get_document_chunks",
            json!({ "id": id, "from": to }),
        )]
    } else {
        vec![]
    };
    suggested_next_actions.push(NextAction::call(
        "Fetch the document overview and full chunk map",
        "get_document",
        json!({ "id": id }),
    ));
    ToolOutcome::new(summary, env, trimmed, suggested_next_actions)
}

/// `list_sources`: a paginated `{ sources: [..], total, next_cursor }` envelope.
pub fn project_sources(env: Value) -> ToolOutcome {
    let sources = env
        .get("sources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let n = sources.len();
    let total = env.get("total").and_then(Value::as_i64).unwrap_or_else(|| {
        n.try_into().unwrap_or(i64::MAX) // usize→i64 can't fail for real page sizes
    });
    let next_cursor = env
        .get("next_cursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let more = if next_cursor.is_some() {
        " More available — pass cursor."
    } else {
        ""
    };
    let summary = format!("Showing {n} of {total} sources.{more}");
    let brief: Vec<Value> = sources
        .iter()
        .map(|s| {
            json!({
                "id": s.get("id").cloned().unwrap_or(Value::Null),
                "slug": s.get("slug").cloned().unwrap_or(Value::Null),
                "display_name": s.get("display_name").cloned().unwrap_or(Value::Null),
                "kind": s.get("kind").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let trimmed = json!({ "count": n, "total": total, "sources": brief });
    let mut actions = Vec::new();
    if let Some(c) = next_cursor {
        actions.push(NextAction::call(
            "Fetch the next page of sources",
            "list_sources",
            json!({ "cursor": c }),
        ));
    }
    if let Some(slug) = sources
        .first()
        .and_then(|s| s.get("slug"))
        .and_then(Value::as_str)
    {
        actions.push(NextAction::call(
            format!("Restrict a search to the `{slug}` source (swap in your own query and slug)"),
            "advanced_search",
            json!({ "queries": ["<your query>"], "filters": { "source_slug": { "any_of": [slug] } } }),
        ));
    }
    ToolOutcome::new(summary, env, trimmed, actions)
}

/// `facets`: two shapes from `GET /v1/facets`.
///
/// - Overview (no params): `{ modes:[..], filters:[ {key, type, negatable,
///   values?, truncated?, total?}, .. ] }`. Dimensions are `filters[].key`.
/// - Drill-down (`?facet=..`): `{ facet, values:[string], total, next_cursor }`
///   — distinguished by the top-level `facet` key.
pub fn project_facets(env: Value) -> ToolOutcome {
    if let Some(facet) = env.get("facet").and_then(Value::as_str).map(str::to_owned) {
        // Drill-down page.
        let values = env
            .get("values")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let n = values.len();
        let total = env.get("total").and_then(Value::as_i64).unwrap_or_else(|| {
            n.try_into().unwrap_or(i64::MAX) // usize→i64 can't fail for real page sizes
        });
        let next_cursor = env
            .get("next_cursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let summary = format!("{facet}: showing {n} of {total} values.");
        let mut actions = Vec::new();
        if let Some(c) = next_cursor {
            actions.push(NextAction::call(
                format!("Fetch the next page of `{facet}` values"),
                "facets",
                json!({ "facet": facet, "cursor": c }),
            ));
        }
        if let Some(v) = values.first().and_then(Value::as_str) {
            // `json!` needs literal object keys; the facet name is dynamic.
            let mut filters = serde_json::Map::new();
            filters.insert(facet.clone(), json!({ "any_of": [v] }));
            actions.push(NextAction::call(
                format!("Search filtered to {facet}=`{v}` (swap in your own query and value)"),
                "advanced_search",
                json!({ "queries": ["<your query>"], "filters": filters }),
            ));
        }
        let trimmed = json!({ "facet": facet, "values": values, "total": total });
        return ToolOutcome::new(summary, env, trimmed, actions);
    }
    // Overview.
    let dims = env
        .get("filters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let keys: Vec<String> = dims
        .iter()
        .filter_map(|f| f.get("key").and_then(Value::as_str).map(str::to_owned))
        .collect();
    // `facets({facet})` is literal guidance for the calling agent (pass a
    // facet name), not a formatting placeholder.
    #[allow(clippy::literal_string_with_formatting_args)]
    let summary = format!(
        "{} filter dimensions for advanced_search: {}. Open-set dimensions show samples — drill in with facets({{facet}}).",
        keys.len(),
        keys.join(", ")
    );
    let trimmed = json!({ "dimensions": dims.iter().map(|f| json!({
        "key": f.get("key").cloned().unwrap_or(Value::Null),
        "type": f.get("type").cloned().unwrap_or(Value::Null),
        "values": f.get("values").cloned().unwrap_or(Value::Null),
        "total": f.get("total").cloned().unwrap_or(Value::Null),
    })).collect::<Vec<_>>() });
    // Concrete example from real corpus data: first dimension that has a
    // non-empty values array.
    let mut actions = Vec::new();
    if let Some((key, v)) = dims.iter().find_map(|f| {
        let key = f.get("key").and_then(Value::as_str)?;
        let v = f
            .get("values")
            .and_then(Value::as_array)?
            .first()?
            .as_str()?;
        Some((key.to_owned(), v.to_owned()))
    }) {
        // `json!` needs literal object keys; the facet name is dynamic.
        let mut filters = serde_json::Map::new();
        filters.insert(key.clone(), json!({ "any_of": [v] }));
        actions.push(NextAction::call(
            format!("Search filtered to {key}=`{v}` (swap in your own query and value)"),
            "advanced_search",
            json!({ "queries": ["<your query>"], "filters": filters }),
        ));
    }
    actions.push(NextAction::call(
        "Page through every value of an open-set facet (e.g. tags)",
        "facets",
        json!({ "facet": "tags" }),
    ));
    ToolOutcome::new(summary, env, trimmed, actions)
}

/// `status` (StatusReport as JSON).
pub fn project_status(env: Value) -> ToolOutcome {
    let s = |k: &str| env.get(k).and_then(Value::as_str).unwrap_or("?").to_owned();
    let cloud = s("cloud");
    let cloud_ver = env
        .get("cloud_version")
        .and_then(Value::as_str)
        .map(|v| format!(" (v{v})"))
        .unwrap_or_default();
    let auth = if env
        .get("authenticated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        format!(
            "{} {} ({})",
            s("auth_type"),
            env.get("identity").and_then(Value::as_str).unwrap_or("?"),
            s("permission_level")
        )
    } else {
        "anonymous (read)".to_owned()
    };
    // Read the two limit systems through the shared typed contract rather than
    // stringly-typed pointers, so a server-side field rename is a compile-time
    // break in `mnm_core::introspect` instead of a silent blank section.
    let rl = env
        .get("rate_limit")
        .filter(|v| !v.is_null())
        .and_then(|r| serde_json::from_value::<MeRateLimit>(r.clone()).ok())
        .map(|r| format!("; requests {}/{}", r.remaining, r.limit))
        .unwrap_or_default();
    let tl = env
        .get("token_limits")
        .filter(|v| !v.is_null())
        .and_then(|t| serde_json::from_value::<MeTokenLimits>(t.clone()).ok())
        .map(|t| {
            format!(
                "; embed tokens {}/{} hr · {}/{} day",
                t.hourly.remaining, t.hourly.limit, t.daily.remaining, t.daily.limit,
            )
        })
        .unwrap_or_default();
    let reranker = format!(
        "{} {}",
        s("reranker"),
        if env
            .get("reranker_loaded")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "loaded"
        } else {
            "not loaded (loads on first reranked search)"
        }
    );
    let summary = format!(
        "Cloud {cloud}{cloud_ver}; auth: {auth}{rl}{tl}; Voyage key {}; reranker {reranker}.",
        s("voyage").replace('_', " "),
    );
    let trimmed = json!({
        "cloud": env.get("cloud").cloned().unwrap_or(Value::Null),
        "authenticated": env.get("authenticated").cloned().unwrap_or(Value::Null),
        "auth_type": env.get("auth_type").cloned().unwrap_or(Value::Null),
        "voyage": env.get("voyage").cloned().unwrap_or(Value::Null),
        "rate_limit": env.get("rate_limit").cloned().unwrap_or(Value::Null),
        "token_limits": env.get("token_limits").cloned().unwrap_or(Value::Null),
    });
    let mut actions = Vec::new();
    if s("voyage") == "invalid_key" {
        actions.push(NextAction::user(
            "Ask the user to check their VOYAGE_API_KEY — the Voyage API rejected it",
        ));
    }
    if !env
        .get("authenticated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        actions.push(NextAction::user(
            "For higher rate limits, ask the user to run `mnm auth github`",
        ));
    }
    ToolOutcome::new(summary, env, trimmed, actions)
}

/// `install_search_skill` (InstallReport as JSON).
pub fn project_install(env: Value) -> ToolOutcome {
    let scope = env.get("scope").and_then(Value::as_str).unwrap_or("user");
    let skill = env
        .get("skill_name")
        .and_then(Value::as_str)
        .unwrap_or("midnight-advanced-search");
    let empty = vec![];
    let installed = env
        .get("installed")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let names: Vec<&str> = installed
        .iter()
        .filter_map(|i| i.get("harness").and_then(Value::as_str))
        .collect();
    let summary = format!(
        "Installed/updated `{skill}` for {} (scope: {scope}). The skill is NOT active yet — \
         ask the user to restart their session or refresh their skills, then it will load automatically.",
        if names.is_empty() { "no harnesses".to_owned() } else { names.join(", ") },
    );
    let trimmed = json!({
        "skill_name": skill, "scope": scope,
        "detected": env.get("detected").cloned().unwrap_or(json!([])),
        "not_detected": env.get("not_detected").cloned().unwrap_or(json!([])),
        "actions": installed.iter().map(|i| json!({
            "harness": i.get("harness").cloned().unwrap_or(Value::Null),
            "action": i.get("action").cloned().unwrap_or(Value::Null),
        })).collect::<Vec<_>>(),
    });
    let actions = installed
        .iter()
        .filter_map(|i| {
            let h = i.get("harness").and_then(Value::as_str)?;
            let step = i.get("reload_step").and_then(Value::as_str)?;
            Some(NextAction::user(format!("[{h}] Ask the user to: {step}")))
        })
        .collect();
    ToolOutcome::new(summary, env, trimmed, actions)
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
    /// A transient or permanent error was returned by the cloud API.
    CloudError,
    /// The `install_search_skill` tool failed to write the skill file.
    InstallFailed,
}

impl ErrorKind {
    /// Every error kind, in canonical (code-set) order. The single source of
    /// truth for the closed `code` set the `errorSchema` enumerates and the
    /// contract's `error_envelope` documents.
    pub const ALL: [Self; 5] = [
        Self::InvalidInput,
        Self::NotFound,
        Self::EmbeddingModelMismatch,
        Self::CloudError,
        Self::InstallFailed,
    ];

    /// The closed-set wire `code` string for this kind.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "INVALID_INPUT",
            Self::NotFound => "NOT_FOUND",
            Self::EmbeddingModelMismatch => "EMBEDDING_MODEL_MISMATCH",
            Self::CloudError => "CLOUD_ERROR",
            Self::InstallFailed => "INSTALL_FAILED",
        }
    }
    const fn retryable(self) -> bool {
        match self {
            // Permanent for an identical retry — recovery requires a suggested action.
            Self::NotFound | Self::EmbeddingModelMismatch => false,
            // Transient or fixable-and-retry.
            Self::InvalidInput | Self::CloudError | Self::InstallFailed => true,
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
    /// Extra fields merged into the `error` object (e.g. mismatch data).
    pub details: Value,
    /// Suggested follow-ups (tool calls or user actions).
    pub suggested_next_actions: Vec<NextAction>,
}

impl ToolFailure {
    /// Minimal failure with no extra details and no next actions.
    pub fn simple(
        kind: ErrorKind,
        message: impl Into<String>,
        guidance: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            guidance: guidance.into(),
            details: Value::Null,
            suggested_next_actions: Vec::new(),
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
            "suggested_next_actions": suggested_next_actions_value(&self.suggested_next_actions),
        });
        let trimmed =
            json!({ "error": { "code": self.kind.code(), "retryable": self.kind.retryable() } });
        let trimmed = serde_json::to_string(&trimmed).unwrap_or_else(|_| "{}".to_owned());
        ToolCallResult {
            content: vec![ContentBlock::Text {
                text: format!("{}\n\n```json\n{trimmed}\n```", self.guidance),
            }],
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
                "scores": { "confidence": 0.81, "trust_score": 1.0, "matched_queries": [0],
                            "confidence_factors": { "attribution": "foundation", "verified": true } }
            }],
            "search_metadata": { "filtered_by_confidence": 0, "deduplicated_count": 0,
                                 "total_candidates": 37 }
        })
    }

    /// Opts for a reranked basic search with the skill already installed
    /// (keeps the nudge out of tests that aren't about it).
    fn basic_opts() -> SearchRenderOpts {
        SearchRenderOpts {
            reranker_used: Some("rerank-2.5".to_owned()),
            advanced: false,
            skill_installed: true,
        }
    }

    #[test]
    fn project_search_summary_and_telemetry() {
        let o = super::project_search(sample_search_envelope(), &basic_opts());
        assert!(o.summary.contains("docs/intro.md"));
        assert!(o.summary.contains("(37 candidates)"));
        assert!(o.summary.contains("foundation"));
        assert!(!o.summary.contains("corpus"), "summary must not name the corpus model");
        assert!(o.trimmed["results"][0].get("scores").is_none());
        assert_eq!(o.trimmed["results"][0]["source_path"], "docs/intro.md");
        assert!(o.structured["results"][0].get("scores").is_some());
        assert_eq!(o.suggested_next_actions[0].tool, Some("get_chunks"));
        assert_eq!(o.suggested_next_actions[0].arguments.as_ref().unwrap()["ids"], json!(["1f39"]));
        let t = o.telemetry.unwrap();
        assert_eq!(t.result_count, 1);
        assert_eq!(t.corpus_model.as_deref(), Some("voyage-code-3@1"));
        assert_eq!(t.top_attribution.as_deref(), Some("foundation"));
        assert_eq!(t.top_source.as_deref(), Some("Compact Docs"));
        assert_eq!(t.reranker_used.as_deref(), Some("rerank-2.5"));
    }

    #[test]
    fn project_search_promotes_confidence_attribution_rank_to_result_top_level() {
        // #88: structuredContent must expose confidence/attribution/rank at the
        // same top-level path the trimmed text fence uses, while keeping the
        // nested scores block for full fidelity.
        let o = super::project_search(sample_search_envelope(), &basic_opts());
        let result = &o.structured["results"][0];
        assert_eq!(result["rank"], json!(1));
        assert_eq!(result["confidence"], json!(0.81));
        assert_eq!(result["attribution"], json!("foundation"));
        // The nested cloud shape is untouched (additive promotion).
        assert_eq!(result["scores"]["confidence"], json!(0.81));
        assert_eq!(result["scores"]["confidence_factors"]["attribution"], json!("foundation"));
    }

    #[test]
    fn project_search_basic_strips_matched_queries_advanced_keeps_it() {
        let basic = super::project_search(sample_search_envelope(), &basic_opts());
        assert!(
            basic.structured["results"][0]["scores"]
                .get("matched_queries")
                .is_none(),
            "basic search must strip scores.matched_queries"
        );
        let opts = SearchRenderOpts { advanced: true, ..basic_opts() };
        let advanced = super::project_search(sample_search_envelope(), &opts);
        assert_eq!(
            advanced.structured["results"][0]["scores"]["matched_queries"],
            json!([0]),
            "advanced_search must keep scores.matched_queries"
        );
    }

    fn envelope_with_candidates(total_candidates: u64) -> Value {
        let mut env = sample_search_envelope();
        env["search_metadata"]["total_candidates"] = json!(total_candidates);
        env
    }

    #[test]
    fn project_search_nudges_on_few_candidates_without_skill() {
        let opts = SearchRenderOpts {
            skill_installed: false,
            ..basic_opts()
        };
        let o = super::project_search(envelope_with_candidates(2), &opts);
        assert!(o.summary.contains("install_search_skill"), "summary must carry the nudge");
        assert!(
            o.suggested_next_actions
                .iter()
                .any(|a| a.tool == Some("install_search_skill")),
            "nudge must add an install_search_skill action"
        );
    }

    #[test]
    fn project_search_nudge_boundary_is_strict_less_than_five() {
        let opts = SearchRenderOpts {
            skill_installed: false,
            ..basic_opts()
        };
        let o = super::project_search(envelope_with_candidates(5), &opts);
        assert!(
            !o.summary.contains("install_search_skill"),
            "exactly 5 candidates must NOT nudge (threshold is strict <5)"
        );
        let o = super::project_search(envelope_with_candidates(4), &opts);
        assert!(o.summary.contains("install_search_skill"), "4 candidates must nudge");
    }

    #[test]
    fn project_search_nudges_when_total_candidates_absent() {
        // An envelope with no search_metadata.total_candidates treats the
        // count as 0 — nudge fires when the skill is absent.
        let opts = SearchRenderOpts {
            skill_installed: false,
            ..basic_opts()
        };
        let mut env = sample_search_envelope();
        if let Some(meta) = env["search_metadata"].as_object_mut() {
            meta.remove("total_candidates");
        }
        let o = super::project_search(env, &opts);
        assert!(o.summary.contains("install_search_skill"));
    }

    #[test]
    fn project_search_no_nudge_when_skill_installed() {
        let opts = SearchRenderOpts {
            skill_installed: true,
            ..basic_opts()
        };
        let o = super::project_search(envelope_with_candidates(2), &opts);
        assert!(!o.summary.contains("install_search_skill"));
        assert!(!o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("install_search_skill")));
    }

    #[test]
    fn project_search_no_nudge_when_candidates_plentiful() {
        let opts = SearchRenderOpts {
            skill_installed: false,
            ..basic_opts()
        };
        let o = super::project_search(envelope_with_candidates(50), &opts);
        assert!(!o.summary.contains("install_search_skill"));
        assert!(!o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("install_search_skill")));
    }

    #[test]
    fn project_search_actions_cover_single_top5_neighbors_and_document() {
        let mut env = sample_search_envelope();
        let results: Vec<Value> = (0..6)
            .map(|i| {
                json!({
                    "chunk_id": format!("c{i}"), "document_id": "d0",
                    "source_path": "docs/x.md", "heading_path": [], "content": "body",
                    "scores": { "confidence": 0.9,
                                "confidence_factors": { "attribution": "foundation" } }
                })
            })
            .collect();
        env["results"] = Value::Array(results);
        let o = super::project_search(env, &basic_opts());
        let find = |tool: &str, pred: &dyn Fn(&NextAction) -> bool| {
            o.suggested_next_actions
                .iter()
                .find(|a| a.tool == Some(tool) && pred(a))
                .unwrap_or_else(|| panic!("missing action for {tool}"))
                .clone()
        };
        let single = find("get_chunks", &|a| a.arguments.as_ref().unwrap()["ids"] == json!(["c0"]));
        let batch = find("get_chunks", &|a| {
            a.arguments.as_ref().unwrap()["ids"]
                .as_array()
                .unwrap()
                .len()
                == 5
        });
        let neighbors =
            find("get_chunk_neighbors", &|a| a.arguments.as_ref().unwrap()["id"] == "c0");
        let document = find("get_document", &|a| a.arguments.as_ref().unwrap()["id"] == "d0");
        for a in [single, batch, neighbors, document] {
            assert!(!a.description.is_empty(), "every action needs a description");
        }
    }

    #[test]
    fn outcome_renders_summary_then_fenced_json_and_structured() {
        let o = ToolOutcome::new(
            "Found 1.".into(),
            json!({ "results": [1] }),
            json!({ "match_count": 1 }),
            vec![NextAction::call(
                "Fetch the chunk's full content",
                "get_chunks",
                json!({ "ids": ["abc"] }),
            )],
        );
        let r = o.into_result();
        assert!(!r.is_error);
        let text = match &r.content[0] {
            ContentBlock::Text { text } => text,
        };
        assert!(text.starts_with("Found 1.\n\n```json\n"));
        assert!(text.contains("\"match_count\":1"));
        let sc = r.structured_content.unwrap();
        assert_eq!(sc["results"][0], 1);
        assert_eq!(sc["suggested_next_actions"][0]["tool"], "get_chunks");
    }

    #[test]
    fn confidence_bucket_thresholds() {
        assert_eq!(super::confidence_bucket(0.85), "high");
        assert_eq!(super::confidence_bucket(0.84), "medium");
        assert_eq!(super::confidence_bucket(0.70), "medium");
        assert_eq!(super::confidence_bucket(0.69), "low");
        assert_eq!(super::confidence_bucket(0.50), "low");
        assert_eq!(super::confidence_bucket(0.49), "very_low");
        assert_eq!(super::confidence_bucket(0.0), "very_low");
    }

    #[test]
    fn project_search_empty_results() {
        let envelope = json!({
            "corpus_embedding_model": "",
            "results": [],
            "search_metadata": { "filtered_by_confidence": 2, "deduplicated_count": 0,
                                 "total_candidates": 3 }
        });
        let o = super::project_search(envelope, &SearchRenderOpts::default());
        assert!(o.summary.starts_with("0 matches (3 candidates)."));
        assert!(!o.summary.contains("corpus")); // no corpus model in the summary
                                                // 3 candidates < 5 and the skill isn't installed → nudge only.
        assert_eq!(o.suggested_next_actions.len(), 1);
        assert_eq!(o.suggested_next_actions[0].tool, Some("install_search_skill"));
        let t = o.telemetry.unwrap();
        assert_eq!(t.result_count, 0);
        assert!(t.top_confidence_bucket.is_none());
        assert!(t.corpus_model.is_none()); // "" treated as absent
    }

    #[test]
    fn project_search_omits_candidates_note_when_absent() {
        let envelope = json!({
            "corpus_embedding_model": "voyage-code-3@1",
            "results": [],
            "search_metadata": { "filtered_by_confidence": 0, "deduplicated_count": 0 }
        });
        let opts = SearchRenderOpts {
            skill_installed: true,
            ..SearchRenderOpts::default()
        };
        let o = super::project_search(envelope, &opts);
        assert!(o.summary.starts_with("0 matches."));
        assert!(!o.summary.contains("candidates"));
    }

    #[test]
    fn failure_renders_iserror_with_envelope() {
        let f = ToolFailure::simple(
            ErrorKind::NotFound,
            "no chunk abc",
            "Verify the id from a recent search.",
        );
        let r = f.into_result();
        assert!(r.is_error);
        let sc = r.structured_content.unwrap();
        assert_eq!(sc["error"]["code"], "NOT_FOUND");
        assert_eq!(sc["error"]["retryable"], false);
    }

    #[test]
    fn project_chunks_single_carries_full_content_in_fence() {
        let long_body = "x".repeat(500); // way past the snippet cut — must survive whole
        let env = json!({
            "chunks": [{
                "id": "c1", "document_id": "d1", "chunk_index": 4, "total_chunks": 35,
                "content": long_body,
                "heading_path": ["A", "B"],
                "document": { "source_path": "docs/intro.md" },
                "source": { "display_name": "Compact Docs" }
            }],
            "missing": []
        });
        let o = super::project_chunks(env);
        assert!(o.summary.contains("c1"));
        assert!(o.summary.contains("docs/intro.md › A › B"));
        // Legacy text-only clients read the fence: full content, not a snippet.
        assert_eq!(o.trimmed["content"], json!(long_body));
        assert!(o.trimmed.get("snippet").is_none());
        assert_eq!(o.trimmed["heading_path"], json!(["A", "B"]));
        // Actions: neighbors of the chunk + its parent document.
        let tools: Vec<_> = o.suggested_next_actions.iter().map(|a| a.tool).collect();
        assert_eq!(tools, vec![Some("get_chunk_neighbors"), Some("get_document")]);
        assert_eq!(o.suggested_next_actions[0].arguments.as_ref().unwrap()["id"], "c1");
        assert_eq!(o.suggested_next_actions[1].arguments.as_ref().unwrap()["id"], "d1");
        for a in &o.suggested_next_actions {
            assert!(!a.description.is_empty());
        }
    }

    #[test]
    fn project_chunks_multi_uses_snippets_not_full_content() {
        let long_body = "y".repeat(500);
        let env = json!({
            "chunks": [
                { "id": "c1", "document_id": "d1", "content": long_body,
                  "document": { "source_path": "docs/a.md" } },
                { "id": "c2", "content": "short two",
                  "document": { "source_path": "docs/b.md" } }
            ],
            "missing": []
        });
        let o = super::project_chunks(env);
        assert!(o.summary.contains("2 chunks fetched."));
        assert_eq!(o.trimmed["count"], 2);
        let s = o.trimmed["chunks"][0]["snippet"].as_str().unwrap();
        assert!(s.ends_with('…'), "long bodies must be snipped in multi mode");
        assert_eq!(s.chars().count(), 151);
        assert_eq!(o.trimmed["chunks"][1]["snippet"], "short two");
        assert!(
            o.trimmed["chunks"][0].get("content").is_none(),
            "multi mode must not carry full content in the fence"
        );
        // structuredContent keeps full fidelity regardless.
        assert_eq!(o.structured["chunks"][0]["content"], json!(long_body));
    }

    #[test]
    fn project_chunks_reports_missing_ids_in_summary() {
        let env = json!({
            "chunks": [{ "id": "c1", "content": "body",
                         "document": { "source_path": "docs/a.md" } }],
            "missing": ["m1", "m2"]
        });
        let o = super::project_chunks(env);
        assert!(o.summary.contains("(2 id(s) not found: m1, m2)"));
    }

    #[test]
    fn project_chunks_all_missing_is_success_with_no_actions() {
        // The cloud answers 200 with partial semantics — not an isError.
        let env = json!({ "chunks": [], "missing": ["m1", "m2", "m3"] });
        let o = super::project_chunks(env);
        assert_eq!(o.summary, "0 chunks fetched. (3 id(s) not found: m1, m2, m3)");
        assert!(o.suggested_next_actions.is_empty());
        assert_eq!(o.trimmed["count"], 0);
        assert!(!o.into_result().is_error);
    }

    #[test]
    fn project_chunk_list_counts() {
        let env = json!({ "chunks": [
            { "id": "a", "content": "alpha body", "heading_path": ["H1"],
              "document": { "source_path": "docs/a.md" } },
            { "id": "b", "content": "beta body",
              "document": { "source_path": "docs/a.md" } }
        ] });
        let o = super::project_chunk_list(env, "after");
        assert!(o.summary.contains('2'));
        assert_eq!(o.trimmed["count"], 2);
        // The fence must carry per-chunk briefs with snippets for text-only clients.
        assert_eq!(o.trimmed["chunks"][0]["id"], "a");
        assert_eq!(o.trimmed["chunks"][0]["source_path"], "docs/a.md");
        assert_eq!(o.trimmed["chunks"][0]["snippet"], "alpha body");
        assert_eq!(o.trimmed["chunks"][1]["snippet"], "beta body");
    }

    #[test]
    fn project_parents_enriched_shape_with_document_action() {
        let env = json!({
            "parents": [
                { "id": "p1", "source_version_id": "sv1", "parent_node_id": "p2",
                  "kind": "document", "name": "intro.md", "order_index": 0,
                  "document_id": "d1" },
                { "id": "p2", "source_version_id": "sv1", "parent_node_id": "p3",
                  "kind": "group", "name": "guides", "order_index": 1,
                  "document_id": null },
                { "id": "p3", "source_version_id": "sv1", "parent_node_id": null,
                  "kind": "root", "name": "/", "order_index": 0,
                  "document_id": null }
            ],
            "source": { "slug": "compact-docs", "display_name": "Compact Docs" }
        });
        let o = super::project_parents(env.clone());
        let mut summary_lines = o.summary.lines();
        assert_eq!(
            summary_lines.next().unwrap(),
            "3 ancestor(s), root last — source: Compact Docs"
        );
        assert_eq!(summary_lines.next().unwrap(), "  intro.md (document) — p1");
        assert_eq!(summary_lines.next().unwrap(), "  guides (group) — p2");
        assert_eq!(summary_lines.next().unwrap(), "  / (root) — p3");
        // structured is the cloud envelope as-is — no extra wrapping.
        assert_eq!(o.structured, env);
        assert_eq!(o.trimmed["count"], 3);
        assert_eq!(o.trimmed["source"]["display_name"], "Compact Docs");
        assert_eq!(o.trimmed["parents"][0]["document_id"], "d1");
        assert_eq!(o.trimmed["parents"][1]["document_id"], Value::Null);
        assert_eq!(o.trimmed["parents"][2]["kind"], "root");
        // The document-kind node's document_id drives the next action.
        assert_eq!(o.suggested_next_actions.len(), 1);
        let action = &o.suggested_next_actions[0];
        assert_eq!(action.description, "Fetch the containing document's overview and chunk map");
        assert_eq!(action.tool, Some("get_document"));
        assert_eq!(action.arguments.as_ref().unwrap()["id"], "d1");
    }

    #[test]
    fn project_parents_no_document_node_yields_no_actions() {
        let env = json!({
            "parents": [
                { "id": "p2", "kind": "group", "name": "guides", "document_id": null },
                { "id": "p3", "kind": "root", "name": "/", "document_id": null }
            ],
            "source": { "slug": "compact-docs", "display_name": "Compact Docs" }
        });
        let o = super::project_parents(env);
        assert!(o.suggested_next_actions.is_empty());
        assert_eq!(o.trimmed["count"], 2);
    }

    #[test]
    fn project_parents_missing_source_is_unknown() {
        let env = json!({
            "parents": [ { "id": "p3", "kind": "root", "name": "/", "document_id": null } ]
        });
        let o = super::project_parents(env);
        assert!(o
            .summary
            .starts_with("1 ancestor(s), root last — source: (unknown)"));
        assert_eq!(o.trimmed["source"], Value::Null);
    }

    #[test]
    fn project_document_summary_counts_chunks_and_tokens() {
        let env = json!({
            "id": "d1", "source_path": "docs/intro.md",
            "source": { "display_name": "Compact Docs" },
            "chunks": [
                { "id": "a", "chunk_index": 0, "token_count": 10 },
                { "id": "b", "chunk_index": 1, "token_count": 20 },
                { "id": "c", "chunk_index": 2, "token_count": 30 }
            ]
        });
        let o = super::project_document(env);
        assert!(o.summary.contains("docs/intro.md"));
        assert!(o.summary.contains("3 chunks"));
        assert!(o.summary.contains("~60 tokens"));
        assert_eq!(o.trimmed["chunk_count"], 3);
        assert_eq!(o.trimmed["total_tokens"], 60);
        // Skeleton fits under the fence cap → carried whole in the fence.
        assert_eq!(o.trimmed["chunks"].as_array().unwrap().len(), 3);
        let action = &o.suggested_next_actions[0];
        assert_eq!(action.tool, Some("get_document_chunks"));
        let args = action.arguments.as_ref().unwrap();
        assert_eq!(args["id"], "d1");
        assert_eq!(args["from"], 0);
    }

    #[test]
    fn project_document_fence_caps_skeleton_structured_keeps_all() {
        let skeleton: Vec<_> = (0..60)
            .map(|i| json!({ "id": format!("c{i}"), "chunk_index": i, "token_count": 5 }))
            .collect();
        let env = json!({
            "id": "d1", "source_path": "docs/big.md",
            "source": { "slug": "compact-docs" },
            "chunks": skeleton
        });
        let o = super::project_document(env);
        // display_name absent → slug fallback.
        assert!(o.summary.contains("compact-docs"));
        assert_eq!(o.trimmed["chunks"].as_array().unwrap().len(), 50);
        assert_eq!(o.structured["chunks"].as_array().unwrap().len(), 60);
        assert_eq!(o.trimmed["chunk_count"], 60);
        assert_eq!(o.trimmed["total_tokens"], 300);
    }

    #[test]
    fn project_document_window_range() {
        let long_body = "x".repeat(400);
        let env = json!({
            "id": "d1", "source_path": "docs/intro.md", "source": {"display_name":"X"},
            "from": 3, "limit": 7, "total_chunks": 35,
            "chunks": [
                {"chunk_id":"a", "chunk_index": 3, "content": long_body,
                 "heading_path": ["Intro"], "token_count": 100},
                {"chunk_id":"b", "chunk_index": 4, "content": "short body",
                 "heading_path": ["Intro"], "token_count": 4}
            ]
        });
        let o = super::project_document_window(env);
        assert!(o.summary.contains("3..5")); // from=3, +2 returned
        assert_eq!(o.trimmed["total_chunks"], 35);
        // Fence carries per-chunk briefs: chunk_id / chunk_index / snippet.
        let briefs = o.trimmed["chunks"].as_array().unwrap();
        assert_eq!(briefs.len(), 2);
        assert_eq!(briefs[0]["chunk_id"], "a");
        assert_eq!(briefs[0]["chunk_index"], 3);
        let snip = briefs[0]["snippet"].as_str().unwrap();
        assert!(snip.chars().count() < 400, "fence carries a snippet, not the full body");
        assert_eq!(briefs[1]["snippet"], "short body");
        // Full bodies stay in structuredContent.
        assert_eq!(o.structured["chunks"][0]["content"].as_str().unwrap().len(), 400);
        // to=5 < total=35 → next-window action AND the overview backlink.
        assert_eq!(o.suggested_next_actions.len(), 2);
        let next = &o.suggested_next_actions[0];
        assert_eq!(next.tool, Some("get_document_chunks"));
        assert_eq!(next.arguments.as_ref().unwrap()["from"], 5);
        let back = &o.suggested_next_actions[1];
        assert_eq!(back.tool, Some("get_document"));
        assert_eq!(back.arguments.as_ref().unwrap()["id"], "d1");
    }

    #[test]
    fn project_chunk_list_empty_has_no_next_action() {
        let o = super::project_chunk_list(json!({ "chunks": [] }), "after");
        assert!(o.summary.contains("No more chunks"));
        assert!(o.suggested_next_actions.is_empty());
        assert_eq!(o.trimmed["count"], 0);
        assert_eq!(o.trimmed["chunks"], json!([]));
    }

    #[test]
    fn project_chunk_list_pages_in_direction() {
        let env = json!({ "chunks": [ { "id": "a" }, { "id": "b" } ] });
        let after = super::project_chunk_list(env.clone(), "after");
        assert_eq!(after.suggested_next_actions[0].tool, Some("get_chunk_next"));
        assert_eq!(after.suggested_next_actions[0].arguments.as_ref().unwrap()["id"], "b"); // last
        let before = super::project_chunk_list(env, "before");
        assert_eq!(before.suggested_next_actions[0].tool, Some("get_chunk_prev"));
        assert_eq!(before.suggested_next_actions[0].arguments.as_ref().unwrap()["id"], "a");
        // first
    }

    #[test]
    fn project_document_window_only_backlink_at_end() {
        // from=33, 2 returned -> to=35 == total -> no next-window action,
        // but the overview backlink is always present.
        let env = json!({ "id":"d1","source_path":"x","source":{"display_name":"X"},
            "from":33,"limit":7,"total_chunks":35,"chunks":[{"chunk_id":"a"},{"chunk_id":"b"}] });
        let o = super::project_document_window(env);
        assert_eq!(o.suggested_next_actions.len(), 1);
        assert_eq!(o.suggested_next_actions[0].tool, Some("get_document"));
        assert_eq!(o.suggested_next_actions[0].arguments.as_ref().unwrap()["id"], "d1");
    }

    #[test]
    fn project_sources_paged_summary_cursor_and_filter_example() {
        let env = json!({
            "sources": [
                { "id": "u1", "slug": "compact-docs", "display_name": "Compact Docs",
                  "kind": "docs_site", "origin_url": "https://x", "retention_count": 3,
                  "created_at": "2026-01-01T00:00:00Z", "retired_at": null },
                { "id": "u2", "slug": "midnight-js", "display_name": "Midnight JS",
                  "kind": "code_repo", "origin_url": "https://y", "retention_count": 3,
                  "created_at": "2026-01-02T00:00:00Z", "retired_at": null }
            ],
            "total": 43,
            "next_cursor": "tok=="
        });
        let o = super::project_sources(env.clone());
        assert_eq!(o.summary, "Showing 2 of 43 sources. More available — pass cursor.");
        // structured = env verbatim (already an object; no wrapping)
        assert_eq!(o.structured, env);
        assert_eq!(o.trimmed["count"], 2);
        assert_eq!(o.trimmed["total"], 43);
        assert_eq!(o.trimmed["sources"][0]["slug"], "compact-docs");
        assert!(o.trimmed["sources"][0].get("origin_url").is_none()); // brief view only
                                                                      // next-page action carries the cursor verbatim
        let next = &o.suggested_next_actions[0];
        assert_eq!(next.tool, Some("list_sources"));
        assert_eq!(next.arguments.as_ref().unwrap()["cursor"], "tok==");
        // advanced_search example uses the REAL first slug in description + filters
        let example = &o.suggested_next_actions[1];
        assert_eq!(example.tool, Some("advanced_search"));
        assert!(example.description.contains("compact-docs"));
        let args = example.arguments.as_ref().unwrap();
        assert_eq!(args["filters"]["source_slug"]["any_of"], json!(["compact-docs"]));
    }

    #[test]
    fn project_sources_last_page_has_no_next_page_action() {
        let env = json!({
            "sources": [{ "id": "u1", "slug": "compact-docs", "display_name": "Compact Docs",
                          "kind": "docs_site" }],
            "total": 1,
            "next_cursor": null
        });
        let o = super::project_sources(env);
        assert_eq!(o.summary, "Showing 1 of 1 sources.");
        assert!(!o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("list_sources")));
        // the concrete filter example is still offered
        assert!(o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("advanced_search")));
    }

    #[test]
    fn project_sources_empty_page_has_no_example_action() {
        let env = json!({ "sources": [], "total": 0, "next_cursor": null });
        let o = super::project_sources(env);
        assert_eq!(o.summary, "Showing 0 of 0 sources.");
        assert!(o.suggested_next_actions.is_empty());
    }

    #[test]
    fn project_facets_overview_names_dimensions_with_concrete_example() {
        let env = json!({ "modes": ["hybrid", "vector", "fts"], "filters": [
            { "key": "kind", "type": "closed_set", "negatable": true },
            { "key": "source_slug", "type": "open_set", "negatable": true,
              "values": ["compact-docs", "midnight-js"], "truncated": true, "total": 43 },
            { "key": "tags", "type": "open_set", "negatable": true,
              "values": ["zk"], "truncated": true, "total": 312 }
        ]});
        let o = super::project_facets(env.clone());
        assert!(o.summary.contains("3 filter dimensions"));
        assert!(o.summary.contains("kind, source_slug, tags"));
        assert!(o.summary.contains("drill in with facets({facet})"));
        // structured = env verbatim
        assert_eq!(o.structured, env);
        // trimmed keeps key/type/values/total per dimension (negatable/truncated dropped)
        assert_eq!(o.trimmed["dimensions"][1]["key"], "source_slug");
        assert_eq!(o.trimmed["dimensions"][1]["total"], 43);
        assert_eq!(o.trimmed["dimensions"][1]["values"], json!(["compact-docs", "midnight-js"]));
        assert!(o.trimmed["dimensions"][1].get("negatable").is_none());
        // example action uses the REAL first value of the first dimension that
        // has values (kind has none → source_slug wins)
        let example = &o.suggested_next_actions[0];
        assert_eq!(example.tool, Some("advanced_search"));
        assert!(example.description.contains("source_slug"));
        assert!(example.description.contains("compact-docs"));
        let args = example.arguments.as_ref().unwrap();
        assert_eq!(args["filters"]["source_slug"]["any_of"], json!(["compact-docs"]));
        // drill-down suggestion present
        let drill = &o.suggested_next_actions[1];
        assert_eq!(drill.tool, Some("facets"));
        assert_eq!(drill.arguments.as_ref().unwrap()["facet"], "tags");
    }

    #[test]
    fn project_facets_overview_without_values_still_suggests_drill() {
        let env = json!({ "modes": ["hybrid"], "filters": [
            { "key": "kind", "type": "closed_set", "negatable": true }
        ]});
        let o = super::project_facets(env);
        // no dimension carries values → no concrete filter example
        assert!(!o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("advanced_search")));
        assert!(o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("facets")));
    }

    #[test]
    fn project_facets_drilldown_pages_with_cursor_and_filter_example() {
        let env = json!({
            "facet": "tags",
            "values": ["zk", "proofs"],
            "total": 312,
            "next_cursor": "tok=="
        });
        let o = super::project_facets(env.clone());
        assert_eq!(o.summary, "tags: showing 2 of 312 values.");
        assert_eq!(o.structured, env);
        assert_eq!(o.trimmed, json!({ "facet": "tags", "values": ["zk", "proofs"], "total": 312 }));
        // next-page action carries facet + cursor verbatim
        let next = &o.suggested_next_actions[0];
        assert_eq!(next.tool, Some("facets"));
        let args = next.arguments.as_ref().unwrap();
        assert_eq!(args["facet"], "tags");
        assert_eq!(args["cursor"], "tok==");
        // filter example uses values[0]
        let example = &o.suggested_next_actions[1];
        assert_eq!(example.tool, Some("advanced_search"));
        assert!(example.description.contains("tags"));
        assert!(example.description.contains("zk"));
        let args = example.arguments.as_ref().unwrap();
        assert_eq!(args["filters"]["tags"]["any_of"], json!(["zk"]));
    }

    #[test]
    fn project_facets_drilldown_last_page_has_no_next_page_action() {
        let env = json!({
            "facet": "language",
            "values": ["compact"],
            "total": 1,
            "next_cursor": null
        });
        let o = super::project_facets(env);
        assert_eq!(o.summary, "language: showing 1 of 1 values.");
        assert!(!o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("facets")));
        // the concrete filter example is still offered
        assert!(o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("advanced_search")));
    }

    #[test]
    fn project_facets_drilldown_empty_values_has_no_example_action() {
        let env = json!({ "facet": "package", "values": [], "total": 0, "next_cursor": null });
        let o = super::project_facets(env);
        assert_eq!(o.summary, "package: showing 0 of 0 values.");
        assert!(o.suggested_next_actions.is_empty());
    }

    /// A fully-populated StatusReport env (authenticated, both limit systems,
    /// valid Voyage key).
    fn full_status_env() -> Value {
        json!({
            "mcp_version": "0.1.0",
            "cloud": "reachable",
            "cloud_version": "0.4.2",
            "authenticated": true,
            "auth_type": "read_uplift",
            "identity": "octocat",
            "permission_level": "write",
            "rate_limit": { "tier": "read_uplift", "limit": 120, "remaining": 87,
                            "reset_secs": 31 },
            "token_limits": {
                "tier": "read_uplift",
                "hourly": { "limit": 200_000, "remaining": 150_000, "reset_at_secs": 1_200 },
                "daily": { "limit": 2_000_000, "remaining": 1_900_000, "reset_at_secs": 50_000 }
            },
            "voyage": "valid",
            "reranker": "rerank-2.5",
            "reranker_loaded": false
        })
    }

    #[test]
    fn project_status_full_env_summarizes_every_section() {
        let o = super::project_status(full_status_env());
        assert!(o.summary.contains("Cloud reachable"), "cloud state: {}", o.summary);
        assert!(o.summary.contains("(v0.4.2)"), "cloud version: {}", o.summary);
        assert!(o.summary.contains("read_uplift octocat (write)"), "identity: {}", o.summary);
        assert!(o.summary.contains("requests 87/120"), "rate limit: {}", o.summary);
        assert!(
            o.summary
                .contains("embed tokens 150000/200000 hr · 1900000/2000000 day"),
            "token limits: {}",
            o.summary
        );
        assert!(o.summary.contains("Voyage key valid"), "voyage: {}", o.summary);
        assert!(o.summary.contains("rerank-2.5 not loaded"), "reranker: {}", o.summary);
        // Authenticated + valid key → nothing to suggest.
        assert!(o.suggested_next_actions.is_empty());
    }

    #[test]
    fn project_status_invalid_key_suggests_user_action() {
        let mut env = full_status_env();
        env["voyage"] = json!("invalid_key");
        let o = super::project_status(env);
        assert!(o.summary.contains("Voyage key invalid key"), "summary: {}", o.summary);
        assert!(
            o.suggested_next_actions
                .iter()
                .any(|a| a.tool.is_none() && a.description.contains("VOYAGE_API_KEY")),
            "invalid_key must surface a user action about the key"
        );
    }

    #[test]
    fn project_status_unauthenticated_suggests_auth_github() {
        let mut env = full_status_env();
        env["authenticated"] = json!(false);
        let o = super::project_status(env);
        assert!(o.summary.contains("anonymous (read)"), "summary: {}", o.summary);
        assert!(
            o.suggested_next_actions
                .iter()
                .any(|a| a.tool.is_none() && a.description.contains("mnm auth github")),
            "unauthenticated must surface the auth-github user action"
        );
    }

    #[test]
    fn project_status_trimmed_carries_all_six_keys() {
        let o = super::project_status(full_status_env());
        let t = o.trimmed.as_object().expect("trimmed is an object");
        for key in [
            "cloud",
            "authenticated",
            "auth_type",
            "voyage",
            "rate_limit",
            "token_limits",
        ] {
            assert!(t.contains_key(key), "trimmed must carry `{key}`");
        }
    }

    /// An InstallReport env with two installed harnesses and one undetected.
    fn install_env() -> Value {
        json!({
            "skill_name": "midnight-advanced-search", "scope": "user",
            "installed": [
                { "harness": "claude-code", "scope": "user",
                  "path": "/home/u/.claude/skills/midnight-advanced-search/SKILL.md",
                  "action": "created", "reload_step": "restart Claude Code or run /skills reload" },
                { "harness": "cursor", "scope": "user",
                  "path": "/home/u/.cursor/skills/midnight-advanced-search/SKILL.md",
                  "action": "updated", "reload_step": "restart Cursor" }
            ],
            "detected": ["claude-code", "cursor"],
            "not_detected": ["codex", "opencode"]
        })
    }

    #[test]
    fn project_install_summary_names_harnesses_and_refresh_instruction() {
        let o = super::project_install(install_env());
        assert!(o.summary.contains("`midnight-advanced-search`"), "summary: {}", o.summary);
        assert!(o.summary.contains("claude-code, cursor"), "summary: {}", o.summary);
        assert!(o.summary.contains("(scope: user)"), "summary: {}", o.summary);
        assert!(
            o.summary.contains(
                "NOT active yet — ask the user to restart their session or refresh their skills"
            ),
            "summary must carry the refresh instruction: {}",
            o.summary
        );
    }

    #[test]
    fn project_install_emits_one_user_action_per_harness_with_reload_step() {
        let o = super::project_install(install_env());
        assert_eq!(o.suggested_next_actions.len(), 2);
        let a0 = &o.suggested_next_actions[0];
        assert_eq!(a0.tool, None, "reload steps are user actions, not tool calls");
        assert!(a0.arguments.is_none());
        assert_eq!(
            a0.description,
            "[claude-code] Ask the user to: restart Claude Code or run /skills reload",
            "reload_step must be carried verbatim"
        );
        let a1 = &o.suggested_next_actions[1];
        assert_eq!(a1.tool, None);
        assert_eq!(a1.description, "[cursor] Ask the user to: restart Cursor");
    }

    #[test]
    fn project_install_trimmed_carries_detected_not_detected_and_actions() {
        let o = super::project_install(install_env());
        assert_eq!(o.trimmed["detected"], json!(["claude-code", "cursor"]));
        assert_eq!(o.trimmed["not_detected"], json!(["codex", "opencode"]));
        assert_eq!(
            o.trimmed["actions"],
            json!([
                { "harness": "claude-code", "action": "created" },
                { "harness": "cursor", "action": "updated" }
            ])
        );
        // Full report (paths, reload steps) stays in structuredContent.
        assert!(o.trimmed["actions"][0].get("path").is_none());
        assert_eq!(
            o.structured["installed"][0]["path"],
            "/home/u/.claude/skills/midnight-advanced-search/SKILL.md"
        );
    }

    #[test]
    fn project_install_empty_installed_says_no_harnesses_with_no_actions() {
        let env = json!({
            "skill_name": "midnight-advanced-search", "scope": "user",
            "installed": [], "detected": [],
            "not_detected": ["claude-code", "codex", "opencode", "cursor"]
        });
        let o = super::project_install(env);
        assert!(o.summary.contains("for no harnesses"), "summary: {}", o.summary);
        assert!(o.suggested_next_actions.is_empty());
        assert_eq!(o.trimmed["actions"], json!([]));
    }

    #[test]
    fn project_search_telemetry_dedup_uses_overlap_counts() {
        let env = json!({
            "corpus_embedding_model": "voyage-code-3@1",
            "results": [{ "chunk_id": "a", "document_id": "b", "source_path": "p",
                          "source_display_name": "S", "heading_path": [], "content": "c",
                          "scores": { "confidence": 0.9, "confidence_factors": { "attribution": "foundation" } } }],
            "search_metadata": { "filtered_by_confidence": 1, "deduplicated_count": 0,
                                 "overlap_dropped_count": 3, "overlap_trimmed_count": 2 }
        });
        let o = super::project_search(env, &SearchRenderOpts::default());
        let t = o.telemetry.unwrap();
        assert_eq!(t.filtered_by_confidence, Some(1));
        assert_eq!(t.deduplicated_count, Some(5)); // overlap_dropped(3) + overlap_trimmed(2), NOT input-dedup(0)
    }

    #[test]
    fn snippet_truncates_long_ascii_on_char_boundary() {
        let long = "a".repeat(200);
        let s = super::snippet(&long);
        assert_eq!(s.chars().count(), 151); // 150 head chars + '…'
        assert!(s.ends_with('…'));
    }

    #[test]
    fn snippet_leaves_short_content_unchanged() {
        assert_eq!(super::snippet("ten chars."), "ten chars.");
    }

    #[test]
    fn snippet_multibyte_counts_chars_not_bytes() {
        let long: String = "é".repeat(200); // 2 bytes per char — byte-index slicing would panic
        let s = super::snippet(&long);
        assert_eq!(s.chars().count(), 151);
        assert!(s.ends_with('…'));
        assert!(s.starts_with("ééé"));
    }

    #[test]
    fn chunk_brief_extracts_fields_from_chunk_with_context() {
        let c = json!({
            "id": "c1", "content": "body text", "chunk_index": 4,
            "heading_path": ["A", "B"],
            "document": { "source_path": "docs/intro.md" },
            "source": { "display_name": "Compact Docs" }
        });
        let b = super::chunk_brief(&c);
        assert_eq!(b["id"], "c1");
        assert_eq!(b["source_path"], "docs/intro.md");
        assert_eq!(b["heading_path"], json!(["A", "B"]));
        assert_eq!(b["snippet"], "body text");
    }

    #[test]
    fn chunk_brief_yields_nulls_for_absent_fields() {
        let b = super::chunk_brief(&json!({}));
        assert_eq!(b["id"], Value::Null);
        assert_eq!(b["source_path"], Value::Null);
        assert_eq!(b["heading_path"], json!([]));
        assert_eq!(b["snippet"], Value::Null);
    }

    #[test]
    fn project_neighbors_fence_carries_anchor_and_side_briefs() {
        let env = json!({
            "prev": { "chunks": [
                { "id": "p1", "content": "prev body", "document": { "source_path": "docs/x.md" } }
            ] },
            "chunk": { "id": "c1", "document_id": "d1", "content": "anchor body",
                       "heading_path": ["H"], "document": { "source_path": "docs/x.md" } },
            "next": { "chunks": [
                { "id": "n1", "content": "next body", "document": { "source_path": "docs/x.md" } },
                { "id": "n2", "content": "next body 2", "document": { "source_path": "docs/x.md" } }
            ] }
        });
        let o = super::project_neighbors(env);
        assert_eq!(o.trimmed["prev"], 1);
        assert_eq!(o.trimmed["next"], 2);
        assert_eq!(o.trimmed["chunks"]["anchor"]["id"], "c1");
        assert_eq!(o.trimmed["chunks"]["anchor"]["snippet"], "anchor body");
        assert_eq!(o.trimmed["chunks"]["prev"][0]["snippet"], "prev body");
        assert_eq!(o.trimmed["chunks"]["next"][1]["snippet"], "next body 2");
    }

    #[test]
    fn user_action_serializes_description_only() {
        let a = NextAction::user("Ask the user to restart the harness");
        let v = a.to_value();
        assert_eq!(v["description"], "Ask the user to restart the harness");
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("tool"), "user action must not carry a tool key");
        assert!(!obj.contains_key("arguments"), "user action must not carry an arguments key");
    }

    #[test]
    fn every_serialized_action_has_nonempty_description() {
        let actions = vec![
            NextAction::call(
                "Fetch the chunk's full content",
                "get_chunks",
                json!({ "ids": ["a"] }),
            ),
            NextAction::user("Ask the user to restart the harness"),
        ];
        let v = suggested_next_actions_value(&actions);
        for entry in v.as_array().unwrap() {
            let d = entry["description"].as_str().unwrap();
            assert!(!d.is_empty(), "every serialized action must carry a non-empty description");
        }
    }
}

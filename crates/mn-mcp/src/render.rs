//! Shapes every tool result into the MCP "summary + structuredContent" form.
//!
//! Success → one `text` content block (`summary` + the trimmed JSON in a fenced
//! ```json block) plus full-fidelity `structuredContent` (with
//! `suggested_next_actions`). Failure → an `isError: true` result carrying a
//! shared error envelope.

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

    ToolOutcome {
        summary,
        structured: envelope,
        trimmed: json!({ "results": trimmed_results, "match_count": result_count }),
        suggested_next_actions,
        telemetry: Some(telemetry),
    }
}

/// `get_chunk`: single chunk-with-context. Chunk fields are top-level (flattened);
/// `document`/`source` are nested summary objects.
pub fn project_chunk(env: Value) -> ToolOutcome {
    let id = env
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned();
    let path = env
        .pointer("/document/source_path")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let heading = env
        .get("heading_path")
        .and_then(Value::as_array)
        .map(|h| {
            h.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" › ")
        })
        .filter(|s| !s.is_empty());
    let idx = env.get("chunk_index").and_then(Value::as_i64);
    let total = env.get("total_chunks").and_then(Value::as_i64);
    let where_ = heading.map_or_else(|| path.to_owned(), |h| format!("{path} › {h}"));
    let pos = match (idx, total) {
        (Some(i), Some(t)) => format!(" (idx {i}/{t})"),
        _ => String::new(),
    };
    let summary = format!("Chunk {id} — {where_}{pos}.");
    let trimmed = json!({ "chunk_id": id, "source_path": path });
    let suggested_next_actions = vec![
        NextAction::call(
            "Read the next chunk in this document",
            "get_chunk_next",
            json!({ "id": id }),
        ),
        NextAction::call(
            "Read the previous chunk in this document",
            "get_chunk_prev",
            json!({ "id": id }),
        ),
        NextAction::call(
            "Fetch the chunks immediately surrounding this one for more context",
            "get_chunk_neighbors",
            json!({ "id": id }),
        ),
        NextAction::call(
            "List this chunk's ancestor sections and parent document",
            "get_chunk_parents",
            json!({ "id": id }),
        ),
    ];
    ToolOutcome::new(summary, env, trimmed, suggested_next_actions)
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

/// `get_chunk_parents`: a top-level JSON ARRAY of ancestor nodes. Wrap as an object so
/// `structured` stays an object (for `suggested_next_actions` injection + outputSchema).
// `env` is consumed by the `json!` macro (ownership move); a reference would require an
// extra clone, so suppress the false-positive lint.
#[allow(clippy::needless_pass_by_value)]
pub fn project_parents(env: Value) -> ToolOutcome {
    let names: Vec<String> = env
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|node| node.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let n = names.len();
    let summary = format!("{n} ancestor node(s): {}.", names.join(" / "));
    let trimmed = json!({ "count": n, "names": names });
    let structured = json!({ "parents": env });
    ToolOutcome::new(summary, structured, trimmed, vec![])
}

/// `get_document` (DocumentOverview): Document flattened to top level; `source` nested; `chunks` skeleton array.
pub fn project_document_overview(env: Value) -> ToolOutcome {
    let path = env
        .get("source_path")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let name = env
        .pointer("/source/display_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let id = env
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned();
    let n = env
        .get("chunks")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let summary = format!("{path} ({name}): {n} chunks.");
    let trimmed = json!({ "source_path": path, "chunk_count": n });
    let suggested_next_actions = vec![NextAction::call(
        "Fetch this document's chunks with full content",
        "get_document_chunks",
        json!({ "id": id }),
    )];
    ToolOutcome::new(summary, env, trimmed, suggested_next_actions)
}

/// `get_document_chunks` (DocumentChunkWindow): Document flattened; window meta top-level.
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
    let trimmed = json!({ "source_path": path, "from": from, "to": to, "total_chunks": total });
    let suggested_next_actions = if to < total {
        vec![NextAction::call(
            "Fetch the next window of chunks in this document",
            "get_document_chunks",
            json!({ "id": id, "from": to }),
        )]
    } else {
        vec![]
    };
    ToolOutcome::new(summary, env, trimmed, suggested_next_actions)
}

/// `list_sources`: a top-level JSON ARRAY of source objects. Wrap as an object for structured.
// `env` is consumed by `json!({ "sources": env })` (ownership move); a reference would require
// an extra clone, so suppress the false-positive lint — same pattern as `project_parents`.
#[allow(clippy::needless_pass_by_value)]
pub fn project_sources(env: Value) -> ToolOutcome {
    let names: Vec<String> = env
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    s.get("display_name")
                        .and_then(Value::as_str)
                        .or_else(|| s.get("slug").and_then(Value::as_str))
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    let n = names.len();
    let summary = format!("{n} sources: {}.", names.join(", "));
    let trimmed = json!({ "count": n, "sources": names });
    let structured = json!({ "sources": env });
    ToolOutcome::new(
        summary,
        structured,
        trimmed,
        vec![NextAction::call(
            "Search the corpus, optionally filtered to one of these sources",
            "search",
            json!({ "query": "<terms>" }),
        )],
    )
}

/// `facets`: `{ modes:[..], filters:[ {key, type, ..}, .. ] }`. Dimensions are `filters[].key`.
pub fn project_facets(env: Value) -> ToolOutcome {
    let keys: Vec<String> = env
        .get("filters")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|f| f.get("key").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let summary = format!("{} facet dimension(s): {}.", keys.len(), keys.join(", "));
    let trimmed = json!({ "dimensions": keys });
    ToolOutcome::new(
        summary,
        env,
        trimmed,
        vec![NextAction::call(
            "Search the corpus using these facet keys as filters",
            "advanced_search",
            json!({ "queries": ["<terms>"], "filters": {} }),
        )],
    )
}

/// `status` (StatusOutput as JSON).
pub fn project_status(env: Value) -> ToolOutcome {
    let reranker = env.get("reranker").and_then(Value::as_str).unwrap_or("?");
    let state = env
        .get("model_state")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let ver = env
        .get("server_version")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let summary = format!("Server {ver}; reranker {reranker}; model state {state}.");
    let next = if state == "ready" {
        vec![]
    } else {
        vec![NextAction::call(
            "Download the missing local models so reranking can run",
            "pull_models",
            json!({}),
        )]
    };
    let trimmed = json!({ "server_version": ver, "reranker": reranker, "model_state": state });
    ToolOutcome::new(summary, env, trimmed, next)
}

/// `pull_models` (PullModelsOutput as JSON).
pub fn project_pull_models(env: Value) -> ToolOutcome {
    let reranker = env.get("reranker").and_then(Value::as_str).unwrap_or("?");
    let loaded = env
        .get("reranker_loaded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let summary = format!(
        "Models pulled. Reranker {reranker} {}.",
        if loaded { "ready" } else { "not loaded" }
    );
    let trimmed = json!({ "reranker": reranker, "reranker_loaded": loaded });
    ToolOutcome::new(
        summary,
        env,
        trimmed,
        vec![NextAction::call(
            "Confirm the models are now loaded and ready",
            "status",
            json!({}),
        )],
    )
}

/// `install_search_skill` (InstallReport as JSON).
pub fn project_install(env: Value) -> ToolOutcome {
    let scope = env.get("scope").and_then(Value::as_str).unwrap_or("user");
    let skill = env
        .get("skill_name")
        .and_then(Value::as_str)
        .unwrap_or("search");
    let installed = env
        .get("installed")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let summary =
        format!("Installed `{skill}` skill for {installed} harness(es) (scope: {scope}).");
    let trimmed = json!({ "skill_name": skill, "scope": scope, "installed_count": installed });
    ToolOutcome::new(summary, env, trimmed, vec![])
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
            Self::CloudError => "CLOUD_ERROR",
            Self::ModelLoadFailed => "MODEL_LOAD_FAILED",
            Self::InstallFailed => "INSTALL_FAILED",
        }
    }
    const fn retryable(self) -> bool {
        match self {
            // Permanent for an identical retry — recovery requires a suggested action.
            Self::NotFound | Self::EmbeddingModelMismatch => false,
            // Transient or fixable-and-retry.
            Self::InvalidInput | Self::CloudError | Self::ModelLoadFailed | Self::InstallFailed => {
                true
            }
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
            reranker_used: Some("bge-reranker-base".to_owned()),
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
        assert_eq!(t.reranker_used.as_deref(), Some("bge-reranker-base"));
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
                "get_chunk",
                json!({ "id": "abc" }),
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
        assert_eq!(sc["suggested_next_actions"][0]["tool"], "get_chunk");
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
    fn project_chunk_summary() {
        let env = json!({
            "id": "c1", "chunk_index": 4, "total_chunks": 35, "content": "body",
            "heading_path": ["A", "B"],
            "document": { "source_path": "docs/intro.md" },
            "source": { "display_name": "Compact Docs" }
        });
        let o = super::project_chunk(env);
        assert!(o.summary.contains("docs/intro.md"));
        assert!(o.summary.contains('4')); // chunk index
        assert_eq!(
            o.suggested_next_actions
                .iter()
                .filter(|a| a.tool == Some("get_chunk_next"))
                .count(),
            1
        );
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
    fn project_parents_wraps_array_as_object() {
        let env = json!([ { "name": "Group A" }, { "name": "Doc B" } ]);
        let o = super::project_parents(env);
        assert!(o.summary.contains("Group A"));
        // structured must be an OBJECT (array wrapped) so suggested_next_actions injection + outputSchema hold
        assert!(o.structured.is_object());
        assert!(o.structured["parents"].is_array());
    }

    #[test]
    fn project_document_overview_summary() {
        let env = json!({
            "id": "d1", "source_path": "docs/intro.md",
            "source": { "display_name": "Compact Docs" },
            "chunks": [
                { "id": "a", "chunk_index": 0, "token_count": 10 },
                { "id": "b", "chunk_index": 1, "token_count": 20 },
                { "id": "c", "chunk_index": 2, "token_count": 30 }
            ]
        });
        let o = super::project_document_overview(env);
        assert!(o.summary.contains("docs/intro.md"));
        assert!(o.summary.contains('3'));
        assert!(o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("get_document_chunks")));
    }

    #[test]
    fn project_document_window_range() {
        let env = json!({
            "id": "d1", "source_path": "docs/intro.md", "source": {"display_name":"X"},
            "from": 3, "limit": 7, "total_chunks": 35,
            "chunks": [ {"chunk_id":"a"}, {"chunk_id":"b"} ]
        });
        let o = super::project_document_window(env);
        assert!(o.summary.contains("3..5")); // from=3, +2 returned
        assert_eq!(o.trimmed["total_chunks"], 35);
        // to=5 < total=35 → should still have a next action
        assert!(!o.suggested_next_actions.is_empty());
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
    fn project_document_window_no_action_at_end() {
        // from=33, 2 returned -> to=35 == total -> no next action
        let env = json!({ "id":"d1","source_path":"x","source":{"display_name":"X"},
            "from":33,"limit":7,"total_chunks":35,"chunks":[{"chunk_id":"a"},{"chunk_id":"b"}] });
        let o = super::project_document_window(env);
        assert!(o.suggested_next_actions.is_empty());
    }

    #[test]
    fn project_sources_lists_names() {
        let env = json!([
            { "slug": "compact-docs", "display_name": "Compact Docs" },
            { "slug": "midnight-js", "display_name": "Midnight JS" }
        ]);
        let o = super::project_sources(env);
        assert!(o.summary.contains("2 sources"));
        assert!(o.summary.contains("Compact Docs"));
        assert!(o.structured.is_object()); // array wrapped
        assert!(o.structured["sources"].is_array());
        assert!(o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("search")));
    }

    #[test]
    fn project_facets_lists_dimensions() {
        let env = json!({ "modes": ["hybrid"], "filters": [
            { "key": "language", "type": "open_set" },
            { "key": "source", "type": "open_set" }
        ]});
        let o = super::project_facets(env);
        assert!(o.summary.contains("language"));
        assert!(o.summary.contains("source"));
        assert_eq!(o.trimmed["dimensions"], json!(["language", "source"]));
    }

    #[test]
    fn project_status_reports_state() {
        let env = json!({ "server_version": "0.1.0", "reranker": "bge-reranker-base",
                          "model_state": "missing", "cache_dir": null });
        let o = super::project_status(env);
        assert!(o.summary.to_lowercase().contains("reranker"));
        assert!(o
            .suggested_next_actions
            .iter()
            .any(|a| a.tool == Some("pull_models"))); // state != ready
    }

    #[test]
    fn project_install_summary() {
        let env = json!({ "skill_name": "search", "scope": "user",
                          "installed": [ {"harness":"claude-code"} ], "not_detected": [] });
        let o = super::project_install(env);
        assert!(o.summary.contains('1')); // 1 harness
        assert!(o.summary.contains("user")); // scope
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
            NextAction::call("Fetch the chunk's full content", "get_chunk", json!({ "id": "a" })),
            NextAction::user("Ask the user to restart the harness"),
        ];
        let v = suggested_next_actions_value(&actions);
        for entry in v.as_array().unwrap() {
            let d = entry["description"].as_str().unwrap();
            assert!(!d.is_empty(), "every serialized action must carry a non-empty description");
        }
    }
}

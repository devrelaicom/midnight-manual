//! The new result contract: success = one text block (summary + fenced json) +
//! structuredContent conforming to the tool's outputSchema; failure = isError.
//!
//! Every projector in `mn_mcp::render` has a conformance case here validating
//! a realistic envelope against the outputSchema advertised in `tools::list()`.

use jsonschema::JSONSchema;
use serde_json::Value;

fn assert_conforms(label: &str, sc: &Value, schema: &Value) {
    assert!(sc.is_object(), "[{label}] structuredContent must be an object: {sc}");
    let compiled = JSONSchema::compile(schema).expect("schema compiles");
    assert!(
        compiled.is_valid(sc),
        "[{label}] structuredContent {sc} must conform to schema {schema}"
    );
}

/// A ChunkWithContext-shaped envelope entry (`get_chunks` / nav tools).
fn chunk_env(id: &str) -> Value {
    serde_json::json!({
        "id": id, "chunk_index": 0, "total_chunks": 3, "content": "x",
        "heading_path": ["Intro"], "document_id": "d1",
        "document": { "source_path": "docs/x.md" }, "source": { "display_name": "S" }
    })
}

#[test]
fn search_structured_conforms_to_output_schema() {
    let env = serde_json::json!({
        "corpus_embedding_model": "voyage-code-3@1",
        "results": [{ "chunk_id": "a", "document_id": "b", "source_path": "docs/x.md",
                      "source_slug": "s", "source_display_name": "S", "heading_path": [],
                      "symbol_path": [], "content": "c",
                      "scores": { "confidence": 0.9, "trust_score": 1.0,
                                  "confidence_factors": { "attribution": "foundation", "verified": true } } }],
        "search_metadata": { "filtered_by_confidence": 0, "deduplicated_count": 0,
                             "total_candidates": 12 }
    });
    let result = mn_mcp::render::project_search(env, &mn_mcp::render::SearchRenderOpts::default())
        .into_result();
    let sc = result
        .structured_content
        .as_ref()
        .expect("structuredContent present");

    assert_conforms("search (basic)", sc, &mn_mcp::schemas::search_output_schema());

    // text block = summary + fenced json, not an isError
    let text = match &result.content[0] {
        mn_mcp::protocol::ContentBlock::Text { text } => text,
    };
    assert!(text.contains("```json"), "text block must embed a fenced json view");
    assert!(!result.is_error);
}

#[test]
fn advanced_search_structured_conforms_to_output_schema() {
    // advanced_search shares search's outputSchema but keeps matched_queries.
    let env = serde_json::json!({
        "corpus_embedding_model": "voyage-code-3@1",
        "results": [{ "chunk_id": "a", "document_id": "b", "source_path": "docs/x.md",
                      "source_slug": "s", "source_display_name": "S", "heading_path": ["H"],
                      "symbol_path": [], "content": "c",
                      "scores": { "confidence": 0.7, "trust_score": 0.8,
                                  "matched_queries": [0, 1],
                                  "confidence_factors": { "attribution": "community", "verified": false } } }],
        "search_metadata": { "filtered_by_confidence": 1, "overlap_dropped_count": 2,
                             "total_candidates": 9 }
    });
    let opts = mn_mcp::render::SearchRenderOpts {
        reranker_used: Some("rerank-2.5".to_owned()),
        advanced: true,
        skill_installed: true,
    };
    let result = mn_mcp::render::project_search(env, &opts).into_result();
    let sc = result
        .structured_content
        .as_ref()
        .expect("structuredContent present");

    assert_conforms("advanced_search", sc, &mn_mcp::schemas::search_output_schema());
    assert!(
        sc.pointer("/results/0/scores/matched_queries").is_some(),
        "advanced flavor must keep matched_queries in structuredContent"
    );
    assert!(!result.is_error);
}

#[test]
// One fixture per projector: length is inherent to the data (same rationale
// as the allow on `tools::list()`); splitting would scatter the sweep.
#[allow(clippy::too_many_lines)]
fn all_passthrough_projectors_conform_to_their_output_schema() {
    // A representative success from each passthrough projector validates
    // against the outputSchema advertised for its tool in tools::list().
    let status_report = mn_mcp::status::StatusReport {
        mcp_version: "0.4.0",
        cloud: mn_mcp::status::CloudState::Reachable,
        cloud_version: Some("0.4.2".to_owned()),
        authenticated: true,
        auth_type: "github_oauth".to_owned(),
        identity: Some("octocat".to_owned()),
        permission_level: "write".to_owned(),
        rate_limit: Some(serde_json::json!({ "limit": 120, "remaining": 118 })),
        token_limits: Some(serde_json::json!({
            "tier": "authenticated",
            "hourly": { "limit": 1_000_000_u64, "remaining": 990_000_u64 },
            "daily": { "limit": 10_000_000_u64, "remaining": 9_900_000_u64 }
        })),
        voyage: mn_mcp::status::VoyageState::Valid,
        reranker: "rerank-2.5",
        reranker_loaded: true,
    };
    let status_env = serde_json::to_value(&status_report).expect("StatusReport serializes");

    let cases: Vec<(&str, mn_mcp::render::ToolOutcome, Value)> = vec![
        (
            "get_chunks (single)",
            mn_mcp::render::project_chunks(
                serde_json::json!({ "chunks": [chunk_env("c1")], "missing": [] }),
            ),
            mn_mcp::schemas::chunks_output_schema(),
        ),
        (
            "get_chunks (multi + missing)",
            mn_mcp::render::project_chunks(serde_json::json!({
                "chunks": [chunk_env("c1"), chunk_env("c2")],
                "missing": ["c3"]
            })),
            mn_mcp::schemas::chunks_output_schema(),
        ),
        (
            "get_chunk_next (chunk_list, after)",
            mn_mcp::render::project_chunk_list(
                serde_json::json!({ "chunks": [chunk_env("c2"), chunk_env("c3")] }),
                "after",
            ),
            mn_mcp::schemas::chunk_list_output_schema(),
        ),
        (
            "get_chunk_prev (chunk_list, before)",
            mn_mcp::render::project_chunk_list(
                serde_json::json!({ "chunks": [chunk_env("c0")] }),
                "before",
            ),
            mn_mcp::schemas::chunk_list_output_schema(),
        ),
        (
            "get_chunk_neighbors",
            mn_mcp::render::project_neighbors(serde_json::json!({
                "prev": { "chunks": [chunk_env("c0")] },
                "chunk": chunk_env("c1"),
                "next": { "chunks": [chunk_env("c2")] }
            })),
            mn_mcp::schemas::neighbors_output_schema(),
        ),
        (
            "get_chunk_parents",
            mn_mcp::render::project_parents(serde_json::json!({
                "parents": [
                    { "id": "n1", "kind": "document", "name": "x.md", "document_id": "d1" },
                    { "id": "p1", "kind": "root", "name": "G", "document_id": null }
                ],
                "source": { "slug": "s", "display_name": "S" }
            })),
            mn_mcp::schemas::parents_output_schema(),
        ),
        (
            "get_document (overview + skeleton)",
            mn_mcp::render::project_document(serde_json::json!({
                "id": "d1", "source_path": "docs/x.md", "language": "markdown",
                "source": { "slug": "s", "display_name": "S" },
                "chunks": [
                    { "id": "c1", "chunk_index": 0, "token_count": 120 },
                    { "id": "c2", "chunk_index": 1, "token_count": 80 }
                ]
            })),
            mn_mcp::schemas::document_output_schema(),
        ),
        (
            "get_document_chunks (window)",
            mn_mcp::render::project_document_window(serde_json::json!({
                "id": "d1", "source_path": "docs/x.md", "from": 0, "total_chunks": 5,
                "source": { "slug": "s", "display_name": "S" },
                "chunks": [
                    { "chunk_id": "c1", "chunk_index": 0, "content": "body one" },
                    { "chunk_id": "c2", "chunk_index": 1, "content": "body two" }
                ]
            })),
            mn_mcp::schemas::document_output_schema(),
        ),
        (
            "list_sources (paged)",
            mn_mcp::render::project_sources(serde_json::json!({
                "sources": [{ "id": "s1", "slug": "s", "display_name": "S", "kind": "docs_site" }],
                "total": 43,
                "next_cursor": "tok=="
            })),
            mn_mcp::schemas::sources_output_schema(),
        ),
        (
            "status (StatusReport env)",
            mn_mcp::render::project_status(status_env),
            mn_mcp::schemas::status_output_schema(),
        ),
        (
            "facets (overview)",
            mn_mcp::render::project_facets(serde_json::json!({
                "modes": ["hybrid"],
                "filters": [{ "key": "source_slug", "type": "open_set", "negatable": true,
                              "values": ["compact-docs"], "truncated": true, "total": 43 }]
            })),
            mn_mcp::schemas::facets_output_schema(),
        ),
        (
            "facets (drill-down)",
            mn_mcp::render::project_facets(serde_json::json!({
                "facet": "tags", "values": ["zk"], "total": 312, "next_cursor": "tok=="
            })),
            mn_mcp::schemas::facets_output_schema(),
        ),
        (
            "install_search_skill (with detected)",
            mn_mcp::render::project_install(serde_json::json!({
                "skill_name": "midnight-advanced-search", "scope": "user",
                "installed": [
                    { "harness": "claude-code", "scope": "user",
                      "path": "/home/u/.claude/skills/midnight-advanced-search/SKILL.md",
                      "action": "created",
                      "reload_step": "restart Claude Code or run /skills reload" }
                ],
                "detected": ["claude-code"],
                "not_detected": ["codex", "opencode", "cursor"]
            })),
            mn_mcp::schemas::install_output_schema(),
        ),
    ];
    for (label, outcome, schema) in cases {
        let sc = outcome
            .into_result()
            .structured_content
            .expect("structuredContent present");
        assert_conforms(label, &sc, &schema);
    }
}

#[test]
fn failure_is_iserror_with_error_code() {
    let f = mn_mcp::render::ToolFailure::simple(
        mn_mcp::render::ErrorKind::NotFound,
        "no chunk x",
        "Verify the id from a recent search.",
    );
    let result = f.into_result();
    assert!(result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["error"]["code"], "NOT_FOUND");
}

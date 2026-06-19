//! The new result contract: success = one text block (summary + fenced json) +
//! structuredContent conforming to the tool's outputSchema; failure = isError.
//!
//! Every projector in `mnm_mcp::render` has a conformance case here validating
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
    let result =
        mnm_mcp::render::project_search(env, &mnm_mcp::render::SearchRenderOpts::default())
            .into_result();
    let sc = result
        .structured_content
        .as_ref()
        .expect("structuredContent present");

    assert_conforms("search (basic)", sc, &mnm_mcp::schemas::search_output_schema());

    // text block = summary + fenced json, not an isError
    let text = match &result.content[0] {
        mnm_mcp::protocol::ContentBlock::Text { text } => text,
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
    let opts = mnm_mcp::render::SearchRenderOpts {
        reranker_used: Some("rerank-2.5".to_owned()),
        advanced: true,
        skill_installed: true,
    };
    let result = mnm_mcp::render::project_search(env, &opts).into_result();
    let sc = result
        .structured_content
        .as_ref()
        .expect("structuredContent present");

    assert_conforms("advanced_search", sc, &mnm_mcp::schemas::search_output_schema());
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
    let status_report = mnm_mcp::status::StatusReport {
        mcp_version: "0.4.0",
        cloud: mnm_mcp::status::CloudState::Reachable,
        cloud_version: Some("0.4.2".to_owned()),
        authenticated: true,
        auth_type: "read_uplift".to_owned(),
        identity: Some("octocat".to_owned()),
        permission_level: "write".to_owned(),
        rate_limit: Some(mnm_core::introspect::MeRateLimit {
            tier: "read_uplift".to_owned(),
            limit: 120,
            remaining: 118,
            reset_secs: 7,
        }),
        token_limits: Some(mnm_core::introspect::MeTokenLimits {
            tier: "read_uplift".to_owned(),
            hourly: mnm_core::introspect::MeTokenWindow {
                limit: 1_000_000,
                remaining: 990_000,
                reset_at_secs: 1_200,
            },
            daily: mnm_core::introspect::MeTokenWindow {
                limit: 10_000_000,
                remaining: 9_900_000,
                reset_at_secs: 50_000,
            },
        }),
        voyage: mnm_mcp::status::VoyageState::Valid,
        reranker: "rerank-2.5",
        reranker_loaded: true,
    };
    let status_env = serde_json::to_value(&status_report).expect("StatusReport serializes");

    let cases: Vec<(&str, mnm_mcp::render::ToolOutcome, Value)> = vec![
        (
            "get_chunks (single)",
            mnm_mcp::render::project_chunks(
                serde_json::json!({ "chunks": [chunk_env("c1")], "missing": [] }),
            ),
            mnm_mcp::schemas::chunks_output_schema(),
        ),
        (
            "get_chunks (multi + missing)",
            mnm_mcp::render::project_chunks(serde_json::json!({
                "chunks": [chunk_env("c1"), chunk_env("c2")],
                "missing": ["c3"]
            })),
            mnm_mcp::schemas::chunks_output_schema(),
        ),
        (
            "get_chunk_next (chunk_list, after)",
            mnm_mcp::render::project_chunk_list(
                serde_json::json!({ "chunks": [chunk_env("c2"), chunk_env("c3")] }),
                "after",
            ),
            mnm_mcp::schemas::chunk_list_output_schema(),
        ),
        (
            "get_chunk_prev (chunk_list, before)",
            mnm_mcp::render::project_chunk_list(
                serde_json::json!({ "chunks": [chunk_env("c0")] }),
                "before",
            ),
            mnm_mcp::schemas::chunk_list_output_schema(),
        ),
        (
            "get_chunk_neighbors",
            mnm_mcp::render::project_neighbors(serde_json::json!({
                "prev": { "chunks": [chunk_env("c0")] },
                "chunk": chunk_env("c1"),
                "next": { "chunks": [chunk_env("c2")] }
            })),
            mnm_mcp::schemas::neighbors_output_schema(),
        ),
        (
            "get_chunk_parents",
            mnm_mcp::render::project_parents(serde_json::json!({
                "parents": [
                    { "id": "n1", "kind": "document", "name": "x.md", "document_id": "d1" },
                    { "id": "p1", "kind": "root", "name": "G", "document_id": null }
                ],
                "source": { "slug": "s", "display_name": "S" }
            })),
            mnm_mcp::schemas::parents_output_schema(),
        ),
        (
            "get_document (overview + skeleton)",
            mnm_mcp::render::project_document(serde_json::json!({
                "id": "d1", "source_path": "docs/x.md", "language": "markdown",
                "source": { "slug": "s", "display_name": "S" },
                "chunks": [
                    { "id": "c1", "chunk_index": 0, "token_count": 120 },
                    { "id": "c2", "chunk_index": 1, "token_count": 80 }
                ]
            })),
            mnm_mcp::schemas::document_output_schema(),
        ),
        (
            "get_document_chunks (window)",
            mnm_mcp::render::project_document_window(serde_json::json!({
                "id": "d1", "source_path": "docs/x.md", "from": 0, "total_chunks": 5,
                "source": { "slug": "s", "display_name": "S" },
                "chunks": [
                    { "chunk_id": "c1", "chunk_index": 0, "content": "body one" },
                    { "chunk_id": "c2", "chunk_index": 1, "content": "body two" }
                ]
            })),
            mnm_mcp::schemas::document_window_output_schema(),
        ),
        (
            "list_sources (paged)",
            mnm_mcp::render::project_sources(serde_json::json!({
                "sources": [{ "id": "s1", "slug": "s", "display_name": "S", "kind": "docs_site" }],
                "total": 43,
                "next_cursor": "tok=="
            })),
            mnm_mcp::schemas::sources_output_schema(),
        ),
        (
            "status (StatusReport env)",
            mnm_mcp::render::project_status(status_env),
            mnm_mcp::schemas::status_output_schema(),
        ),
        (
            "facets (overview)",
            mnm_mcp::render::project_facets(serde_json::json!({
                "modes": ["hybrid"],
                "filters": [{ "key": "source_slug", "type": "open_set", "negatable": true,
                              "values": ["compact-docs"], "truncated": true, "total": 43 }]
            })),
            mnm_mcp::schemas::facets_output_schema(),
        ),
        (
            "facets (drill-down)",
            mnm_mcp::render::project_facets(serde_json::json!({
                "facet": "tags", "values": ["zk"], "total": 312, "next_cursor": "tok=="
            })),
            mnm_mcp::schemas::facets_output_schema(),
        ),
        // Terminal / null-state fixtures: the cloud emits these fields as JSON
        // null (present, not absent), so the schemas must admit null.
        (
            "get_document (no language → null)",
            mnm_mcp::render::project_document(serde_json::json!({
                "id": "d1", "source_path": "docs/x.md", "language": null,
                "source": { "slug": "s", "display_name": "S" },
                "chunks": [{ "id": "c1", "chunk_index": 0, "token_count": 120 }]
            })),
            mnm_mcp::schemas::document_output_schema(),
        ),
        (
            "list_sources (last page, next_cursor null)",
            mnm_mcp::render::project_sources(serde_json::json!({
                "sources": [{ "id": "s1", "slug": "s", "display_name": "S", "kind": "docs_site" }],
                "total": 1,
                "next_cursor": null
            })),
            mnm_mcp::schemas::sources_output_schema(),
        ),
        (
            "facets (drill-down last page, next_cursor null)",
            mnm_mcp::render::project_facets(serde_json::json!({
                "facet": "tags", "values": ["zk"], "total": 1, "next_cursor": null
            })),
            mnm_mcp::schemas::facets_output_schema(),
        ),
        (
            "install_search_skill (with detected)",
            mnm_mcp::render::project_install(serde_json::json!({
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
            mnm_mcp::schemas::install_output_schema(),
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
    let f = mnm_mcp::render::ToolFailure::simple(
        mnm_mcp::render::ErrorKind::NotFound,
        "no chunk x",
        "Verify the id from a recent search.",
    );
    let result = f.into_result();
    assert!(result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["error"]["code"], "NOT_FOUND");
}

#[test]
fn failure_structured_conforms_to_error_schema() {
    // A minimal failure and a mismatch failure with extra details both conform
    // to the discoverable errorSchema (issue #89 C2).
    let simple = mnm_mcp::render::ToolFailure::simple(
        mnm_mcp::render::ErrorKind::NotFound,
        "no chunk x",
        "Verify the id from a recent search.",
    );
    let sc = simple
        .into_result()
        .structured_content
        .expect("structuredContent present");
    assert_conforms("error (simple)", &sc, &mnm_mcp::schemas::error_output_schema());

    let mismatch = mnm_mcp::render::ToolFailure {
        kind: mnm_mcp::render::ErrorKind::EmbeddingModelMismatch,
        message: "model mismatch".to_owned(),
        guidance: "Re-embed with the corpus's active model.".to_owned(),
        details: serde_json::json!({
            "client_model": "voyage-code-3@2",
            "corpus_model": "voyage-code-3@1",
            "remediation": "switch to revision 1"
        }),
        suggested_next_actions: vec![],
    };
    let sc = mismatch
        .into_result()
        .structured_content
        .expect("structuredContent present");
    assert_conforms("error (mismatch)", &sc, &mnm_mcp::schemas::error_output_schema());
}

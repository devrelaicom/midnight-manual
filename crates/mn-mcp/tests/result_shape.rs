//! The new result contract: success = one text block (summary + fenced json) +
//! structuredContent conforming to the tool's outputSchema; failure = isError.

use jsonschema::JSONSchema;

#[test]
fn search_structured_conforms_to_output_schema() {
    let env = serde_json::json!({
        "corpus_embedding_model": "voyage-code-3@1",
        "results": [{ "chunk_id": "a", "document_id": "b", "source_path": "docs/x.md",
                      "source_slug": "s", "source_display_name": "S", "heading_path": [],
                      "symbol_path": [], "content": "c",
                      "scores": { "confidence": 0.9, "trust_score": 1.0,
                                  "confidence_factors": { "attribution": "foundation", "verified": true } } }],
        "search_metadata": { "filtered_by_confidence": 0, "deduplicated_count": 0 }
    });
    let result = mn_mcp::render::project_search(env, None).into_result();
    let sc = result.structured_content.as_ref().expect("structuredContent present");

    let schema = mn_mcp::schemas::search_output_schema();
    let compiled = JSONSchema::compile(&schema).expect("schema compiles");
    assert!(compiled.is_valid(sc), "search structuredContent must conform to its outputSchema");

    // text block = summary + fenced json, not an isError
    let text = match &result.content[0] {
        mn_mcp::protocol::ContentBlock::Text { text } => text,
    };
    assert!(text.contains("```json"), "text block must embed a fenced json view");
    assert!(!result.is_error);
}

#[test]
fn all_passthrough_projectors_conform_to_passthrough_schema() {
    // A representative success from each passthrough projector validates against
    // its (permissive) outputSchema and is an object.
    let chunk_env = serde_json::json!({
        "id": "c1", "chunk_index": 0, "total_chunks": 1, "content": "x",
        "heading_path": [], "document": { "source_path": "docs/x.md" }, "source": { "display_name": "S" }
    });
    let cases: Vec<(serde_json::Value, serde_json::Value)> = vec![
        (mn_mcp::render::project_chunk(chunk_env).into_result().structured_content.unwrap(),
         mn_mcp::schemas::chunk_output_schema()),
        (mn_mcp::render::project_sources(serde_json::json!([{ "slug": "s", "display_name": "S" }]))
            .into_result().structured_content.unwrap(),
         mn_mcp::schemas::sources_output_schema()),
        (mn_mcp::render::project_status(serde_json::json!({ "server_version":"0","reranker":"r","model_state":"ready" }))
            .into_result().structured_content.unwrap(),
         mn_mcp::schemas::status_output_schema()),
        (mn_mcp::render::project_parents(serde_json::json!([{ "name": "G" }]))
            .into_result().structured_content.unwrap(),
         mn_mcp::schemas::parents_output_schema()),
    ];
    for (sc, schema) in cases {
        assert!(sc.is_object(), "structuredContent must be an object: {sc}");
        let compiled = JSONSchema::compile(&schema).expect("schema compiles");
        assert!(compiled.is_valid(&sc), "structuredContent {sc} must conform to schema {schema}");
    }
}

#[test]
fn failure_is_iserror_with_error_code() {
    let f = mn_mcp::render::ToolFailure::simple(
        mn_mcp::render::ErrorKind::NotFound, "no chunk x", "Verify the id from a recent search.",
    );
    let result = f.into_result();
    assert!(result.is_error);
    let sc = result.structured_content.unwrap();
    assert_eq!(sc["error"]["code"], "NOT_FOUND");
}

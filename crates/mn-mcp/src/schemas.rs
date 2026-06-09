//! `outputSchema` JSON Schemas advertised per tool (MCP). `structuredContent`
//! on a success result conforms to the matching schema; conformance is asserted
//! in tests.

use serde_json::{json, Value};

fn chunk_fragment() -> Value {
    json!({
        "type": "object",
        "properties": {
            "chunk_id": { "type": "string" },
            "document_id": { "type": "string" },
            "source_slug": { "type": "string" },
            "source_display_name": { "type": "string" },
            "source_path": { "type": "string" },
            "heading_path": { "type": "array", "items": { "type": "string" } },
            "symbol_path": { "type": "array", "items": { "type": "string" } },
            "content": { "type": "string" }
        },
        "additionalProperties": true
    })
}

fn next_actions_fragment() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": { "tool": { "type": "string" }, "arguments": { "type": "object" } },
            "required": ["tool"]
        }
    })
}

/// Output schema for `search`.
pub fn search_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "corpus_embedding_model": { "type": "string" },
            "results": { "type": "array", "items": chunk_fragment() },
            "search_metadata": { "type": "object", "additionalProperties": true },
            "next_actions": next_actions_fragment()
        },
        "required": ["results"],
        "additionalProperties": true
    })
}

fn passthrough_object_schema() -> Value {
    json!({ "type": "object", "additionalProperties": true, "properties": { "next_actions": next_actions_fragment() } })
}

/// Output schema for `get_chunk`.
pub fn chunk_output_schema() -> Value {
    passthrough_object_schema()
}

/// Output schema for `get_chunk_next` / `get_chunk_prev`.
pub fn chunk_list_output_schema() -> Value {
    passthrough_object_schema()
}

/// Output schema for `get_chunk_neighbors`.
pub fn neighbors_output_schema() -> Value {
    passthrough_object_schema()
}

/// Output schema for `get_document` / `get_document_full` / `get_document_chunks`.
pub fn document_output_schema() -> Value {
    passthrough_object_schema()
}

/// Output schema for `list_sources`.
pub fn sources_output_schema() -> Value {
    passthrough_object_schema()
}

/// Output schema for `facets`.
pub fn facets_output_schema() -> Value {
    passthrough_object_schema()
}

/// Output schema for `status`.
pub fn status_output_schema() -> Value {
    passthrough_object_schema()
}

/// Output schema for `pull_models`.
pub fn pull_models_output_schema() -> Value {
    passthrough_object_schema()
}

/// Output schema for `install_search_skill`.
pub fn install_output_schema() -> Value {
    passthrough_object_schema()
}

/// Output schema for `get_chunk_parents` (array wrapped under `parents`).
pub fn parents_output_schema() -> Value {
    passthrough_object_schema()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_valid_json_schema_objects() {
        for s in [
            search_output_schema(),
            chunk_output_schema(),
            document_output_schema(),
            sources_output_schema(),
            facets_output_schema(),
            status_output_schema(),
            parents_output_schema(),
        ] {
            // Compiles as a schema (catches malformed schema definitions).
            jsonschema_compile(&s);
        }
    }

    /// Wraps the jsonschema 0.18.x compile API (`JSONSchema::compile`) and
    /// panics if the schema is malformed.
    fn jsonschema_compile(schema: &Value) {
        jsonschema::JSONSchema::compile(schema).expect("schema must be a valid JSON Schema");
    }
}

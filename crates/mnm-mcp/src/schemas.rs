//! `outputSchema` JSON Schemas advertised per tool (MCP). `structuredContent`
//! on a success result conforms to the matching schema; conformance is asserted
//! in `tests/result_shape.rs`.
//!
//! Design: each schema describes the *stable* fields its projector emits
//! (derived from the cloud read API shapes and the local report structs) and
//! `require`s the decision-critical ones, but keeps `additionalProperties: true`
//! on cloud-passthrough objects so the surface stays additive as the cloud
//! evolves — mirroring the passthrough-verbatim policy in `render.rs`. The
//! goal is a contract a schema-reading client can deserialize, validate, and
//! render UI from, without pinning every incidental field.

use serde_json::{json, Value};

fn suggested_next_actions_fragment() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "What this suggested action achieves. Actions are suggestions, not required next steps." },
                "tool": { "type": "string", "description": "Tool to call. Absent for actions the user (not the agent) must take." },
                "arguments": { "type": "object" }
            },
            "required": ["description"]
        }
    })
}

/// A retrieval result's nested `scores` block (the cloud scoring layer; see
/// `mnm_core::scoring`). Describes the fields the projectors and clients read;
/// additive-open for the rest of the per-factor breakdown.
fn scores_fragment() -> Value {
    json!({
        "type": "object",
        "properties": {
            "confidence": { "type": "number", "description": "Blended confidence in [0,1]." },
            "trust_score": { "type": "number", "description": "Content trust in [0,1]." },
            "matched_queries": { "type": "array", "items": { "type": "integer" },
                "description": "Indices of the fused queries that matched (advanced_search only; basic search strips this)." },
            "confidence_factors": {
                "type": "object",
                "properties": {
                    "attribution": { "type": "string", "enum": mnm_retrieval::facets::ATTRIBUTION_VALUES,
                        "description": "Source attribution that drove the dominant trust multiplier." },
                    "verified": { "type": "boolean" },
                    "relevance_source": { "type": "string", "enum": ["rrf", "rerank"],
                        "description": "Which relevance term fed the confidence blend." }
                },
                "required": ["attribution"],
                "additionalProperties": true
            }
        },
        "required": ["confidence", "confidence_factors"],
        "additionalProperties": true
    })
}

/// The **structured** `symbol_path` shape emitted by the body-read endpoints —
/// the `get_chunks` family (chunk-read) and `get_document_chunks` (window) —
/// where the server serializes [`mnm_core::types::SymbolSegment`] verbatim: an
/// ordered array of `{kind, name, path}` segments (`path` = the segment's
/// ancestor names, outermost first, omitted when empty).
///
/// This is deliberately richer than the flat name-string list `search`
/// returns (see [`search_result_fragment`], where `symbol_path` is
/// `items: {type: "string"}`): the search route intentionally flattens the
/// segments and drops `kind` for a ranked hit, whereas the read endpoints hand
/// back the full segment structs. Issue #132 fixed the drift between this true
/// shape and the schema that used to advertise `items: {type: "string"}` here.
fn symbol_path_fragment() -> Value {
    json!({
        "type": "array",
        "description": "Code-symbol path as structured `{kind, name, path}` segments (empty for prose chunks); `path` = ancestor symbol names, present for nested symbols. Richer than search's flat name-string list.",
        "items": {
            "type": "object",
            "properties": {
                "kind": { "type": "string",
                    "description": "Syntactic kind: impl, fn, class, interface, key, element, …" },
                "name": { "type": "string", "description": "Identifier or label for this segment." },
                "path": { "type": "array", "items": { "type": "string" },
                    "description": "Ancestor symbol names, outermost first; omitted when empty." }
            },
            "required": ["kind", "name"],
            "additionalProperties": true
        }
    })
}

/// One `search` / `advanced_search` result: the chunk fields, the promoted
/// top-level `rank` / `confidence` / `attribution` (issue #88 — mirrors the
/// trimmed text fence), and the nested `scores` block (kept for full fidelity).
fn search_result_fragment() -> Value {
    json!({
        "type": "object",
        "properties": {
            "chunk_id": { "type": "string" },
            "document_id": { "type": "string" },
            "source_slug": { "type": "string" },
            "source_display_name": { "type": "string" },
            "source_path": { "type": "string" },
            "heading_path": { "type": "array", "items": { "type": "string" } },
            // Flat name-string list: the search route flattens the structured
            // segments and drops `kind` for a ranked hit. Its order is not a
            // containment hierarchy (post dual-embeddings cutover, nesting lives
            // in each segment's `path`, which search drops). The body-read
            // endpoints return the richer object form (`symbol_path_fragment`)
            // — see issue #132.
            "symbol_path": { "type": "array", "items": { "type": "string" },
                "description": "Flat list of symbol names (kind dropped; order is not a containment hierarchy). Empty for prose chunks. The get_chunks family and get_document_chunks return the richer structured `{kind, name, path}` segment form." },
            "content": { "type": "string" },
            "rank": { "type": "integer", "minimum": 1,
                "description": "1-based position in the returned result list." },
            "confidence": { "type": ["number", "null"],
                "description": "Promoted copy of scores.confidence (top-level for direct reads)." },
            "attribution": { "type": "string",
                "description": "Promoted copy of scores.confidence_factors.attribution." },
            "scores": scores_fragment(),
            "rerank_score": { "type": "number",
                "description": "Voyage relevance score; present only when a local (BYOK) rerank ran." }
        },
        // `rank`/`confidence`/`attribution` are guaranteed by promotion in
        // `render::promote_result_scores`; `scores` is not required because the
        // cloud may omit it (older corpus, or `include_scores=false`), in which
        // case promotion still fills the top-level fields (null / "").
        "required": ["chunk_id", "content", "rank", "confidence", "attribution"],
        "additionalProperties": true
    })
}

/// A ChunkWithContext entry (`get_chunks` / `get_chunk_next` / `get_chunk_prev`
/// / `get_chunk_neighbors`): chunk fields flat, `document` / `source` nested.
///
/// The server serializes `mnm_store::entities::chunk::ChunkWithContext`
/// directly, so `symbol_path` is the **structured** `[{kind, name, path}]` form
/// (`symbol_path_fragment`), not the flat name strings `search` returns — issue
/// #132.
fn chunk_with_context_fragment() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string" },
            "chunk_index": { "type": "integer" },
            "total_chunks": { "type": "integer" },
            "document_id": { "type": "string" },
            "content": { "type": "string" },
            "heading_path": { "type": "array", "items": { "type": "string" } },
            "symbol_path": symbol_path_fragment(),
            "document": { "type": "object", "additionalProperties": true },
            "source": { "type": "object", "additionalProperties": true }
        },
        "required": ["id", "content"],
        "additionalProperties": true
    })
}

/// Output schema for `search` / `advanced_search`.
pub fn search_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "corpus_embedding_model": { "type": "string" },
            "corpus_code_embedding_model": { "type": "string",
                "description": "Present when the code-vector half ran (code_mode != off)." },
            "results": { "type": "array", "items": search_result_fragment() },
            "search_metadata": { "type": "object", "additionalProperties": true },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["results", "suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Output schema for `get_chunks` (`{ chunks, missing }`).
pub fn chunks_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "chunks": { "type": "array", "items": chunk_with_context_fragment() },
            "missing": { "type": "array", "items": { "type": "string" },
                "description": "Ids that did not resolve to a chunk." },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["chunks", "suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Output schema for `get_chunk_next` / `get_chunk_prev` (`{ chunks }`).
pub fn chunk_list_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "chunks": { "type": "array", "items": chunk_with_context_fragment() },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["chunks", "suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Output schema for `get_chunk_neighbors`
/// (`{ prev: {chunks}, chunk, next: {chunks} }`).
pub fn neighbors_output_schema() -> Value {
    let side = || {
        json!({
            "type": "object",
            "properties": { "chunks": { "type": "array", "items": chunk_with_context_fragment() } },
            "required": ["chunks"],
            "additionalProperties": true
        })
    };
    json!({
        "type": "object",
        "properties": {
            "prev": side(),
            "next": side(),
            "chunk": chunk_with_context_fragment(),
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["prev", "next", "chunk", "suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Output schema for `get_chunk_parents`
/// (`{ parents: [ParentNode..], source }`, immediate parent → root).
pub fn parents_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "parents": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "name": { "type": "string" },
                    "kind": { "type": "string", "description": "Node kind (e.g. document, folder, root)." },
                    "document_id": { "type": ["string", "null"],
                        "description": "Set on the document-kind node; null otherwise." }
                },
                "required": ["id", "name", "kind"],
                "additionalProperties": true
            } },
            "source": { "type": "object", "additionalProperties": true },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["parents", "source", "suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Output schema for `get_document` — DocumentOverview: metadata plus an
/// ordered chunk skeleton (`{id, chunk_index, token_count}`, no bodies).
pub fn document_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string" },
            "source_path": { "type": "string" },
            "language": { "type": ["string", "null"],
                "description": "ISO language tag or extension fallback; null when undetected." },
            "source": { "type": "object", "additionalProperties": true },
            "chunks": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "chunk_index": { "type": "integer" },
                    "token_count": { "type": "integer" }
                },
                "required": ["id", "chunk_index"],
                "additionalProperties": true
            } },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["id", "source_path", "chunks", "suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Output schema for `get_document_chunks` — DocumentChunkWindow: a positional
/// window of chunk *bodies* (`{chunk_id, chunk_index, content, heading_path,
/// token_count, symbol_path}`) plus window metadata. Distinct shape from
/// `get_document`'s skeleton overview.
///
/// `symbol_path` carries the same **structured** `{kind, name, path}` segments
/// as the `get_chunks` family (`symbol_path_fragment`) — this is the only
/// windowed body-read path, so it must not lose the code-symbol context (issue
/// #132). Empty for prose chunks.
pub fn document_window_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string" },
            "source_path": { "type": "string" },
            "from": { "type": "integer", "minimum": 0 },
            "total_chunks": { "type": "integer", "minimum": 0 },
            "source": { "type": "object", "additionalProperties": true },
            "chunks": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "chunk_id": { "type": "string" },
                    "chunk_index": { "type": "integer" },
                    "content": { "type": "string" },
                    "heading_path": { "type": "array", "items": { "type": "string" } },
                    "token_count": { "type": "integer" },
                    "symbol_path": symbol_path_fragment()
                },
                "required": ["chunk_id"],
                "additionalProperties": true
            } },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["id", "source_path", "total_chunks", "chunks", "suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Output schema for `list_sources` (`{ sources, total, next_cursor? }`).
pub fn sources_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sources": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "slug": { "type": "string" },
                    "display_name": { "type": "string" },
                    "kind": { "type": "string" }
                },
                "required": ["id", "slug"],
                "additionalProperties": true
            } },
            "total": { "type": "integer", "minimum": 0 },
            "next_cursor": { "type": ["string", "null"],
                "description": "Opaque pagination token; null on the last page." },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["sources", "total", "suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Output schema for `facets`. `GET /v1/facets` returns one of two shapes
/// depending on the request: the overview (carries `modes` + `filters`) and a
/// single facet's drill-down page (carries `facet` + `values`). Both are
/// described in one root **object** schema — required only by what they share
/// (`suggested_next_actions`) — rather than a root `oneOf`, because strict MCP
/// clients require every advertised `outputSchema` to carry a root
/// `type: "object"` and reject a bare combinator.
pub fn facets_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            // Overview shape.
            "modes": { "type": "array", "items": { "type": "string" } },
            "filters": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Filter dimension name for advanced_search." },
                    "type": { "type": "string" },
                    "negatable": { "type": "boolean" },
                    "values": { "type": "array" },
                    "truncated": { "type": "boolean" },
                    "total": { "type": "integer" }
                },
                "required": ["key"],
                "additionalProperties": true
            } },
            // Drill-down shape.
            "facet": { "type": "string", "description": "The drilled-into dimension (drill-down responses only)." },
            "values": { "type": "array", "items": { "type": "string" },
                "description": "Values of the drilled-into facet (drill-down responses only)." },
            "total": { "type": "integer", "minimum": 0 },
            "next_cursor": { "type": ["string", "null"],
                "description": "Opaque drill-down pagination token; null on the last page." },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Output schema for `status` (StatusReport — see `crate::status`).
pub fn status_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "mcp_version": { "type": "string" },
            "cloud": { "type": "string", "enum": ["reachable", "degraded", "unreachable"] },
            "cloud_version": { "type": ["string", "null"] },
            "authenticated": { "type": "boolean" },
            "auth_type": { "type": "string", "description": "anonymous / read_uplift / admin." },
            "identity": { "type": ["string", "null"] },
            "permission_level": { "type": "string", "description": "read / write / admin." },
            "rate_limit": { "type": ["object", "null"], "additionalProperties": true },
            "token_limits": { "type": ["object", "null"], "additionalProperties": true },
            "voyage": { "type": "string", "enum": ["valid", "invalid_key", "unreachable", "not_configured"] },
            "reranker": { "type": "string" },
            "reranker_loaded": { "type": "boolean" },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["mcp_version", "cloud", "authenticated", "auth_type", "permission_level",
                     "voyage", "reranker", "reranker_loaded", "suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Output schema for `install_search_skill` (InstallReport — see `mnm_skills`).
pub fn install_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "skill_name": { "type": "string" },
            "scope": { "type": "string", "enum": ["user", "project"] },
            "installed": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "harness": { "type": "string" },
                    "scope": { "type": "string" },
                    "path": { "type": "string" },
                    "action": { "type": "string", "description": "What happened (e.g. created / updated)." },
                    "reload_step": { "type": "string" }
                },
                "required": ["harness", "action"],
                "additionalProperties": true
            } },
            "detected": { "type": "array", "items": { "type": "string" } },
            "not_detected": { "type": "array", "items": { "type": "string" } },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["skill_name", "scope", "installed", "suggested_next_actions"],
        "additionalProperties": true
    })
}

/// Machine-readable schema for the shared **error** result `structuredContent`
/// (issue #89 C2). Tool failures are MCP results with `isError: true` carrying
/// `{ error: { code, retryable, message, ...details }, suggested_next_actions }`.
/// `code` is the closed set derived from [`crate::render::ErrorKind`], so the
/// discoverable schema, the prose `error_envelope`, and the code never drift.
/// Published as `errorSchema` in the contract artifact, not advertised per tool
/// (success `outputSchema` constrains success output only, by design).
pub fn error_output_schema() -> Value {
    let codes: Vec<&'static str> = crate::render::ErrorKind::ALL
        .iter()
        .map(|k| k.code())
        .collect();
    json!({
        "type": "object",
        "properties": {
            "error": {
                "type": "object",
                "properties": {
                    "code": { "type": "string", "enum": codes,
                        "description": "Closed set of tool-execution error codes." },
                    "retryable": { "type": "boolean",
                        "description": "false means an identical retry cannot succeed — recovery needs a different call (follow suggested_next_actions)." },
                    "message": { "type": "string", "description": "Human-readable error detail." },
                    "client_model": { "type": "string", "description": "EMBEDDING_MODEL_MISMATCH only: the {name}@{revision} the client embedded with." },
                    "corpus_model": { "type": "string", "description": "EMBEDDING_MODEL_MISMATCH only: the corpus's active {name}@{revision}." },
                    "remediation": { "type": "string", "description": "EMBEDDING_MODEL_MISMATCH only: concrete next step (cloud-provided)." },
                    "retry_after_secs": { "type": "integer", "description": "RATE_LIMITED only: seconds to WAIT before retrying (from the server's Retry-After header, or a conservative default when absent). Do not retry sooner." },
                    "rate_limit": { "type": "object", "description": "RATE_LIMITED only: snapshot of the server's X-RateLimit-* headers at rejection — any of { limit, remaining, reset_secs } the server sent." },
                    "auth_reason": { "type": "string", "enum": ["invalid_credentials", "insufficient_tier"], "description": "AUTH_FAILED only: invalid_credentials (401 — invalid or expired token) or insufficient_tier (403 — valid token, request not permitted for the account, typically an access-tier restriction)." }
                },
                "required": ["code", "retryable", "message"],
                "additionalProperties": true
            },
            "suggested_next_actions": suggested_next_actions_fragment()
        },
        "required": ["error", "suggested_next_actions"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_valid_json_schema_objects() {
        for s in [
            search_output_schema(),
            chunks_output_schema(),
            chunk_list_output_schema(),
            neighbors_output_schema(),
            parents_output_schema(),
            document_output_schema(),
            document_window_output_schema(),
            sources_output_schema(),
            facets_output_schema(),
            status_output_schema(),
            install_output_schema(),
            error_output_schema(),
        ] {
            // Compiles as a schema (catches malformed schema definitions).
            jsonschema_compile(&s);
        }
    }

    #[test]
    fn error_schema_enumerates_every_error_kind() {
        let schema = error_output_schema();
        let enumerated: Vec<&str> = schema["properties"]["error"]["properties"]["code"]["enum"]
            .as_array()
            .expect("code.enum is an array")
            .iter()
            .map(|v| v.as_str().expect("enum entry is a string"))
            .collect();
        let from_code: Vec<&str> = crate::render::ErrorKind::ALL
            .iter()
            .map(|k| k.code())
            .collect();
        assert_eq!(enumerated, from_code, "error schema enum must mirror ErrorKind::ALL");
    }

    /// Wraps the jsonschema 0.18.x compile API (`JSONSchema::compile`) and
    /// panics if the schema is malformed.
    fn jsonschema_compile(schema: &Value) {
        jsonschema::JSONSchema::compile(schema).expect("schema must be a valid JSON Schema");
    }
}

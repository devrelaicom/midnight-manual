//! Minimal MCP / JSON-RPC 2.0 types (FR-036).
//!
//! The Model Context Protocol speaks JSON-RPC 2.0 over a transport (stdio in
//! our case, per US5). We hand-roll the types here rather than depend on an
//! external MCP SDK — those crates are still moving fast in 2026 (per
//! research.md R-2). The small tool surface our server exposes (see
//! [`crate::tools::list`], the single source of truth) is compact enough
//! that the wire types fit in this module.

use serde::{Deserialize, Serialize};

/// JSON-RPC 2.0 protocol version sentinel.
pub const JSONRPC: &str = "2.0";

/// MCP protocol version we declare in the `initialize` response.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Request id: per JSON-RPC 2.0, either a string or a number.
///
/// [`RequestId::Null`] exists ONLY so an error response can carry `"id": null`
/// when the request id can't be determined — JSON-RPC 2.0 mandates `null` for
/// that case (§5.1). A well-formed request never carries a null id: the
/// classifier in [`Incoming::classify`] rejects a request whose `id` is `null`
/// (or a float / out-of-`i64`-range number) with [`ErrorCode::InvalidRequest`]
/// rather than admitting it here (issue #173).
///
/// The untagged representation serializes `Number`/`String` as the bare scalar
/// and `Null` as JSON `null`; deserialization tries the variants in order, so a
/// non-integer / out-of-range number matches none and fails (the classifier
/// relies on that to reject a malformed id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Numeric id.
    Number(i64),
    /// String id.
    String(String),
    /// Undetermined id, serialized as JSON `null`. Response-only — see the type
    /// docs; a request is never classified with this variant.
    Null,
}

/// Incoming JSON-RPC request from the client.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    /// MUST be "2.0".
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// The method name (e.g. "initialize", "tools/list", "tools/call").
    pub method: String,
    /// Method-specific parameters; left as raw Value for now and decoded per-method.
    #[serde(default)]
    pub params: serde_json::Value,
    /// Request id (must be set for non-notification requests).
    pub id: RequestId,
}

/// Incoming notification (no `id`, no expected response).
#[derive(Debug, Clone, Deserialize)]
pub struct Notification {
    /// MUST be "2.0".
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// The notification method name.
    pub method: String,
    /// Optional payload.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A classified incoming JSON-RPC message.
///
/// We classify by hand rather than with `#[serde(untagged)]` because the
/// untagged fall-through silently reclassified a *request* whose `id` failed to
/// deserialize (a bignum `> i64::MAX`, a non-integer float, or `null`) as a
/// [`Notification`] and dropped it — leaving a strict client waiting forever
/// (issue #173). Hand classification also lets malformed-JSON ([`ErrorCode::ParseError`])
/// be told apart from well-formed-but-invalid requests ([`ErrorCode::InvalidRequest`]),
/// which the untagged parse collapsed into a single parse error.
#[derive(Debug)]
pub enum Incoming {
    /// A regular request expecting a response.
    Request(Request),
    /// A fire-and-forget notification (has a `method`, no `id`).
    Notification(Notification),
    /// The message cannot be dispatched. `id` mirrors the request id when it can
    /// be determined and is [`RequestId::Null`] otherwise (JSON-RPC 2.0 §5.1);
    /// `code` is [`ErrorCode::ParseError`] for invalid JSON or
    /// [`ErrorCode::InvalidRequest`] for a well-formed object that is not a valid
    /// request. The caller turns this into an error [`Response`].
    Invalid {
        /// Id to echo (or [`RequestId::Null`] when undeterminable).
        id: RequestId,
        /// `-32700` (parse) or `-32600` (invalid request).
        code: ErrorCode,
        /// Operator-facing reason.
        message: String,
    },
}

/// Outcome of interpreting a message's `id` field.
enum IdState {
    /// A valid JSON-RPC id (string, or integer within `i64` range).
    Valid(RequestId),
    /// Present but not a valid id (a float, a number outside `i64` range,
    /// `null`, a bool, an array, or an object).
    Invalid,
    /// No `id` field at all — a notification candidate.
    Absent,
}

/// Interpret a raw `id` field into an [`IdState`]. A JSON-RPC id MUST be a
/// string or an integer; everything else (float, out-of-range integer, `null`,
/// bool, array, object) is [`IdState::Invalid`], and an absent field is
/// [`IdState::Absent`].
fn classify_id(id: Option<&serde_json::Value>) -> IdState {
    match id {
        None => IdState::Absent,
        Some(serde_json::Value::String(s)) => IdState::Valid(RequestId::String(s.clone())),
        // `as_i64` is `Some` only for an integer that fits `i64`; a float (e.g.
        // `1.0`) or an out-of-range integer (bignum `> i64::MAX`) yields `None`.
        Some(serde_json::Value::Number(n)) => n
            .as_i64()
            .map_or(IdState::Invalid, |i| IdState::Valid(RequestId::Number(i))),
        Some(_) => IdState::Invalid,
    }
}

impl Incoming {
    /// Classify a raw framed message into a [`Request`], a [`Notification`], or
    /// an [`Incoming::Invalid`] describing the correct JSON-RPC error.
    ///
    /// A message is a **request** when it has a string `method` and a valid `id`;
    /// a **notification** when it has a string `method` and no `id`. A `method`
    /// with an invalid `id`, or a valid `id` with no `method`, or any non-object
    /// JSON, is [`Incoming::Invalid`]. Invalid JSON is a parse error.
    #[must_use]
    pub fn classify(body: &[u8]) -> Self {
        let value: serde_json::Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => {
                return Self::Invalid {
                    id: RequestId::Null,
                    code: ErrorCode::ParseError,
                    message: e.to_string(),
                };
            }
        };

        // A JSON-RPC request/notification is a JSON object. Anything else
        // (array batch — unsupported — or a bare scalar) is invalid, id
        // undeterminable.
        let Some(obj) = value.as_object() else {
            return Self::Invalid {
                id: RequestId::Null,
                code: ErrorCode::InvalidRequest,
                message: "JSON-RPC message must be a JSON object".to_owned(),
            };
        };

        // Own the `method` and classify the `id` up front so the `obj` borrow of
        // `value` is released before the request arm moves `value` below.
        let method = obj
            .get("method")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        let id_state = classify_id(obj.get("id"));

        match (method, id_state) {
            // method + valid id → a request. `from_value` also enforces the rest
            // of the envelope (e.g. `jsonrpc`); if that fails we still know the id.
            (Some(_), IdState::Valid(id)) => match serde_json::from_value::<Request>(value) {
                Ok(req) => Self::Request(req),
                Err(e) => Self::Invalid {
                    id,
                    code: ErrorCode::InvalidRequest,
                    message: e.to_string(),
                },
            },
            // method, no id → a notification. Built directly (not via `from_value`)
            // so a missing/odd `jsonrpc` on an id-less message never turns into a
            // spurious error response — id-less messages get no response.
            (Some(method), IdState::Absent) => Self::Notification(Notification {
                jsonrpc: JSONRPC.to_owned(),
                method,
                params: value
                    .get("params")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            }),
            // method present but the id is malformed (bignum/float/null/…): this
            // is a request, so it MUST get a response — the bug was dropping it as
            // a notification. The id is undeterminable, so `null` (issue #173).
            (Some(_), IdState::Invalid) => Self::Invalid {
                id: RequestId::Null,
                code: ErrorCode::InvalidRequest,
                message: "JSON-RPC request `id` must be a string or an integer within i64 range"
                    .to_owned(),
            },
            // No usable `method`: a well-formed object that isn't a valid request.
            // Echo the id when we have a valid one, else `null`.
            (None, IdState::Valid(id)) => Self::Invalid {
                id,
                code: ErrorCode::InvalidRequest,
                message: "JSON-RPC request is missing a string `method`".to_owned(),
            },
            (None, IdState::Invalid | IdState::Absent) => Self::Invalid {
                id: RequestId::Null,
                code: ErrorCode::InvalidRequest,
                message: "JSON-RPC request is missing a string `method`".to_owned(),
            },
        }
    }
}

/// Outgoing JSON-RPC response shape.
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    /// MUST be "2.0".
    pub jsonrpc: &'static str,
    /// Mirrors the request id.
    pub id: RequestId,
    /// Method result on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error envelope on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl Response {
    /// Build a success response.
    #[must_use]
    pub const fn success(id: RequestId, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC,
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Build an error response.
    #[must_use]
    pub fn err(id: RequestId, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC,
            id,
            result: None,
            error: Some(JsonRpcError {
                code: code as i32,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// JSON-RPC error envelope.
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcError {
    /// Numeric code (see [`ErrorCode`]).
    pub code: i32,
    /// Operator-facing summary.
    pub message: String,
    /// Optional structured data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// JSON-RPC standard error codes plus MCP-extension space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ErrorCode {
    /// Parse error (invalid JSON).
    ParseError = -32_700,
    /// Invalid request (not conformant to the spec).
    InvalidRequest = -32_600,
    /// Method not found.
    MethodNotFound = -32_601,
    /// Invalid params for the requested method.
    InvalidParams = -32_602,
    /// Internal error (catchall).
    InternalError = -32_603,
    /// Tool not found (MCP-specific).
    ToolNotFound = -32_001,
    /// Tool call failed at runtime (MCP-specific).
    ToolFailed = -32_002,
    /// Prompt not found (MCP-specific).
    PromptNotFound = -32_003,
}

/// Server capabilities advertised in the `initialize` response.
#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    /// Tool support.
    pub tools: ToolsCapability,
    /// Prompt support.
    pub prompts: PromptsCapability,
}

/// Tool capability flags.
#[derive(Debug, Serialize)]
pub struct ToolsCapability {
    /// Whether tool descriptions can change at runtime.
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

/// Prompt capability flags.
#[derive(Debug, Serialize)]
pub struct PromptsCapability {
    /// Whether the prompt list can change at runtime.
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

/// Server `instructions` sent in the `initialize` response (MCP spec field,
/// issue #138). This is the ONLY channel that reaches an agent BEFORE its first
/// tool call, so it front-loads the what/when/when-not, the cold-start map, the
/// escalation ladder, a search-budget hint, and the skill pointer.
///
/// Host support is unreliable (several clients ignore this field and some
/// truncate it), so the critical lines are ALSO duplicated where they already
/// live — tool descriptions and `suggested_next_actions`. This is an additive
/// channel, never the sole home of load-bearing guidance. The most important
/// lines (what this is / when to use it) come first so they survive a ~500-char
/// truncation.
pub const SERVER_INSTRUCTIONS: &str = "\
Midnight Manual: hybrid (vector + keyword) search over the Midnight Network docs and code corpus — docs sites, SDK sources, and Compact examples — version-aware and trust-scored.

Use it for ANY question about Midnight, Compact, or the Midnight SDK, even ones you think you already know: training data about Midnight is frequently stale, so verify here first. Do NOT use it for general programming questions unrelated to Midnight, and it is not a substitute for reading the user's own project files.

Cold start (unsure what to query?): call `facets` with no arguments — the response carries a compact `corpus` overview (source counts by kind/attribution, top languages, version coverage, freshness, sample tags) plus the filter dimensions for `advanced_search`; call `list_sources` to see what material exists.

Escalation ladder: one plain question → `search`; filters, multi-query fusion, or rerank control → `advanced_search` (call `facets` first to discover valid filter values); read a hit with `get_chunks`, widen via the neighbor and parent tools, then `get_document` / `get_document_chunks` for full context.

Budget hint: if three searches haven't found it, change strategy — broaden terms, switch retrieval mode, or drop filters — rather than rephrasing the same query.

For the full retrieval playbook, run `install_skill` to install the bundled Midnight search skills into your AI harness.";

/// `initialize` response payload.
#[derive(Debug, Serialize)]
pub struct InitializeResult {
    /// MCP protocol version we speak.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    /// What we support.
    pub capabilities: ServerCapabilities,
    /// Server identity.
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    /// Free-form usage guidance for the agent (MCP `instructions` field). Sent
    /// as [`SERVER_INSTRUCTIONS`]; `None` omits it from the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<&'static str>,
}

/// Server identity block.
#[derive(Debug, Serialize)]
pub struct ServerInfo {
    /// Human-readable server name.
    pub name: &'static str,
    /// Crate version.
    pub version: &'static str,
}

/// MCP tool annotations (behavior hints for clients).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ToolAnnotations {
    /// Human-friendly display name for the tool (the MCP `title` annotation),
    /// shown by clients in place of the raw tool name. Omitted when unset.
    #[serde(rename = "title", skip_serializing_if = "Option::is_none")]
    pub title: Option<&'static str>,
    /// Tool does not modify its environment.
    #[serde(rename = "readOnlyHint")]
    pub read_only_hint: bool,
    /// Tool may perform destructive updates (only meaningful when not read-only).
    #[serde(rename = "destructiveHint", skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// Repeated identical calls have no additional effect.
    #[serde(rename = "idempotentHint", skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// Tool interacts with an open world of external entities.
    #[serde(rename = "openWorldHint")]
    pub open_world_hint: bool,
}

impl ToolAnnotations {
    /// Read-only, closed-world (every corpus/read tool).
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            title: None,
            read_only_hint: true,
            destructive_hint: None,
            idempotent_hint: None,
            open_world_hint: false,
        }
    }

    /// Local writer that only touches its own files, safely re-runnable.
    #[must_use]
    pub const fn idempotent_writer() -> Self {
        Self {
            title: None,
            read_only_hint: false,
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: false,
        }
    }

    /// Attach the human-friendly `title` display annotation.
    #[must_use]
    pub const fn with_title(mut self, title: &'static str) -> Self {
        self.title = Some(title);
        self
    }
}

/// One tool declaration in `tools/list` response.
#[derive(Debug, Serialize)]
pub struct ToolDescription {
    /// Tool name (e.g. "search").
    pub name: &'static str,
    /// Human-readable description (shown by AI clients).
    pub description: &'static str,
    /// JSON Schema for the tool's input parameters.
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
    /// JSON Schema for the tool's `structuredContent`.
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// Behavior hints (read-only / idempotent / open-world).
    pub annotations: ToolAnnotations,
}

/// `tools/list` response payload.
#[derive(Debug, Serialize)]
pub struct ToolsListResult {
    /// All available tools.
    pub tools: Vec<ToolDescription>,
}

/// One declared argument of a prompt.
#[derive(Debug, Serialize)]
pub struct PromptArgument {
    /// Argument name.
    pub name: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// Whether the client must supply it.
    pub required: bool,
}

/// One prompt declaration in a `prompts/list` response.
#[derive(Debug, Serialize)]
pub struct PromptDescription {
    /// Prompt name (e.g. `add_midnight_skills`).
    pub name: &'static str,
    /// Human-readable description (shown in client prompt menus).
    pub description: &'static str,
    /// Declared arguments.
    pub arguments: Vec<PromptArgument>,
}

/// `prompts/list` response payload.
#[derive(Debug, Serialize)]
pub struct PromptsListResult {
    /// All available prompts.
    pub prompts: Vec<PromptDescription>,
}

/// `prompts/get` request params.
#[derive(Debug, Deserialize)]
pub struct PromptGetParams {
    /// Prompt name to render.
    pub name: String,
    /// Caller-supplied arguments (string -> string map per MCP).
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// One message in a rendered prompt.
#[derive(Debug, Serialize)]
pub struct PromptMessage {
    /// `"user"` or `"assistant"`.
    pub role: &'static str,
    /// Message content (a single text block).
    pub content: ContentBlock,
}

/// `prompts/get` response payload.
#[derive(Debug, Serialize)]
pub struct PromptGetResult {
    /// Human-readable description of the rendered prompt.
    pub description: String,
    /// The rendered messages.
    pub messages: Vec<PromptMessage>,
}

/// `tools/call` request params.
#[derive(Debug, Deserialize)]
pub struct ToolCallParams {
    /// Tool name to invoke.
    pub name: String,
    /// Tool-specific arguments.
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// `tools/call` response payload.
#[derive(Debug, Serialize)]
pub struct ToolCallResult {
    /// Output content blocks (we always emit a single `text` block).
    pub content: Vec<ContentBlock>,
    /// Machine-readable result; conforms to the tool's `outputSchema` on success.
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    /// Set when the tool reported an error condition (vs. a hard JSON-RPC error).
    #[serde(rename = "isError", skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

/// One content block in a tool call response.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentBlock {
    /// Plain-text block.
    Text {
        /// The text content.
        text: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_round_trips() {
        let n = serde_json::to_value(RequestId::Number(7)).unwrap();
        assert_eq!(n, serde_json::json!(7));
        let s = serde_json::to_value(RequestId::String("abc".into())).unwrap();
        assert_eq!(s, serde_json::json!("abc"));
    }

    #[test]
    fn response_success_omits_error() {
        let r = Response::success(RequestId::Number(1), serde_json::json!({ "ok": true }));
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["ok"], true);
        assert!(v.get("error").is_none());
    }

    #[test]
    fn response_err_omits_result() {
        let r = Response::err(RequestId::String("x".into()), ErrorCode::ToolNotFound, "boom");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["error"]["code"], -32001);
        assert_eq!(v["error"]["message"], "boom");
        assert!(v.get("result").is_none());
    }

    #[test]
    fn incoming_distinguishes_request_from_notification() {
        let req_json = br#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#;
        let n_json = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        match Incoming::classify(req_json) {
            Incoming::Request(req) => assert_eq!(req.id, RequestId::Number(1)),
            other => panic!("expected Request, got {other:?}"),
        }
        assert!(matches!(Incoming::classify(n_json), Incoming::Notification(_)));
    }

    #[test]
    fn request_id_null_serializes_as_json_null() {
        // The whole undetermined-id fix rests on this: the untagged unit variant
        // must serialize to JSON `null`, not omit or `"Null"`.
        assert_eq!(serde_json::to_value(RequestId::Null).unwrap(), serde_json::Value::Null);
    }

    #[test]
    fn err_response_with_null_id_serializes_id_null() {
        let r = Response::err(RequestId::Null, ErrorCode::ParseError, "bad json");
        let v = serde_json::to_value(&r).unwrap();
        // Present on the wire AND explicitly null (JSON-RPC 2.0 §5.1), not omitted.
        assert!(v.as_object().unwrap().contains_key("id"));
        assert_eq!(v["id"], serde_json::Value::Null);
        assert_eq!(v["error"]["code"], -32700);
    }

    #[test]
    fn classify_invalid_json_is_parse_error_with_null_id() {
        // A truncated object is not valid JSON → ParseError (-32700), id null.
        match Incoming::classify(b"{\"jsonrpc\":\"2.0\"") {
            Incoming::Invalid { id, code, .. } => {
                assert_eq!(id, RequestId::Null);
                assert_eq!(code, ErrorCode::ParseError);
            }
            other => panic!("expected Invalid/ParseError, got {other:?}"),
        }
    }

    #[test]
    fn classify_wellformed_but_no_method_is_invalid_request() {
        // Valid JSON object with no `method` is a well-formed-but-invalid request
        // → InvalidRequest (-32600), NOT ParseError. Id undeterminable → null.
        match Incoming::classify(br#"{"jsonrpc":"2.0"}"#) {
            Incoming::Invalid { id, code, .. } => {
                assert_eq!(id, RequestId::Null);
                assert_eq!(code, ErrorCode::InvalidRequest);
            }
            other => panic!("expected Invalid/InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn classify_no_method_with_valid_id_echoes_that_id() {
        // When the id is determinable it must be echoed, not nulled.
        match Incoming::classify(br#"{"jsonrpc":"2.0","id":7}"#) {
            Incoming::Invalid { id, code, .. } => {
                assert_eq!(id, RequestId::Number(7));
                assert_eq!(code, ErrorCode::InvalidRequest);
            }
            other => panic!("expected Invalid/InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn classify_bignum_id_request_is_invalid_not_notification() {
        // The core #173 scenario: an id > i64::MAX must NOT fall through to a
        // notification (which would drop it) — it is an InvalidRequest, id null.
        let body = br#"{"jsonrpc":"2.0","id":12345678901234567890,"method":"tools/list"}"#;
        match Incoming::classify(body) {
            Incoming::Invalid { id, code, .. } => {
                assert_eq!(id, RequestId::Null);
                assert_eq!(code, ErrorCode::InvalidRequest);
            }
            other => panic!("bignum id must be Invalid, not {other:?}"),
        }
    }

    #[test]
    fn classify_float_and_null_ids_are_invalid_requests() {
        for body in [
            br#"{"jsonrpc":"2.0","id":1.0,"method":"tools/list"}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#.as_slice(),
        ] {
            match Incoming::classify(body) {
                Incoming::Invalid { id, code, .. } => {
                    assert_eq!(id, RequestId::Null, "body: {}", String::from_utf8_lossy(body));
                    assert_eq!(code, ErrorCode::InvalidRequest);
                }
                other => panic!("non-integer id must be Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn classify_batch_array_is_invalid_request_with_null_id() {
        // Valid JSON, but a top-level array (JSON-RPC batch — unsupported here) is
        // not a request object → InvalidRequest (-32600), id undeterminable → null.
        match Incoming::classify(br#"[{"jsonrpc":"2.0","method":"tools/list","id":1}]"#) {
            Incoming::Invalid { id, code, .. } => {
                assert_eq!(id, RequestId::Null);
                assert_eq!(code, ErrorCode::InvalidRequest);
            }
            other => panic!("a JSON array must be Invalid/InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn classify_string_id_request_round_trips() {
        // String ids remain valid requests.
        match Incoming::classify(br#"{"jsonrpc":"2.0","id":"abc","method":"tools/list"}"#) {
            Incoming::Request(req) => assert_eq!(req.id, RequestId::String("abc".into())),
            other => panic!("expected Request, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_result_serializes_structured_content() {
        let r = ToolCallResult {
            content: vec![ContentBlock::Text { text: "hi".into() }],
            structured_content: Some(serde_json::json!({ "k": 1 })),
            is_error: false,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["structuredContent"]["k"], 1);
        assert!(v.get("isError").is_none()); // false is skipped
    }

    #[test]
    fn tool_description_serializes_output_schema() {
        let d = ToolDescription {
            name: "x",
            description: "y",
            input_schema: serde_json::json!({}),
            output_schema: Some(serde_json::json!({ "type": "object" })),
            annotations: ToolAnnotations::read_only(),
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["outputSchema"]["type"], "object");
        assert_eq!(v["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn read_only_annotations_omit_optional_hints() {
        // skip_serializing_if must drop the two Option hints entirely — a
        // read-only tool advertises only readOnlyHint + openWorldHint.
        let v = serde_json::to_value(ToolAnnotations::read_only()).unwrap();
        assert_eq!(v["readOnlyHint"], true);
        assert_eq!(v["openWorldHint"], false);
        assert!(v.get("destructiveHint").is_none());
        assert!(v.get("idempotentHint").is_none());
    }

    #[test]
    fn idempotent_writer_annotations_carry_both_hints() {
        let v = serde_json::to_value(ToolAnnotations::idempotent_writer()).unwrap();
        assert_eq!(v["readOnlyHint"], false);
        assert_eq!(v["destructiveHint"], false);
        assert_eq!(v["idempotentHint"], true);
        assert_eq!(v["openWorldHint"], false);
    }

    #[test]
    fn with_title_sets_title_annotation_else_omitted() {
        // Default constructors omit `title` entirely (skip_serializing_if).
        let bare = serde_json::to_value(ToolAnnotations::read_only()).unwrap();
        assert!(bare.get("title").is_none());
        // `with_title` adds the human-friendly display name.
        let titled =
            serde_json::to_value(ToolAnnotations::read_only().with_title("Search corpus")).unwrap();
        assert_eq!(titled["title"], "Search corpus");
        assert_eq!(titled["readOnlyHint"], true);
    }
}

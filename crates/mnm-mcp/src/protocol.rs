//! Minimal MCP / JSON-RPC 2.0 types (FR-036).
//!
//! The Model Context Protocol speaks JSON-RPC 2.0 over a transport (stdio in
//! our case, per US5). We hand-roll the types here rather than depend on an
//! external MCP SDK — those crates are still moving fast in 2026 (per
//! research.md R-2). The 7-tool surface our server exposes is small enough
//! that the wire types fit in this module.

use serde::{Deserialize, Serialize};

/// JSON-RPC 2.0 protocol version sentinel.
pub const JSONRPC: &str = "2.0";

/// MCP protocol version we declare in the `initialize` response.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Request id: per JSON-RPC 2.0, either a string or a number (or null in a
/// notification, but we don't model that distinction here — see [`Notification`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Numeric id.
    Number(i64),
    /// String id.
    String(String),
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

/// JSON-RPC envelope: either a Request (with id) or a Notification (without).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Incoming {
    /// A regular request expecting a response.
    Request(Request),
    /// A fire-and-forget notification.
    Notification(Notification),
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
    /// Prompt name (e.g. `add_advanced_search_skill`).
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
        let req_json = r#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#;
        let n_json = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(matches!(
            serde_json::from_str::<Incoming>(req_json).unwrap(),
            Incoming::Request(_)
        ));
        assert!(matches!(
            serde_json::from_str::<Incoming>(n_json).unwrap(),
            Incoming::Notification(_)
        ));
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

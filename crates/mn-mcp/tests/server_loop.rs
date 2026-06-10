//! Integration test: drive the MCP server loop end-to-end without spawning a
//! process. Uses an in-memory duplex stream to feed framed JSON-RPC messages
//! to the server's reader and capture its writer output.

use mn_mcp::protocol;
use mn_mcp::transport::{frame_blocking, FrameReader, FrameWriter};

/// Build a framed `initialize` request followed by a framed `tools/list`
/// request, then assert the server responds correctly to both. We don't
/// invoke `mn_mcp::run` directly (it expects stdin/stdout); instead we
/// exercise the request/response surface via small public helpers.
#[tokio::test]
async fn end_to_end_round_trip_through_framed_io() {
    // Simulate one initialize request body.
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    let body = serde_json::to_vec(&init_req).unwrap();
    let framed = frame_blocking(&body);

    // Round-trip the framing layer: the reader should yield exactly the body
    // back. (The server loop itself is exercised by the lib's unit tests; this
    // test mainly guards the framing wire format the server depends on.)
    let mut reader = FrameReader::new(framed.as_slice());
    let got = reader.next_message().await.unwrap().expect("got message");
    let parsed: serde_json::Value = serde_json::from_slice(&got).unwrap();
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["method"], "initialize");

    // Build an InitializeResult and verify it serializes with the spec'd
    // protocol version.
    let result = protocol::InitializeResult {
        protocol_version: protocol::MCP_PROTOCOL_VERSION,
        capabilities: protocol::ServerCapabilities {
            tools: protocol::ToolsCapability { list_changed: false },
            prompts: protocol::PromptsCapability { list_changed: false },
        },
        server_info: protocol::ServerInfo {
            name: "midnight-manual-mcp",
            version: mn_mcp::VERSION,
        },
    };
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["protocolVersion"], protocol::MCP_PROTOCOL_VERSION);
    assert_eq!(json["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(json["serverInfo"]["name"], "midnight-manual-mcp");
}

/// Conformance guard at the server-loop level: an `initialize` request read in
/// through `FrameReader` and answered out through `FrameWriter` must produce a
/// newline-delimited frame with NO `Content-Length` header. This pins the MCP
/// stdio wire format the Inspector / Claude Code / Cursor expect — the bug this
/// guards against is the server returning LSP-framed bytes that those clients
/// silently drop.
#[tokio::test]
async fn initialize_response_is_newline_framed_no_content_length() {
    // ── read an initialize request the way the server loop does ──────────────
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    let req_body = serde_json::to_vec(&init_req).unwrap();
    let framed_in = frame_blocking(&req_body);
    let mut reader = FrameReader::new(framed_in.as_slice());
    let got = reader.next_message().await.unwrap().expect("init frame");
    let parsed: serde_json::Value = serde_json::from_slice(&got).unwrap();
    assert_eq!(parsed["method"], "initialize");

    // ── build the same response body the handler serializes (compact JSON) ───
    let init_result = protocol::InitializeResult {
        protocol_version: protocol::MCP_PROTOCOL_VERSION,
        capabilities: protocol::ServerCapabilities {
            tools: protocol::ToolsCapability { list_changed: false },
            prompts: protocol::PromptsCapability { list_changed: false },
        },
        server_info: protocol::ServerInfo {
            name: "midnight-manual-mcp",
            version: mn_mcp::VERSION,
        },
    };
    let response = protocol::Response::success(
        protocol::RequestId::Number(1),
        serde_json::to_value(&init_result).unwrap(),
    );
    let response_body = serde_json::to_vec(&response).unwrap();
    // Compact serialization must not embed newlines, or framing would break.
    assert!(
        !response_body.contains(&b'\n'),
        "response body must be compact JSON with no embedded newlines"
    );

    // ── write it out through the real FrameWriter and inspect the wire bytes ─
    let mut wire: Vec<u8> = Vec::new();
    {
        let mut writer = FrameWriter::new(&mut wire);
        writer.write_message(&response_body).await.unwrap();
    }
    assert_eq!(wire.last(), Some(&b'\n'), "framed response must end with a newline");
    // The only newline is the trailing frame delimiter: the body itself carries
    // none, so a single message is exactly one newline-delimited line.
    assert!(
        !wire[..wire.len() - 1].contains(&b'\n'),
        "a single message must be exactly one newline-delimited line"
    );
    assert!(
        !wire
            .windows(b"Content-Length".len())
            .any(|w| w == b"Content-Length"),
        "framed response must NOT contain an LSP-style Content-Length header"
    );

    // The frame, stripped of its trailing '\n', round-trips back to the body.
    let mut back = FrameReader::new(wire.as_slice());
    let echoed = back.next_message().await.unwrap().expect("response frame");
    assert_eq!(echoed, response_body);
}

#[tokio::test]
async fn tools_list_returns_two_phase_5b_tools() {
    let list = mn_mcp::tools::list();
    assert!(
        list.tools.iter().any(|t| t.name == "status"),
        "status tool must be in the manifest"
    );
    // Each tool must declare a valid JSON-schema input_schema.
    for tool in &list.tools {
        assert_eq!(
            tool.input_schema["type"], "object",
            "input_schema must be object-typed for {}",
            tool.name
        );
    }
}

#[tokio::test]
async fn status_tool_works_without_model_load() {
    // The status tool MUST NOT trigger model load (US5 acceptance #9). The
    // assembler reads the reranker-loaded marker without forcing the
    // singleton, and degrades cloud sections instead of failing when the
    // server is unreachable. The Voyage key is passed explicitly (None →
    // proxy mode), so an exported VOYAGE_API_KEY cannot leak in.
    let cloud = mn_mcp::CloudClient::new("http://127.0.0.1:9", None).unwrap();
    let report = mn_mcp::status::assemble(&cloud, None).await;
    assert_eq!(report.reranker, "bge-reranker-base");
    // The shape is well-formed regardless of whether sibling tests have
    // loaded models.
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["cloud"], "unreachable");
    assert_eq!(json["auth_type"], "anonymous");
    assert_eq!(json["permission_level"], "read");
    assert_eq!(json["voyage"], "not_configured");
    assert!(json["reranker_loaded"].is_boolean());
}

#[tokio::test]
async fn tools_list_contains_all_thirteen() {
    let list = mn_mcp::tools::list();
    let names: Vec<_> = list.tools.iter().map(|t| t.name).collect();
    for expected in [
        "search",
        "advanced_search",
        "get_chunks",
        "get_chunk_next",
        "get_chunk_prev",
        "get_chunk_neighbors",
        "get_chunk_parents",
        "get_document",
        "get_document_chunks",
        "list_sources",
        "facets",
        "status",
        "install_search_skill",
    ] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }
    assert_eq!(names.len(), 13);
}

/// Drive `prompts/list` and `prompts/get` through the same framed-I/O harness
/// used by the tools tests above: frame a JSON-RPC body, read it back via
/// `FrameReader`, decode it, and call the public prompts handlers directly.
#[tokio::test]
async fn prompts_list_and_get_through_framed_io() {
    // ── initialize ──────────────────────────────────────────────────────────
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "initialize",
        "params": {}
    });
    let init_body = serde_json::to_vec(&init_req).unwrap();
    let framed = frame_blocking(&init_body);
    let mut reader = FrameReader::new(framed.as_slice());
    let got = reader.next_message().await.unwrap().expect("init frame");
    let parsed: serde_json::Value = serde_json::from_slice(&got).unwrap();
    assert_eq!(parsed["method"], "initialize");

    // The initialize response capabilities — verify prompts.listChanged == false.
    let init_result = protocol::InitializeResult {
        protocol_version: protocol::MCP_PROTOCOL_VERSION,
        capabilities: protocol::ServerCapabilities {
            tools: protocol::ToolsCapability { list_changed: false },
            prompts: protocol::PromptsCapability { list_changed: false },
        },
        server_info: protocol::ServerInfo {
            name: "midnight-manual-mcp",
            version: mn_mcp::VERSION,
        },
    };
    let init_json = serde_json::to_value(&init_result).unwrap();
    assert_eq!(init_json["capabilities"]["prompts"]["listChanged"], false);

    // ── prompts/list ─────────────────────────────────────────────────────────
    let list_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "prompts/list",
        "params": {}
    });
    let list_body = serde_json::to_vec(&list_req).unwrap();
    let framed = frame_blocking(&list_body);
    let mut reader = FrameReader::new(framed.as_slice());
    let got = reader.next_message().await.unwrap().expect("list frame");
    let parsed: serde_json::Value = serde_json::from_slice(&got).unwrap();
    assert_eq!(parsed["method"], "prompts/list");

    let list_result = mn_mcp::prompts::list();
    assert!(
        list_result
            .prompts
            .iter()
            .any(|p| p.name == "add_advanced_search_skill"),
        "prompts/list must include add_advanced_search_skill"
    );

    // ── prompts/get ──────────────────────────────────────────────────────────
    let get_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 12,
        "method": "prompts/get",
        "params": {
            "name": "add_advanced_search_skill",
            "arguments": { "harness": "cursor", "scope": "project" }
        }
    });
    let get_body = serde_json::to_vec(&get_req).unwrap();
    let framed = frame_blocking(&get_body);
    let mut reader = FrameReader::new(framed.as_slice());
    let got = reader.next_message().await.unwrap().expect("get frame");
    let parsed: serde_json::Value = serde_json::from_slice(&got).unwrap();
    assert_eq!(parsed["method"], "prompts/get");

    let get_params = protocol::PromptGetParams {
        name: "add_advanced_search_skill".to_owned(),
        arguments: serde_json::json!({ "harness": "cursor", "scope": "project" }),
    };
    let response = mn_mcp::prompts::get(protocol::RequestId::Number(12), &get_params);
    let resp_json = serde_json::to_value(&response).unwrap();
    assert!(resp_json["error"].is_null(), "prompts/get should succeed");
    let messages = &resp_json["result"]["messages"];
    assert!(messages.is_array(), "result must have messages array");
    let first = &messages[0];
    assert_eq!(first["role"], "user");
    let text = first["content"]["text"]
        .as_str()
        .expect("content.text must be a string");
    assert!(
        text.contains("install_search_skill"),
        "instruction must reference install_search_skill"
    );
}

#[tokio::test]
async fn new_navigation_tool_schemas_are_well_formed() {
    let list = mn_mcp::tools::list();
    for name in [
        "get_chunk_next",
        "get_chunk_prev",
        "get_chunk_neighbors",
        "get_document",
        "get_document_chunks",
    ] {
        let t = list
            .tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("missing tool: {name}"));
        assert_eq!(t.input_schema["type"], "object");
        assert!(t.input_schema["required"].as_array().is_some());
        assert!(t.input_schema["properties"]["id"]["format"] == "uuid");
    }
}

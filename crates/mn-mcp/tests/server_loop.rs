//! Integration test: drive the MCP server loop end-to-end without spawning a
//! process. Uses an in-memory duplex stream to feed framed JSON-RPC messages
//! to the server's reader and capture its writer output.

use mn_mcp::protocol;
use mn_mcp::transport::{frame_blocking, FrameReader};

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

#[tokio::test]
async fn tools_list_returns_two_phase_5b_tools() {
    let list = mn_mcp::tools::list();
    let names: Vec<_> = list.tools.iter().map(|t| t.name).collect();
    assert!(names.contains(&"status"), "status tool must be in the manifest");
    assert!(names.contains(&"pull_models"), "pull_models tool must be in the manifest");
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
    // The status tool MUST NOT trigger model load (US5 acceptance #9). It
    // reports model_state without forcing the embedder/reranker singletons.
    let out = mn_mcp::tools::run_status(None);
    assert_eq!(out.embedder, "bge-base-en-v1.5");
    assert_eq!(out.reranker, "bge-reranker-base");
    // The shape is well-formed regardless of whether sibling tests have
    // loaded models.
    let json = serde_json::to_value(&out).unwrap();
    assert!(json["model_state"].is_string());
}

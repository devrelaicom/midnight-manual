//! MCP server event loop. Reads framed JSON-RPC messages from stdin,
//! dispatches them to handlers, writes responses to stdout.
//!
//! Logging goes to stderr (FR-021): stdout is reserved for the MCP wire.

use std::path::PathBuf;

use tokio::io::{stdin, stdout, Stdin, Stdout};
use tracing::{debug, info, warn};

use crate::protocol::{
    ContentBlock, ErrorCode, Incoming, InitializeResult, RequestId, Response, ServerCapabilities,
    ServerInfo, ToolCallParams, ToolCallResult, ToolsCapability, MCP_PROTOCOL_VERSION,
};
use crate::tools;
use crate::transport::{FrameReader, FrameWriter};

/// Per-instance server config (mostly the model cache path).
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Where the embedder / reranker store their ONNX files.
    pub cache_dir: PathBuf,
}

/// Run the MCP server until EOF on stdin.
///
/// # Errors
///
/// Returns the underlying io error if stdin or stdout fails. JSON-RPC and
/// tool-level errors are translated into wire responses and do NOT bubble up.
pub async fn run(cfg: ServerConfig) -> std::io::Result<()> {
    let stdin: Stdin = stdin();
    let stdout: Stdout = stdout();
    let mut reader = FrameReader::new(stdin);
    let mut writer = FrameWriter::new(stdout);

    info!("mn-mcp server: handshake ready, awaiting initialize");

    while let Some(body) = reader.next_message().await? {
        let response_body = match handle_message(&body, &cfg).await {
            Some(bytes) => bytes,
            None => continue, // notification — no response
        };
        writer.write_message(&response_body).await?;
    }

    info!("mn-mcp server: stdin EOF, shutting down");
    Ok(())
}

/// Decode and dispatch a single framed message. Returns `Some(response_bytes)`
/// for a request and `None` for a notification.
async fn handle_message(body: &[u8], cfg: &ServerConfig) -> Option<Vec<u8>> {
    let incoming: Incoming = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            // Per JSON-RPC 2.0, a parse error gets id=null; we don't have a
            // null variant in RequestId, so use the placeholder Number(0).
            warn!(error = %e, "parse error");
            let resp = Response::err(RequestId::Number(0), ErrorCode::ParseError, e.to_string());
            return Some(serde_json::to_vec(&resp).expect("serialize parse-err response"));
        }
    };

    match incoming {
        Incoming::Request(req) => Some(handle_request(req, cfg).await),
        Incoming::Notification(n) => {
            debug!(method = %n.method, "notification");
            None
        }
    }
}

async fn handle_request(req: crate::protocol::Request, cfg: &ServerConfig) -> Vec<u8> {
    let id = req.id.clone();
    let response = match req.method.as_str() {
        "initialize" => Response::success(
            id.clone(),
            serde_json::to_value(InitializeResult {
                protocol_version: MCP_PROTOCOL_VERSION,
                capabilities: ServerCapabilities {
                    tools: ToolsCapability { list_changed: false },
                },
                server_info: ServerInfo {
                    name: "midnight-manual-mcp",
                    version: crate::VERSION,
                },
            })
            .expect("serialize InitializeResult"),
        ),
        "tools/list" => Response::success(
            id.clone(),
            serde_json::to_value(tools::list()).expect("serialize tool list"),
        ),
        "tools/call" => match serde_json::from_value::<ToolCallParams>(req.params.clone()) {
            Ok(params) => dispatch_tool(id.clone(), params, cfg).await,
            Err(e) => Response::err(id.clone(), ErrorCode::InvalidParams, e.to_string()),
        },
        // Pings and shutdown follow MCP convention.
        "ping" => Response::success(id.clone(), serde_json::json!({})),
        other => {
            Response::err(id.clone(), ErrorCode::MethodNotFound, format!("unknown method: {other}"))
        }
    };
    serde_json::to_vec(&response).expect("serialize response")
}

async fn dispatch_tool(id: RequestId, params: ToolCallParams, cfg: &ServerConfig) -> Response {
    let result_text = match params.name.as_str() {
        "status" => {
            let out = tools::run_status(Some(&cfg.cache_dir));
            serde_json::to_string(&out).unwrap_or_else(|e| format!("serialize status: {e}"))
        }
        "pull_models" => match tools::run_pull_models(cfg.cache_dir.clone()).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("serialize: {e}")),
            Err(msg) => {
                return Response::err(id, ErrorCode::ToolFailed, msg);
            }
        },
        other => {
            return Response::err(id, ErrorCode::ToolNotFound, format!("unknown tool: {other}"));
        }
    };
    let result = ToolCallResult {
        content: vec![ContentBlock::Text { text: result_text }],
        is_error: false,
    };
    Response::success(id, serde_json::to_value(result).expect("serialize result"))
}

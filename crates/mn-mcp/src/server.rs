//! MCP server event loop. Reads framed JSON-RPC messages from stdin,
//! dispatches them to handlers, writes responses to stdout.
//!
//! Logging goes to stderr (FR-021): stdout is reserved for the MCP wire.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use mn_telemetry::events::{Component, EventPayload, McpToolName, ModelState, Outcome};
use mn_telemetry::{Event, TelemetryClient};
use tokio::io::{stdin, stdout, Stdin, Stdout};
use tracing::{debug, info, warn};

use crate::cloud_client::{CloudClient, CloudError};
use crate::protocol::{
    ContentBlock, ErrorCode, Incoming, InitializeResult, JsonRpcError, RequestId, Response,
    ServerCapabilities, ServerInfo, ToolCallParams, ToolCallResult, ToolsCapability, JSONRPC,
    MCP_PROTOCOL_VERSION,
};
use crate::tools;
use crate::transport::{FrameReader, FrameWriter};

/// Per-instance server config.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Where the embedder / reranker store their ONNX files.
    pub cache_dir: PathBuf,
    /// Base URL of the cloud server (`https://midnight-manual.midnightntwrk.expert` in
    /// production). Tools call this for everything except `status` /
    /// `pull_models`.
    pub cloud_url: String,
    /// Optional read-uplift bearer to forward as `Authorization: Bearer ...`
    /// on every cloud request. `None` means the MCP server is running in
    /// anonymous read mode.
    pub bearer_token: Option<String>,
    /// `{name}@{revision}` model identifier the cloud expects clients to
    /// declare on every search. For the v1 corpus this is
    /// `"bge-base-en-v1.5@1"` (seeded by migration 0006). Configurable here
    /// so tests can pin a different value.
    pub client_embedding_model: String,
    /// Resolved telemetry sink URL. Defaults to `{cloud_url}/v1/telemetry/events`.
    pub telemetry_url: String,
    /// Config-side master telemetry-enabled flag. The runtime opt-out
    /// resolver still wins over this (FR-107).
    pub telemetry_enabled: bool,
}

impl ServerConfig {
    /// Build a config with the production defaults: production cloud URL,
    /// no bearer, the seeded `bge-base-en-v1.5@1` model id, and telemetry
    /// enabled (subject to the opt-out resolver).
    #[must_use]
    pub fn with_defaults(cache_dir: PathBuf) -> Self {
        let cloud_url = "https://midnight-manual.midnightntwrk.expert".to_owned();
        let telemetry_url = format!("{cloud_url}/v1/telemetry/events");
        Self {
            cache_dir,
            cloud_url,
            bearer_token: None,
            client_embedding_model: "bge-base-en-v1.5@1".to_owned(),
            telemetry_url,
            telemetry_enabled: true,
        }
    }
}

/// Shared per-process state — the cloud HTTP client lives here so we don't
/// rebuild it on every tool call.
#[derive(Clone)]
struct ServerState {
    cfg: ServerConfig,
    cloud: Arc<CloudClient>,
    telemetry: Arc<TelemetryClient>,
    started_at: Arc<Instant>,
    tools_served: Arc<AtomicU32>,
}

/// Run the MCP server until EOF on stdin.
///
/// # Errors
///
/// Returns the underlying io error if stdin or stdout fails, or a string
/// error if the cloud client cannot be built. JSON-RPC and tool-level errors
/// are translated into wire responses and do NOT bubble up.
pub async fn run(cfg: ServerConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // FR-107 mechanism #3: honour any previously-set persistent marker
    // before constructing the telemetry client. Failing to resolve the
    // marker path (no `HOME`) degrades to "no marker", which is correct.
    let env = mn_core::config::StdEnv;
    mn_telemetry::optout::load_persistent_marker(
        mn_core::paths::telemetry_marker_path(&env).as_deref(),
    );
    let cloud = CloudClient::new(&cfg.cloud_url, cfg.bearer_token.clone())
        .map_err(|e| format!("build cloud client: {e}"))?;
    let telemetry = TelemetryClient::boot(&cfg.telemetry_url, cfg.telemetry_enabled)
        .map_err(|e| format!("build telemetry client: {e}"))?;
    let started_at = Arc::new(Instant::now());
    // Emit `mcp_startup` right away. The `startup_ms` field measures
    // process-start → here; for stdio MCP that's effectively 0 because the
    // event fires before the first JSON-RPC frame.
    let startup_ms = u32::try_from(started_at.elapsed().as_millis()).unwrap_or(u32::MAX);
    telemetry
        .emit(Event::new(
            Component::Mcp,
            crate::VERSION,
            EventPayload::McpStartup {
                startup_ms,
                model_state: ModelState::Missing,
            },
        ))
        .await;
    let state = ServerState {
        cfg,
        cloud: Arc::new(cloud),
        telemetry: Arc::new(telemetry),
        started_at,
        tools_served: Arc::new(AtomicU32::new(0)),
    };

    let stdin: Stdin = stdin();
    let stdout: Stdout = stdout();
    let mut reader = FrameReader::new(stdin);
    let mut writer = FrameWriter::new(stdout);

    info!("mn-mcp server: handshake ready, awaiting initialize");

    while let Some(body) = reader.next_message().await? {
        let response_body = match handle_message(&body, &state).await {
            Some(bytes) => bytes,
            None => continue, // notification — no response
        };
        writer.write_message(&response_body).await?;
    }

    info!("mn-mcp server: stdin EOF, shutting down");
    let uptime_s = u32::try_from(state.started_at.elapsed().as_secs()).unwrap_or(u32::MAX);
    let tools_served = state.tools_served.load(Ordering::Relaxed);
    state
        .telemetry
        .emit(Event::new(
            Component::Mcp,
            crate::VERSION,
            EventPayload::McpShutdown { uptime_s, tools_served },
        ))
        .await;
    state.telemetry.flush().await;
    Ok(())
}

/// Decode and dispatch a single framed message. Returns `Some(response_bytes)`
/// for a request and `None` for a notification.
async fn handle_message(body: &[u8], state: &ServerState) -> Option<Vec<u8>> {
    let incoming: Incoming = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "parse error");
            let resp = Response::err(RequestId::Number(0), ErrorCode::ParseError, e.to_string());
            return Some(serde_json::to_vec(&resp).expect("serialize parse-err response"));
        }
    };

    match incoming {
        Incoming::Request(req) => Some(handle_request(req, state).await),
        Incoming::Notification(n) => {
            debug!(method = %n.method, "notification");
            None
        }
    }
}

async fn handle_request(req: crate::protocol::Request, state: &ServerState) -> Vec<u8> {
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
            Ok(params) => dispatch_tool(id.clone(), params, state).await,
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

async fn dispatch_tool(id: RequestId, params: ToolCallParams, state: &ServerState) -> Response {
    let started = Instant::now();
    let tool_name_for_event = tool_name_for_event(&params.name);
    // `rerank` is only meaningful to the `search` tool; for everything else
    // the field doesn't exist in the schema, so the telemetry value is false.
    // Search's own default is `true` (see parse_search_args), so an absent
    // field there must log `true` to match what actually happened on the wire.
    let rerank_on = params.name == "search"
        && params
            .arguments
            .get("rerank")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
    let response = dispatch_tool_inner(id, params, state).await;
    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    state.tools_served.fetch_add(1, Ordering::Relaxed);
    if let Some(name) = tool_name_for_event {
        let outcome = if response.error.is_some() {
            Outcome::Error
        } else {
            Outcome::Ok
        };
        // We don't have access to the typed result count here without
        // re-parsing; the search tool exposes it on its own. Default to 0
        // for non-search tools — the spec lists result_count as 0 for those.
        state
            .telemetry
            .emit(Event::new(
                Component::Mcp,
                crate::VERSION,
                EventPayload::McpToolCall {
                    tool_name: name,
                    latency_ms,
                    result_count: 0,
                    model_state: ModelState::Missing,
                    rerank_on,
                    outcome,
                },
            ))
            .await;
    }
    response
}

fn tool_name_for_event(name: &str) -> Option<McpToolName> {
    match name {
        "search" => Some(McpToolName::Search),
        "get_chunk" => Some(McpToolName::GetChunk),
        "get_chunk_next" => Some(McpToolName::GetChunkNext),
        "get_chunk_prev" => Some(McpToolName::GetChunkPrev),
        "get_chunk_neighbors" => Some(McpToolName::GetChunkNeighbors),
        "get_chunk_parents" => Some(McpToolName::GetChunkParents),
        "get_document" => Some(McpToolName::GetDocument),
        "get_document_full" => Some(McpToolName::GetDocumentFull),
        "get_document_chunks" => Some(McpToolName::GetDocumentChunks),
        "list_sources" => Some(McpToolName::ListSources),
        "pull_models" => Some(McpToolName::PullModels),
        "status" => Some(McpToolName::Status),
        _ => None,
    }
}

async fn dispatch_tool_inner(
    id: RequestId,
    params: ToolCallParams,
    state: &ServerState,
) -> Response {
    let outcome = match params.name.as_str() {
        "status" => {
            let out = tools::run_status(Some(&state.cfg.cache_dir));
            Ok(serde_json::to_string(&out).unwrap_or_else(|e| format!("serialize status: {e}")))
        }
        "pull_models" => tools::run_pull_models(state.cfg.cache_dir.clone())
            .await
            .map(|out| serde_json::to_string(&out).unwrap_or_else(|e| format!("serialize: {e}")))
            .map_err(|msg| Response::err(id.clone(), ErrorCode::ToolFailed, msg)),
        "search" => run_search_dispatch(&id, &params, state).await,
        "get_chunk" => {
            run_passthrough_dispatch(&id, &params, state, tools::PassthroughKind::Chunk).await
        }
        "get_chunk_next" => {
            run_chunk_nav_dispatch(&id, &params, state, tools::ChunkNavDirection::Next).await
        }
        "get_chunk_prev" => {
            run_chunk_nav_dispatch(&id, &params, state, tools::ChunkNavDirection::Prev).await
        }
        "get_chunk_neighbors" => run_chunk_neighbors_dispatch(&id, &params, state).await,
        "get_chunk_parents" => {
            run_passthrough_dispatch(&id, &params, state, tools::PassthroughKind::Parents).await
        }
        "get_document" => {
            run_passthrough_dispatch(&id, &params, state, tools::PassthroughKind::Document).await
        }
        "get_document_full" => {
            run_passthrough_dispatch(&id, &params, state, tools::PassthroughKind::DocumentFull)
                .await
        }
        "get_document_chunks" => run_document_chunks_dispatch(&id, &params, state).await,
        "list_sources" => match state.cloud.list_sources().await {
            Ok(v) => Ok(v.to_string()),
            Err(CloudError::NotFound(msg)) => {
                Err(Response::err(id.clone(), ErrorCode::ToolFailed, format!("not found: {msg}")))
            }
            Err(e) => Err(Response::err(id.clone(), ErrorCode::ToolFailed, e.to_string())),
        },
        other => {
            return Response::err(id, ErrorCode::ToolNotFound, format!("unknown tool: {other}"));
        }
    };
    match outcome {
        Ok(result_text) => {
            let result = ToolCallResult {
                content: vec![ContentBlock::Text { text: result_text }],
                is_error: false,
            };
            Response::success(id, serde_json::to_value(result).expect("serialize result"))
        }
        Err(resp) => resp,
    }
}

async fn run_search_dispatch(
    id: &RequestId,
    params: &ToolCallParams,
    state: &ServerState,
) -> Result<String, Response> {
    match tools::run_search(&params.arguments, &state.cfg, &state.cloud).await {
        Ok(out) => Ok(out.to_string()),
        Err(tools::SearchError::InvalidInput(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::InvalidParams, msg))
        }
        Err(tools::SearchError::Mismatch {
            corpus_model,
            client_model,
            message,
            remediation,
        }) => Err(mismatch_response(
            id.clone(),
            &corpus_model,
            &client_model,
            &message,
            &remediation,
        )),
        Err(tools::SearchError::Cloud(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, msg))
        }
    }
}

async fn run_passthrough_dispatch(
    id: &RequestId,
    params: &ToolCallParams,
    state: &ServerState,
    kind: tools::PassthroughKind,
) -> Result<String, Response> {
    match tools::run_passthrough_id(&params.arguments, &state.cloud, kind).await {
        Ok(v) => Ok(v.to_string()),
        Err(tools::PassthroughError::InvalidInput(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::InvalidParams, msg))
        }
        Err(tools::PassthroughError::NotFound(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, format!("not found: {msg}")))
        }
        Err(tools::PassthroughError::TooManyChunks { chunk_count, cap, hint }) => {
            Err(too_many_chunks_response(id.clone(), chunk_count, cap, &hint))
        }
        Err(tools::PassthroughError::Cloud(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, msg))
        }
    }
}

async fn run_chunk_nav_dispatch(
    id: &RequestId,
    params: &ToolCallParams,
    state: &ServerState,
    dir: tools::ChunkNavDirection,
) -> Result<String, Response> {
    match tools::run_chunk_nav(&params.arguments, &state.cloud, dir).await {
        Ok(v) => Ok(v.to_string()),
        Err(tools::PassthroughError::InvalidInput(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::InvalidParams, msg))
        }
        Err(tools::PassthroughError::NotFound(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, format!("not found: {msg}")))
        }
        Err(tools::PassthroughError::TooManyChunks { .. }) => {
            // Not reachable for next/prev (no 412 on /next or /prev), but
            // exhaustively matched so the compiler catches additions.
            Err(Response::err(
                id.clone(),
                ErrorCode::ToolFailed,
                "unexpected too_many_chunks on /next or /prev".to_owned(),
            ))
        }
        Err(tools::PassthroughError::Cloud(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, msg))
        }
    }
}

async fn run_chunk_neighbors_dispatch(
    id: &RequestId,
    params: &ToolCallParams,
    state: &ServerState,
) -> Result<String, Response> {
    // NOTE: this is the fourth dispatch helper that follows the same
    // InvalidInput / NotFound / TooManyChunks / Cloud shape. A follow-up PR
    // will collapse all four into a single generic helper; keeping them
    // separate here keeps this PR focused on the new tool.
    match tools::run_chunk_neighbors(&params.arguments, &state.cloud).await {
        Ok(v) => Ok(v.to_string()),
        Err(tools::PassthroughError::InvalidInput(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::InvalidParams, msg))
        }
        Err(tools::PassthroughError::NotFound(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, format!("not found: {msg}")))
        }
        Err(tools::PassthroughError::TooManyChunks { .. }) => {
            // Not reachable: the cloud doesn't raise 412 on /:id, /:id/next,
            // or /:id/prev. Exhaustively matched so a future variant addition
            // fails the build instead of getting silently swallowed.
            Err(Response::err(
                id.clone(),
                ErrorCode::ToolFailed,
                "unexpected too_many_chunks on /neighbors".to_owned(),
            ))
        }
        Err(tools::PassthroughError::Cloud(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, msg))
        }
    }
}

async fn run_document_chunks_dispatch(
    id: &RequestId,
    params: &ToolCallParams,
    state: &ServerState,
) -> Result<String, Response> {
    match tools::run_document_chunks(&params.arguments, &state.cloud).await {
        Ok(v) => Ok(v.to_string()),
        Err(tools::PassthroughError::InvalidInput(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::InvalidParams, msg))
        }
        Err(tools::PassthroughError::NotFound(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, format!("not found: {msg}")))
        }
        Err(tools::PassthroughError::TooManyChunks { .. }) => Err(Response::err(
            id.clone(),
            ErrorCode::ToolFailed,
            "unexpected too_many_chunks on /chunks window".to_owned(),
        )),
        Err(tools::PassthroughError::Cloud(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, msg))
        }
    }
}

/// Build a JSON-RPC error response for the cloud's 409 embedding-model
/// mismatch, putting the corpus + client model in the `data` field so an AI
/// client can render a structured remediation (US5 acceptance #6).
fn mismatch_response(
    id: RequestId,
    corpus_model: &str,
    client_model: &str,
    message: &str,
    remediation: &str,
) -> Response {
    let data = serde_json::json!({
        "kind": "embedding_model_mismatch",
        "corpus_model": corpus_model,
        "client_model": client_model,
        "remediation": remediation,
        "next_tool": "pull_models",
    });
    Response {
        jsonrpc: JSONRPC,
        id,
        result: None,
        error: Some(JsonRpcError {
            code: ErrorCode::ToolFailed as i32,
            message: if message.is_empty() {
                format!("embedding model mismatch: corpus={corpus_model} client={client_model}")
            } else {
                message.to_owned()
            },
            data: Some(data),
        }),
    }
}

/// Build a JSON-RPC error response for the cloud's 412 `too_many_chunks`
/// body, putting the count + cap + hint in the `data` field so an AI client
/// can render a structured remediation (next_tool = "get_document_chunks").
fn too_many_chunks_response(id: RequestId, chunk_count: u32, cap: u32, hint: &str) -> Response {
    let data = serde_json::json!({
        "kind": "too_many_chunks",
        "chunk_count": chunk_count,
        "cap": cap,
        "hint": hint,
        "next_tool": "get_document_chunks",
    });
    Response {
        jsonrpc: JSONRPC,
        id,
        result: None,
        error: Some(JsonRpcError {
            code: ErrorCode::ToolFailed as i32,
            message: format!("document has {chunk_count} chunks (cap {cap})"),
            data: Some(data),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adding a tool to the manifest without adding the corresponding
    /// `McpToolName` arm here would silently drop telemetry. This test
    /// closes that loop: if `tools::list()` grows a name that
    /// `tool_name_for_event` can't translate, the build fails.
    #[test]
    fn every_manifest_tool_has_a_telemetry_name() {
        for tool in crate::tools::list().tools {
            assert!(
                tool_name_for_event(tool.name).is_some(),
                "tool `{}` is in the manifest but has no McpToolName mapping in \
                 tool_name_for_event — add the arm to keep telemetry coverage",
                tool.name,
            );
        }
    }
}

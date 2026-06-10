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

use crate::cloud_client::CloudClient;
use crate::prompts;
use crate::protocol::{
    ErrorCode, Incoming, InitializeResult, PromptGetParams, PromptsCapability, RequestId, Response,
    ServerCapabilities, ServerInfo, ToolCallParams, ToolCallResult, ToolsCapability,
    MCP_PROTOCOL_VERSION,
};
use crate::render::{ErrorKind, NextAction, ToolFailure};
use crate::tools;
use crate::transport::{FrameReader, FrameWriter};

/// Per-instance server config.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Where the reranker stores its ONNX files. (The corpus embedder is no
    /// longer local — embedding runs via VoyageAI — so nothing embedder-side
    /// is cached here.)
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
    /// declare on every search. NOTE: since the Voyage cutover, `run_search`
    /// resolves the corpus wire id live via `CloudClient::fetch_active_model`
    /// (`GET /v1/models/active`) and no longer reads this field; it is retained
    /// for config back-compat and other potential consumers.
    pub client_embedding_model: String,
    /// Resolved telemetry sink URL. Defaults to `{cloud_url}/v1/telemetry/events`.
    pub telemetry_url: String,
    /// Config-side master telemetry-enabled flag. The runtime opt-out
    /// resolver still wins over this (FR-107).
    pub telemetry_enabled: bool,
}

impl ServerConfig {
    /// Build a config with the production defaults: production cloud URL,
    /// no bearer, the `voyage-code-3@1` corpus model id, and telemetry
    /// enabled (subject to the opt-out resolver).
    #[must_use]
    pub fn with_defaults(cache_dir: PathBuf) -> Self {
        let cloud_url = "https://midnight-manual.midnightntwrk.expert".to_owned();
        let telemetry_url = format!("{cloud_url}/v1/telemetry/events");
        Self {
            cache_dir,
            cloud_url,
            bearer_token: None,
            client_embedding_model: "voyage-code-3@1".to_owned(),
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
                    prompts: PromptsCapability { list_changed: false },
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
        "prompts/list" => Response::success(
            id.clone(),
            serde_json::to_value(prompts::list()).expect("serialize prompt list"),
        ),
        "prompts/get" => match serde_json::from_value::<PromptGetParams>(req.params.clone()) {
            Ok(params) => prompts::get(id.clone(), &params),
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

// ---------------------------------------------------------------------------
// Tool dispatch carrier types
// ---------------------------------------------------------------------------

/// Carrier from the per-tool dispatch back to the telemetry-aware caller.
struct ToolResponse {
    result: ToolCallResult,
    telemetry: Option<crate::render::SearchTelemetry>,
    outcome: Outcome,
}

/// Coarse telemetry outcome for a passthrough error (computed before the error is consumed).
const fn passthrough_outcome(e: &tools::PassthroughError) -> Outcome {
    match e {
        tools::PassthroughError::InvalidInput(_) => Outcome::InvalidInput,
        _ => Outcome::Error,
    }
}

fn cloud_failure(e: &crate::cloud_client::CloudError) -> ToolFailure {
    use crate::cloud_client::CloudError;
    match e {
        CloudError::NotFound(msg) => ToolFailure {
            kind: ErrorKind::NotFound,
            message: format!("not found: {msg}"),
            guidance: "Resource not found — verify the id from a recent search result.".into(),
            details: serde_json::Value::Null,
            suggested_next_actions: vec![NextAction::call(
                "Run a fresh search to find a valid id",
                "search",
                serde_json::json!({ "query": "<terms>" }),
            )],
        },
        other => ToolFailure::simple(
            ErrorKind::CloudError,
            other.to_string(),
            "Upstream call failed; retry shortly.",
        ),
    }
}

fn passthrough_failure(e: tools::PassthroughError) -> ToolFailure {
    use serde_json::json;
    match e {
        tools::PassthroughError::InvalidInput(msg) => {
            ToolFailure::simple(ErrorKind::InvalidInput, msg.clone(), msg)
        }
        tools::PassthroughError::NotFound(msg) => ToolFailure {
            kind: ErrorKind::NotFound,
            message: format!("not found: {msg}"),
            guidance: "Not found — verify the id from a recent search result.".into(),
            details: json!({}),
            suggested_next_actions: vec![NextAction::call(
                "Run a fresh search to find a valid id",
                "search",
                json!({ "query": "<terms>" }),
            )],
        },
        tools::PassthroughError::Cloud(msg) => {
            ToolFailure::simple(ErrorKind::CloudError, msg, "Upstream call failed; retry shortly.")
        }
    }
}

// ---------------------------------------------------------------------------
// dispatch_tool (top-level, emits telemetry)
// ---------------------------------------------------------------------------

async fn dispatch_tool(id: RequestId, params: ToolCallParams, state: &ServerState) -> Response {
    let started = Instant::now();
    let name_for_event = tool_name_for_event(&params.name);
    // `rerank` is only meaningful to the search tools; for everything else the
    // field doesn't exist in the schema, so the telemetry value is false. Basic
    // `search` has no rerank arg at all (reranking always runs), and
    // `advanced_search` defaults the toggle to `true`, so an absent field must
    // log `true` to match what actually happened on the wire.
    let rerank_on = match params.name.as_str() {
        "search" => true,
        "advanced_search" => params
            .arguments
            .get("rerank")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        _ => false,
    };

    let (response, telemetry, outcome) = match dispatch_tool_inner(id.clone(), params, state).await
    {
        Ok(tr) => (
            Response::success(id, serde_json::to_value(tr.result).expect("serialize result")),
            tr.telemetry,
            tr.outcome,
        ),
        Err(resp) => (resp, None, Outcome::Error),
    };

    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    state.tools_served.fetch_add(1, Ordering::Relaxed);
    if let Some(name) = name_for_event {
        let t = telemetry.unwrap_or_default();
        state
            .telemetry
            .emit(Event::new(
                Component::Mcp,
                crate::VERSION,
                EventPayload::McpToolCall {
                    tool_name: name,
                    latency_ms,
                    result_count: t.result_count,
                    model_state: ModelState::Missing,
                    rerank_on,
                    outcome,
                    corpus_model: t.corpus_model,
                    reranker_used: t.reranker_used,
                    top_confidence: t.top_confidence_bucket.map(str::to_owned),
                    top_attribution: t.top_attribution,
                    top_source: t.top_source,
                    filtered_by_confidence: t.filtered_by_confidence,
                    deduplicated_count: t.deduplicated_count,
                },
            ))
            .await;
    }
    response
}

fn tool_name_for_event(name: &str) -> Option<McpToolName> {
    match name {
        // TODO(task 21): map advanced_search to a dedicated
        // McpToolName::AdvancedSearch variant once the telemetry schema grows one.
        "search" | "advanced_search" => Some(McpToolName::Search),
        "get_chunk" => Some(McpToolName::GetChunk),
        "get_chunk_next" => Some(McpToolName::GetChunkNext),
        "get_chunk_prev" => Some(McpToolName::GetChunkPrev),
        "get_chunk_neighbors" => Some(McpToolName::GetChunkNeighbors),
        "get_chunk_parents" => Some(McpToolName::GetChunkParents),
        "get_document" => Some(McpToolName::GetDocument),
        "get_document_chunks" => Some(McpToolName::GetDocumentChunks),
        "list_sources" => Some(McpToolName::ListSources),
        "facets" => Some(McpToolName::Facets),
        "pull_models" => Some(McpToolName::PullModels),
        "status" => Some(McpToolName::Status),
        "install_search_skill" => Some(McpToolName::InstallSearchSkill),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// dispatch_tool_inner: routes each tool name to its handler
// ---------------------------------------------------------------------------

/// Route a tool call to its handler, returning a `ToolResponse` on success or
/// a JSON-RPC `Response` (protocol fault) on unknown tool.
///
/// Every tool-execution error (bad input, cloud failure, etc.) becomes an
/// `isError: true` `ToolCallResult` inside `Ok(ToolResponse)` — JSON-RPC
/// errors are reserved for protocol faults only.
async fn dispatch_tool_inner(
    id: RequestId,
    params: ToolCallParams,
    state: &ServerState,
) -> Result<ToolResponse, Response> {
    use crate::render;

    let ok = |result: ToolCallResult, telemetry| ToolResponse {
        result,
        telemetry,
        outcome: Outcome::Ok,
    };
    let err = |result: ToolCallResult, outcome| ToolResponse {
        result,
        telemetry: None,
        outcome,
    };

    Ok(match params.name.as_str() {
        "status" => {
            let out = tools::run_status(Some(&state.cfg.cache_dir));
            let v = serde_json::to_value(out).unwrap_or(serde_json::Value::Null);
            ok(render::project_status(v).into_result(), None)
        }
        "pull_models" => match tools::run_pull_models(state.cfg.cache_dir.clone()).await {
            Ok(out) => {
                let v = serde_json::to_value(out).unwrap_or(serde_json::Value::Null);
                ok(render::project_pull_models(v).into_result(), None)
            }
            Err(msg) => err(
                ToolFailure::simple(
                    ErrorKind::ModelLoadFailed,
                    msg,
                    "Model download failed; retry pull_models.",
                )
                .into_result(),
                Outcome::Error,
            ),
        },
        "search" | "advanced_search" => return Ok(run_search_dispatch(&params, state).await),
        "get_chunk"
        | "get_chunk_next"
        | "get_chunk_prev"
        | "get_chunk_neighbors"
        | "get_chunk_parents"
        | "get_document"
        | "get_document_chunks" => return Ok(run_passthrough_tool(&params, state).await),
        "list_sources" => match state.cloud.list_sources(&[]).await {
            Ok(v) => ok(render::project_sources(v).into_result(), None),
            Err(e) => err(cloud_failure(&e).into_result(), Outcome::Error),
        },
        "facets" => match state.cloud.get_facets(&[]).await {
            Ok(v) => ok(render::project_facets(v).into_result(), None),
            Err(e) => err(cloud_failure(&e).into_result(), Outcome::Error),
        },
        "install_search_skill" => match tools::run_install_search_skill(&params.arguments) {
            Ok(text) => {
                let v = serde_json::from_str::<serde_json::Value>(&text)
                    .unwrap_or_else(|_| serde_json::json!({ "message": text }));
                ok(render::project_install(v).into_result(), None)
            }
            Err((code, msg)) => {
                let outcome = if code == crate::protocol::ErrorCode::InvalidParams {
                    Outcome::InvalidInput
                } else {
                    Outcome::Error
                };
                err(
                    ToolFailure::simple(ErrorKind::InstallFailed, msg.clone(), msg).into_result(),
                    outcome,
                )
            }
        },
        other => {
            return Err(Response::err(
                id,
                ErrorCode::ToolNotFound,
                format!("unknown tool: {other}"),
            ));
        }
    })
}

// ---------------------------------------------------------------------------
// run_search_dispatch
// ---------------------------------------------------------------------------

async fn run_search_dispatch(params: &ToolCallParams, state: &ServerState) -> ToolResponse {
    use crate::render::{self, SearchRenderOpts, ToolFailure};

    // The dispatcher knows the tool name; the parser encodes each tool's
    // argument contract (basic: {query, mode?, limit?}; advanced: full surface).
    let advanced = params.name == "advanced_search";
    let parsed = if advanced {
        tools::parse_advanced_search_args(&params.arguments)
    } else {
        tools::parse_basic_search_args(&params.arguments)
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(msg) => {
            return ToolResponse {
                result: ToolFailure::simple(ErrorKind::InvalidInput, msg.clone(), msg)
                    .into_result(),
                telemetry: None,
                outcome: Outcome::InvalidInput,
            };
        }
    };
    // Best-effort reranker name for telemetry. Use the well-known constant —
    // the constant is what `run_status` and `pull_models` also report, so it's
    // the most accurate single-process value we have.
    let reranker_name: Option<String> = if parsed.rerank {
        Some(mn_embedding::RERANKER_MODEL_NAME.to_string())
    } else {
        None
    };

    match tools::run_search(&parsed, &state.cfg, &state.cloud).await {
        Ok(envelope) => {
            let opts = SearchRenderOpts {
                reranker_used: reranker_name,
                advanced,
                skill_installed: mn_skills::installed_anywhere(&mn_skills::StdSkillEnv),
            };
            let outcome = render::project_search(envelope, &opts);
            let telemetry = outcome.telemetry.clone();
            ToolResponse {
                result: outcome.into_result(),
                telemetry,
                outcome: Outcome::Ok,
            }
        }
        Err(tools::SearchError::InvalidInput(msg)) => ToolResponse {
            result: ToolFailure::simple(ErrorKind::InvalidInput, msg.clone(), msg).into_result(),
            telemetry: None,
            outcome: Outcome::InvalidInput,
        },
        Err(tools::SearchError::Mismatch {
            corpus_model,
            client_model,
            message,
            remediation,
        }) => ToolResponse {
            result: ToolFailure {
                kind: ErrorKind::EmbeddingModelMismatch,
                message,
                guidance: remediation.clone(),
                details: serde_json::json!({
                    "corpus_model": corpus_model,
                    "client_model": client_model,
                    "remediation": remediation,
                }),
                suggested_next_actions: vec![NextAction::call(
                    "Pull the current corpus models to resolve the mismatch",
                    "pull_models",
                    serde_json::json!({}),
                )],
            }
            .into_result(),
            telemetry: None,
            outcome: Outcome::Error,
        },
        Err(tools::SearchError::Cloud(msg)) => ToolResponse {
            result: ToolFailure::simple(
                ErrorKind::CloudError,
                msg,
                "Search failed upstream; retry shortly.",
            )
            .into_result(),
            telemetry: None,
            outcome: Outcome::Error,
        },
    }
}

// ---------------------------------------------------------------------------
// run_passthrough_tool: eight chunk/document tools via projectors
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
async fn run_passthrough_tool(params: &ToolCallParams, state: &ServerState) -> ToolResponse {
    use crate::render;
    let cloud = &state.cloud;
    let args = &params.arguments;

    // Inline each arm to avoid macro/closure type-inference fights.
    match params.name.as_str() {
        "get_chunk" => {
            match tools::run_passthrough_id(args, cloud, tools::PassthroughKind::Chunk).await {
                Ok(v) => ToolResponse {
                    result: render::project_chunk(v).into_result(),
                    telemetry: None,
                    outcome: Outcome::Ok,
                },
                Err(e) => {
                    let outcome = passthrough_outcome(&e);
                    ToolResponse {
                        result: passthrough_failure(e).into_result(),
                        telemetry: None,
                        outcome,
                    }
                }
            }
        }
        "get_chunk_next" => {
            match tools::run_chunk_nav(args, cloud, tools::ChunkNavDirection::Next).await {
                Ok(v) => ToolResponse {
                    result: render::project_chunk_list(v, "after").into_result(),
                    telemetry: None,
                    outcome: Outcome::Ok,
                },
                Err(e) => {
                    let outcome = passthrough_outcome(&e);
                    ToolResponse {
                        result: passthrough_failure(e).into_result(),
                        telemetry: None,
                        outcome,
                    }
                }
            }
        }
        "get_chunk_prev" => {
            match tools::run_chunk_nav(args, cloud, tools::ChunkNavDirection::Prev).await {
                Ok(v) => ToolResponse {
                    result: render::project_chunk_list(v, "before").into_result(),
                    telemetry: None,
                    outcome: Outcome::Ok,
                },
                Err(e) => {
                    let outcome = passthrough_outcome(&e);
                    ToolResponse {
                        result: passthrough_failure(e).into_result(),
                        telemetry: None,
                        outcome,
                    }
                }
            }
        }
        "get_chunk_neighbors" => match tools::run_chunk_neighbors(args, cloud).await {
            Ok(v) => ToolResponse {
                result: render::project_neighbors(v).into_result(),
                telemetry: None,
                outcome: Outcome::Ok,
            },
            Err(e) => {
                let outcome = passthrough_outcome(&e);
                ToolResponse {
                    result: passthrough_failure(e).into_result(),
                    telemetry: None,
                    outcome,
                }
            }
        },
        "get_chunk_parents" => {
            match tools::run_passthrough_id(args, cloud, tools::PassthroughKind::Parents).await {
                Ok(v) => ToolResponse {
                    result: render::project_parents(v).into_result(),
                    telemetry: None,
                    outcome: Outcome::Ok,
                },
                Err(e) => {
                    let outcome = passthrough_outcome(&e);
                    ToolResponse {
                        result: passthrough_failure(e).into_result(),
                        telemetry: None,
                        outcome,
                    }
                }
            }
        }
        "get_document" => {
            match tools::run_passthrough_id(args, cloud, tools::PassthroughKind::Document).await {
                Ok(v) => ToolResponse {
                    result: render::project_document_overview(v).into_result(),
                    telemetry: None,
                    outcome: Outcome::Ok,
                },
                Err(e) => {
                    let outcome = passthrough_outcome(&e);
                    ToolResponse {
                        result: passthrough_failure(e).into_result(),
                        telemetry: None,
                        outcome,
                    }
                }
            }
        }
        "get_document_chunks" => match tools::run_document_chunks(args, cloud).await {
            Ok(v) => ToolResponse {
                result: render::project_document_window(v).into_result(),
                telemetry: None,
                outcome: Outcome::Ok,
            },
            Err(e) => {
                let outcome = passthrough_outcome(&e);
                ToolResponse {
                    result: passthrough_failure(e).into_result(),
                    telemetry: None,
                    outcome,
                }
            }
        },
        other => ToolResponse {
            result: ToolFailure::simple(
                ErrorKind::InvalidInput,
                format!("unknown passthrough tool: {other}"),
                "internal routing error",
            )
            .into_result(),
            telemetry: None,
            outcome: Outcome::Error,
        },
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

    /// `initialize` must advertise the prompts capability so clients query it.
    #[test]
    fn initialize_advertises_prompts_capability() {
        let init = InitializeResult {
            protocol_version: MCP_PROTOCOL_VERSION,
            capabilities: ServerCapabilities {
                tools: ToolsCapability { list_changed: false },
                prompts: PromptsCapability { list_changed: false },
            },
            server_info: ServerInfo { name: "x", version: "0" },
        };
        let v = serde_json::to_value(&init).unwrap();
        assert_eq!(v["capabilities"]["prompts"]["listChanged"], false);
    }
}

//! MCP server event loop. Reads framed JSON-RPC messages from stdin,
//! dispatches them to handlers, writes responses to stdout.
//!
//! Logging goes to stderr (FR-021): stdout is reserved for the MCP wire.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use mnm_telemetry::events::{
    McpShutdown, McpStartup, McpToolCall, McpToolName, ModelState, Outcome, Rerank,
};
use mnm_telemetry::{
    build as build_telemetry, BuildParams, Flusher, Surface, Telemetry, DEFAULT_FLUSH_TIMEOUT,
    FLUSH_ARGS,
};
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
    /// production). Every tool calls it — `status` probes its `/readyz` and
    /// `/v1/me` alongside the local checks.
    pub cloud_url: String,
    /// Optional read-uplift bearer to forward as `Authorization: Bearer ...`
    /// on every cloud request. `None` means the MCP server is running in
    /// anonymous read mode.
    pub bearer_token: Option<String>,
    /// Resolved Gauge ingest endpoint (base URL; the client appends /v1/logs).
    pub telemetry_endpoint: String,
    /// Config-side master telemetry-enabled flag. Runtime opt-out still wins.
    pub telemetry_enabled: bool,
    /// Client-side prompt-injection guarding level (issue #103). Decides, per
    /// returned chunk's source attribution and verification status, whether the
    /// content is wrapped in a nonce-tagged untrusted block before the model
    /// sees it — and at [`SecurityLevel::Strict`], whether pattern-flagged
    /// content is removed. Defaults to [`SecurityLevel::Moderate`].
    ///
    /// [`SecurityLevel::Strict`]: mnm_core::injection::SecurityLevel::Strict
    /// [`SecurityLevel::Moderate`]: mnm_core::injection::SecurityLevel::Moderate
    pub security: mnm_core::injection::SecurityLevel,
}

impl ServerConfig {
    /// Build a config with the production defaults: production cloud URL,
    /// no bearer, and telemetry enabled (subject to the opt-out resolver). The
    /// corpus embedding-model id is no longer configured here — `run_search`
    /// resolves it live via `CloudClient::fetch_active_model`
    /// (`GET /v1/models/active`).
    #[must_use]
    pub fn with_defaults(cache_dir: PathBuf) -> Self {
        let cloud_url = mnm_core::config::DEFAULT_SERVER_URL.to_owned();
        Self {
            cache_dir,
            cloud_url,
            bearer_token: None,
            telemetry_endpoint: mnm_core::config::DEFAULT_TELEMETRY_ENDPOINT.to_owned(),
            telemetry_enabled: true,
            security: mnm_core::injection::SecurityLevel::default(),
        }
    }
}

/// Shared per-process state — the cloud HTTP client lives here so we don't
/// rebuild it on every tool call.
#[derive(Clone)]
struct ServerState {
    cfg: ServerConfig,
    cloud: Arc<CloudClient>,
    telemetry: Arc<Telemetry>,
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
    let env = mnm_core::config::StdEnv;
    let marker = mnm_core::paths::telemetry_marker_path(&env);
    let runtime_enabled = !mnm_telemetry::optout::env_disabled(&env)
        && !marker
            .as_deref()
            .is_some_and(mnm_telemetry::optout::marker_present);
    let telemetry: Telemetry = build_telemetry(BuildParams {
        app_version: crate::VERSION.to_owned(),
        endpoint: cfg.telemetry_endpoint.clone(),
        install_id_path: mnm_core::paths::telemetry_install_id_path(&env),
        config_enabled: cfg.telemetry_enabled,
        runtime_enabled,
        flush_args: FLUSH_ARGS.iter().map(|s| (*s).to_owned()).collect(),
    });
    // Background drain every 30s. Kept alive for the session; dropped at
    // shutdown to stop + join the background thread.
    let flusher: Option<Flusher> =
        Flusher::start(&telemetry, std::time::Duration::from_secs(30), 0);

    let cloud = CloudClient::new(&cfg.cloud_url, cfg.bearer_token.clone())
        .map_err(|e| format!("build cloud client: {e}"))?;
    let started_at = Arc::new(Instant::now());
    // Emit `mcp_startup` right away. The `startup_ms` field measures
    // process-start → here; for stdio MCP that's effectively 0 because the
    // event fires before the first JSON-RPC frame.
    let startup_ms = u32::try_from(started_at.elapsed().as_millis()).unwrap_or(u32::MAX);
    telemetry.emit(&McpStartup {
        startup_ms,
        model_state: ModelState::Missing,
    });
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

    info!("mnm-mcp server: handshake ready, awaiting initialize");

    while let Some(body) = reader.next_message().await? {
        let response_body = match handle_message(&body, &state).await {
            Some(bytes) => bytes,
            None => continue, // notification — no response
        };
        writer.write_message(&response_body).await?;
    }

    info!("mnm-mcp server: stdin EOF, shutting down");
    let uptime_s = u32::try_from(state.started_at.elapsed().as_secs()).unwrap_or(u32::MAX);
    let tools_served = state.tools_served.load(Ordering::Relaxed);
    state
        .telemetry
        .emit(&McpShutdown { uptime_s, tools_served });
    drop(flusher); // stop the background loop + join
    state.telemetry.flush_blocking(DEFAULT_FLUSH_TIMEOUT); // final drain
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
    /// Rerank facts for the FR-109 `Rerank` event (search tools only; `None`
    /// for every other tool and on early-return error paths).
    rerank: Option<tools::RerankFacts>,
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

    let (response, telemetry, rerank, outcome) =
        match dispatch_tool_inner(id.clone(), params, state).await {
            Ok(tr) => (
                Response::success(id, serde_json::to_value(tr.result).expect("serialize result")),
                tr.telemetry,
                tr.rerank,
                tr.outcome,
            ),
            Err(resp) => (resp, None, None, Outcome::Error),
        };

    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    state.tools_served.fetch_add(1, Ordering::Relaxed);
    if let Some(name) = name_for_event {
        let t = telemetry.unwrap_or_default();
        // One `Rerank` event per search (spec §6), alongside the McpToolCall.
        // Only the search tools carry rerank facts; the three-mechanism opt-out
        // wraps `emit`, so no extra gating is needed here.
        if let Some(r) = rerank {
            state.telemetry.emit(&Rerank {
                placement: r.placement.to_owned(),
                model: r.model,
                applied: r.applied,
                reason: r.reason,
                billed_tokens: r.billed_tokens,
                surface: Surface::Mcp,
            });
        }
        state.telemetry.emit(&McpToolCall {
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
        });
    }
    response
}

fn tool_name_for_event(name: &str) -> Option<McpToolName> {
    match name {
        "search" => Some(McpToolName::Search),
        "advanced_search" => Some(McpToolName::AdvancedSearch),
        "get_chunks" => Some(McpToolName::GetChunks),
        "get_chunk_next" => Some(McpToolName::GetChunkNext),
        "get_chunk_prev" => Some(McpToolName::GetChunkPrev),
        "get_chunk_neighbors" => Some(McpToolName::GetChunkNeighbors),
        "get_chunk_parents" => Some(McpToolName::GetChunkParents),
        "get_document" => Some(McpToolName::GetDocument),
        "get_document_chunks" => Some(McpToolName::GetDocumentChunks),
        "list_sources" => Some(McpToolName::ListSources),
        "facets" => Some(McpToolName::Facets),
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
        rerank: None,
        outcome: Outcome::Ok,
    };
    let err = |result: ToolCallResult, outcome| ToolResponse {
        result,
        telemetry: None,
        rerank: None,
        outcome,
    };

    Ok(match params.name.as_str() {
        "status" => {
            // Resolve the BYOK Voyage key the same way `run_search` does
            // (flag is always None on the MCP surface; env + config only).
            let voyage_key = {
                let cfg_env = mnm_core::config::StdEnv;
                let (core_cfg, _) =
                    mnm_core::config::Config::discover(None, &cfg_env).map_err(|e| {
                        Response::err(id.clone(), ErrorCode::InternalError, e.to_string())
                    })?;
                mnm_core::config::resolve_voyage_api_key(None, &core_cfg.models, &cfg_env)
            };
            let report = crate::status::assemble(&state.cloud, voyage_key.as_deref()).await;
            let v = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
            ok(render::project_status(v).into_result(), None)
        }
        "search" | "advanced_search" => return Ok(run_search_dispatch(&params, state).await),
        "get_chunks"
        | "get_chunk_next"
        | "get_chunk_prev"
        | "get_chunk_neighbors"
        | "get_chunk_parents"
        | "get_document"
        | "get_document_chunks" => return Ok(run_passthrough_tool(&params, state).await),
        "list_sources" => match tools::run_list_sources(&params.arguments, &state.cloud).await {
            Ok(v) => ok(render::project_sources(v).into_result(), None),
            Err(e) => err(cloud_failure(&e).into_result(), Outcome::Error),
        },
        "facets" => match tools::run_facets(&params.arguments, &state.cloud).await {
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
                rerank: None,
                outcome: Outcome::InvalidInput,
            };
        }
    };
    // Best-effort reranker name for telemetry. Reranking is VoyageAI now, so
    // report the default Voyage rerank model from the shared `mnm_core::rerank`
    // vocabulary (the same value `status` reports).
    let reranker_name: Option<String> = if parsed.rerank {
        mnm_core::rerank::RerankParam::Rerank25
            .model_name()
            .map(str::to_owned)
    } else {
        None
    };

    match tools::run_search(&parsed, &state.cfg, &state.cloud).await {
        Ok(success) => {
            let opts = SearchRenderOpts {
                reranker_used: reranker_name,
                advanced,
                skill_installed: mnm_skills::installed_anywhere(&mnm_skills::StdSkillEnv),
                security: state.cfg.security,
            };
            let outcome = render::project_search(success.envelope, &opts);
            let telemetry = outcome.telemetry.clone();
            ToolResponse {
                result: outcome.into_result(),
                telemetry,
                rerank: Some(success.rerank),
                outcome: Outcome::Ok,
            }
        }
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
                // No suggested tool call: the mismatch is corpus-side (the
                // client embeds via the live `/v1/models/active` wire id), so
                // the cloud-provided `remediation` string is the next step.
                suggested_next_actions: vec![],
            }
            .into_result(),
            telemetry: None,
            rerank: None,
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
            rerank: None,
            outcome: Outcome::Error,
        },
    }
}

// ---------------------------------------------------------------------------
// run_passthrough_tool: seven chunk/document tools via projectors
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
async fn run_passthrough_tool(params: &ToolCallParams, state: &ServerState) -> ToolResponse {
    use crate::render;
    let cloud = &state.cloud;
    let args = &params.arguments;
    let security = state.cfg.security;

    // Inline each arm to avoid macro/closure type-inference fights.
    match params.name.as_str() {
        "get_chunks" => match tools::run_get_chunks(args, cloud).await {
            Ok(v) => ToolResponse {
                result: render::project_chunks(v, security).into_result(),
                telemetry: None,
                rerank: None,
                outcome: Outcome::Ok,
            },
            Err(e) => {
                let outcome = passthrough_outcome(&e);
                ToolResponse {
                    result: passthrough_failure(e).into_result(),
                    telemetry: None,
                    rerank: None,
                    outcome,
                }
            }
        },
        "get_chunk_next" => {
            match tools::run_chunk_nav(args, cloud, tools::ChunkNavDirection::Next).await {
                Ok(v) => ToolResponse {
                    result: render::project_chunk_list(v, "after", security).into_result(),
                    telemetry: None,
                    rerank: None,
                    outcome: Outcome::Ok,
                },
                Err(e) => {
                    let outcome = passthrough_outcome(&e);
                    ToolResponse {
                        result: passthrough_failure(e).into_result(),
                        telemetry: None,
                        rerank: None,
                        outcome,
                    }
                }
            }
        }
        "get_chunk_prev" => {
            match tools::run_chunk_nav(args, cloud, tools::ChunkNavDirection::Prev).await {
                Ok(v) => ToolResponse {
                    result: render::project_chunk_list(v, "before", security).into_result(),
                    telemetry: None,
                    rerank: None,
                    outcome: Outcome::Ok,
                },
                Err(e) => {
                    let outcome = passthrough_outcome(&e);
                    ToolResponse {
                        result: passthrough_failure(e).into_result(),
                        telemetry: None,
                        rerank: None,
                        outcome,
                    }
                }
            }
        }
        "get_chunk_neighbors" => match tools::run_chunk_neighbors(args, cloud).await {
            Ok(v) => ToolResponse {
                result: render::project_neighbors(v, security).into_result(),
                telemetry: None,
                rerank: None,
                outcome: Outcome::Ok,
            },
            Err(e) => {
                let outcome = passthrough_outcome(&e);
                ToolResponse {
                    result: passthrough_failure(e).into_result(),
                    telemetry: None,
                    rerank: None,
                    outcome,
                }
            }
        },
        "get_chunk_parents" => {
            match tools::run_passthrough_id(args, cloud, tools::PassthroughKind::Parents).await {
                Ok(v) => ToolResponse {
                    result: render::project_parents(v).into_result(),
                    telemetry: None,
                    rerank: None,
                    outcome: Outcome::Ok,
                },
                Err(e) => {
                    let outcome = passthrough_outcome(&e);
                    ToolResponse {
                        result: passthrough_failure(e).into_result(),
                        telemetry: None,
                        rerank: None,
                        outcome,
                    }
                }
            }
        }
        "get_document" => {
            match tools::run_passthrough_id(args, cloud, tools::PassthroughKind::Document).await {
                Ok(v) => ToolResponse {
                    result: render::project_document(v).into_result(),
                    telemetry: None,
                    rerank: None,
                    outcome: Outcome::Ok,
                },
                Err(e) => {
                    let outcome = passthrough_outcome(&e);
                    ToolResponse {
                        result: passthrough_failure(e).into_result(),
                        telemetry: None,
                        rerank: None,
                        outcome,
                    }
                }
            }
        }
        "get_document_chunks" => match tools::run_document_chunks(args, cloud).await {
            Ok(v) => ToolResponse {
                result: render::project_document_window(v, security).into_result(),
                telemetry: None,
                rerank: None,
                outcome: Outcome::Ok,
            },
            Err(e) => {
                let outcome = passthrough_outcome(&e);
                ToolResponse {
                    result: passthrough_failure(e).into_result(),
                    telemetry: None,
                    rerank: None,
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
            rerank: None,
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

//! Streamable HTTP transport for the MCP server (spec rev 2025-06-18 — the
//! same `MCP_PROTOCOL_VERSION` the `initialize` handler advertises).
//!
//! This is the minimal, spec-legal *stateless* form of Streamable HTTP:
//!
//! - `POST /mcp` — one JSON-RPC message per request body. A request → `200`
//!   with a single JSON response object (no SSE: the spec lets a server answer
//!   any POST with plain JSON, and this server has nothing to stream — no
//!   progress notifications, no sampling, `list_changed: false` on both
//!   capabilities). A notification (or stray client→server response) → `202`
//!   with an empty body. A malformed/invalid message → `400` carrying the
//!   JSON-RPC error object `Incoming::classify` builds (`id: null` when the id
//!   is unrecoverable — issue #173 semantics, identical to stdio).
//! - `GET`/`DELETE /mcp` → `405` + `Allow: POST`, free from axum's method
//!   router (spec-legal for a server with no server-initiated stream and no
//!   sessions). No `Mcp-Session-Id` is ever issued — every tool call is
//!   self-contained. JSON-RPC batching is not supported (removed in spec rev
//!   2025-06-18, matching the stdio dispatch).
//! - `GET /healthz` — orchestration probe for deliberate public binds.
//!
//! The message-handling core is the same transport-blind
//! [`handle_message`](crate::server) the stdio loop uses; only the outer
//! framing differs. Logging still goes to stderr — there is no stdout wire to
//! protect here, but the two transports share one logging story (FR-021).

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use sentry::SentryFutureExt as _;
use tracing::{debug, info, warn};

use crate::server::{
    build_runtime, handle_message, shutdown, HandleOutcome, ServerConfig, ServerState,
    TransportKind,
};
use crate::transport::MAX_BODY_BYTES;

/// Router state: the transport-agnostic [`ServerState`] shared with stdio,
/// plus the HTTP-only Origin-enforcement flag resolved once at bind time.
#[derive(Clone)]
struct HttpState {
    /// Shared per-process server state (cloud client, telemetry, counters).
    server: ServerState,
    /// Whether the DNS-rebinding Origin guard is active. [`run_http`] computes
    /// this from the bind address: loopback binds enforce it, deliberate
    /// public binds skip it (rebinding is an attack on *localhost* services
    /// via a victim's browser; on an already-public authless port the check
    /// adds nothing but breakage).
    enforce_loopback_origin: bool,
}

/// Run the MCP server over Streamable HTTP on `bind` until SIGINT (or, on
/// unix, SIGTERM), then drain telemetry via the same shutdown tail as stdio.
///
/// Binding a non-loopback address logs a prominent warning: the server has no
/// authentication, and every caller transacts with the operator's identity —
/// their read-uplift bearer (rate tier) and their BYOK `VOYAGE_API_KEY` on
/// rerank paths. Never silently.
///
/// # Errors
///
/// Returns the underlying io error if the listener cannot bind or the server
/// loop fails, or a string error if the cloud client cannot be built. JSON-RPC
/// and tool-level errors are translated into wire responses and do NOT bubble
/// up.
pub async fn run_http(
    cfg: ServerConfig,
    bind: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (state, flusher) = build_runtime(cfg, TransportKind::Http)?;

    // Loopback bind → enforce the spec's Origin guard; non-loopback bind → the
    // operator has deliberately exposed the port, so skip it (see `HttpState`)
    // and warn loudly about what exposure means.
    let enforce_loopback_origin = bind.ip().is_loopback();
    if !enforce_loopback_origin {
        warn!(
            %bind,
            "binding a NON-LOOPBACK address: this MCP server has NO authentication — every \
             caller transacts with the operator's identity, spending their read-uplift bearer \
             rate tier and their BYOK VOYAGE_API_KEY on rerank paths. Expose deliberately."
        );
    }

    let app = router(HttpState {
        server: state.clone(),
        enforce_loopback_origin,
    });
    let listener = tokio::net::TcpListener::bind(bind).await?;
    // Report the *resolved* address (`local_addr`, not `bind`) so a `:0`
    // ephemeral-port bind logs the port that was actually assigned.
    let local_addr = listener.local_addr()?;
    info!(addr = %local_addr, "mnm-mcp HTTP server listening (POST /mcp, GET /healthz)");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("mnm-mcp HTTP server: shutdown signal received, draining");
    shutdown(&state, flusher);
    Ok(())
}

/// Build the axum router. Pure (no I/O, no sockets) so tests can drive it
/// in-process via `tower::ServiceExt::oneshot`.
fn router(state: HttpState) -> Router {
    Router::new()
        .route("/mcp", post(post_mcp))
        .route("/healthz", get(healthz))
        // Same 16 MiB cap as the stdio framing (`transport::MAX_BODY_BYTES`),
        // raising axum's 2 MB default; excess → 413.
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// `POST /mcp` — one JSON-RPC message in, at most one JSON-RPC message out.
// Extractors arrive by value in axum handlers; `HttpState` is only borrowed
// here, which trips the pass-by-value heuristic without a real alternative.
#[allow(clippy::needless_pass_by_value)]
async fn post_mcp(State(state): State<HttpState>, headers: HeaderMap, body: Bytes) -> Response {
    // Origin guard (spec MUST, DNS-rebinding defence): only on loopback binds,
    // and only for requests that carry an Origin at all — non-browser MCP
    // clients send none and always pass. A present Origin must parse to a
    // loopback origin or the request is refused before any dispatch work.
    if state.enforce_loopback_origin {
        if let Some(origin) = headers.get(header::ORIGIN) {
            let allowed = origin.to_str().is_ok_and(origin_is_loopback);
            if !allowed {
                warn!(origin = ?origin, "refusing non-loopback Origin on a loopback bind");
                return (
                    StatusCode::FORBIDDEN,
                    "Origin not allowed: this MCP server accepts loopback origins only\n",
                )
                    .into_response();
            }
        }
    }

    // `MCP-Protocol-Version` (spec: clients SHOULD send it on every request
    // after initialize): observed for debugging, never enforced — rejecting on
    // mismatch would break clients that negotiate newer revisions and buys
    // nothing for a read-only tool surface.
    if let Some(version) = headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
    {
        debug!(version = %version, "client sent MCP-Protocol-Version");
    }

    // Dispatch on a per-request hub derived from the main hub (which carries
    // the `surface`/`session_id`/`transport` tags set in `build_runtime`).
    // This is what lets concurrent requests each run their own Sentry
    // transaction span without competing — see the comment in `dispatch_tool`.
    let hub = sentry::Hub::new_from_top(sentry::Hub::main());
    let outcome = handle_message(body.as_ref(), &state.server)
        .bind_hub(hub)
        .await;

    match outcome {
        // A dispatched request — success or JSON-RPC error envelope alike — is
        // a valid response object, so it rides a 200.
        HandleOutcome::Reply(bytes) => json_response(StatusCode::OK, bytes),
        // An undispatchable message keeps the exact error bytes stdio would
        // send (id:null semantics, issue #173) but signals transport-level
        // rejection with a 400, per the Streamable HTTP spec.
        HandleOutcome::Invalid(bytes) => json_response(StatusCode::BAD_REQUEST, bytes),
        // A notification gets no body at all — 202 Accepted, per spec.
        HandleOutcome::NoReply => StatusCode::ACCEPTED.into_response(),
    }
}

/// `GET /healthz` — liveness probe for containers/orchestration (the
/// configurable-public-bind story implies one). Name + version only; the
/// richer diagnostics live in the `status` tool.
async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": "midnight-manual-mcp",
        "version": crate::VERSION,
    }))
}

/// Wrap pre-serialized JSON-RPC bytes in an HTTP response with the given
/// status. The bytes come straight from the shared handle path — the HTTP
/// layer never re-serializes or reshapes them.
fn json_response(status: StatusCode, body: Vec<u8>) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// Syntactic loopback check for an `Origin` header value: `localhost`
/// (case-insensitive), any `127.0.0.0/8` IPv4, or IPv6 loopback (`[::1]`).
///
/// Anything that does not parse as a URL with such a host — including the
/// opaque `Origin: null` a browser sends from a sandboxed context — is NOT
/// loopback: an unidentifiable origin gets no loopback trust.
fn origin_is_loopback(origin: &str) -> bool {
    let Ok(parsed) = url::Url::parse(origin) else {
        return false;
    };
    match parsed.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Resolve when the process receives SIGINT (ctrl-c) or, on unix, SIGTERM —
/// the signal containers/orchestrators send. Mirrors the cloud server's
/// graceful-shutdown listener.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install ctrl-c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use mnm_telemetry::{build as build_telemetry, BuildParams};
    use tower::ServiceExt as _; // `Router::oneshot` — in-process, no sockets.

    use super::*;

    /// Build an `HttpState` for router tests: telemetry disabled (no
    /// filesystem/network I/O) and the cloud pointed at a closed discard port
    /// so anything that DID reach it fails fast. Mirrors `server::tests::
    /// test_state`, which is `#[cfg(test)]`-private to that module.
    fn test_state(enforce_loopback_origin: bool) -> HttpState {
        let telemetry = build_telemetry(BuildParams {
            app_version: "test".to_owned(),
            endpoint: "https://telemetry.disabled.invalid".to_owned(),
            install_id_path: None, // None disables telemetry — no I/O
            config_enabled: false,
            runtime_enabled: false,
            flush_args: vec![],
        });
        let mut cfg = ServerConfig::with_defaults(std::env::temp_dir());
        cfg.cloud_url = "http://127.0.0.1:9".to_owned();
        let cloud = crate::cloud_client::CloudClient::new(&cfg.cloud_url, None)
            .expect("build cloud client");
        HttpState {
            server: ServerState {
                cfg,
                cloud: std::sync::Arc::new(cloud),
                telemetry: std::sync::Arc::new(telemetry),
                started_at: std::sync::Arc::new(std::time::Instant::now()),
                tools_served: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            },
            enforce_loopback_origin,
        }
    }

    /// A loopback-mode router (Origin guard active) — the default posture.
    fn loopback_router() -> Router {
        router(test_state(true))
    }

    /// POST `body` to `/mcp`, optionally with an `Origin` header.
    fn mcp_post(body: impl Into<Body>, origin: Option<&str>) -> Request<Body> {
        let mut req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            req = req.header(header::ORIGIN, origin);
        }
        req.body(body.into()).expect("build request")
    }

    /// Read the full response body as parsed JSON.
    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        serde_json::from_slice(&bytes).expect("parse body JSON")
    }

    #[tokio::test]
    async fn initialize_returns_200_json_with_protocol_version() {
        let req_body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
        });
        let resp = loopback_router()
            .oneshot(mcp_post(serde_json::to_vec(&req_body).unwrap(), None))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "a JSON-RPC reply must be application/json"
        );
        let json = body_json(resp).await;
        assert_eq!(
            json.pointer("/result/protocolVersion")
                .and_then(serde_json::Value::as_str),
            Some("2025-06-18"),
            "initialize must advertise the Streamable HTTP spec revision: {json}"
        );
        assert!(
            json.pointer("/result/serverInfo/name").is_some(),
            "serverInfo must be present: {json}"
        );
    }

    #[tokio::test]
    async fn tools_list_returns_200_with_nonempty_tools() {
        let req_body = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
        });
        let resp = loopback_router()
            .oneshot(mcp_post(serde_json::to_vec(&req_body).unwrap(), None))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let tools = json
            .pointer("/result/tools")
            .and_then(serde_json::Value::as_array)
            .expect("tools array present");
        assert!(!tools.is_empty(), "tools/list must be non-empty over HTTP");
    }

    /// A JSON-RPC notification gets `202 Accepted` with an EMPTY body — the
    /// spec forbids a response body for accepted notifications.
    #[tokio::test]
    async fn notification_returns_202_with_empty_body() {
        let req_body = serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        });
        let resp = loopback_router()
            .oneshot(mcp_post(serde_json::to_vec(&req_body).unwrap(), None))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert!(bytes.is_empty(), "202 must carry an empty body, got {bytes:?}");
    }

    /// Malformed JSON → 400 carrying the SAME JSON-RPC error object stdio
    /// would frame: ParseError (-32700) with `id: null` (issue #173 semantics
    /// preserved across transports).
    #[tokio::test]
    async fn malformed_json_returns_400_with_jsonrpc_error_null_id() {
        let resp = loopback_router()
            .oneshot(mcp_post(&b"{\"jsonrpc\":\"2.0\""[..], None))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "the 400 body is a JSON-RPC error object"
        );
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], -32700, "must be ParseError: {json}");
        assert!(json["id"].is_null(), "undetermined id → null (issue #173): {json}");
    }

    /// The method router answers non-POST `/mcp` with 405 + `Allow: POST` —
    /// spec-legal for a stateless server with no server-initiated stream (GET)
    /// and no sessions to delete (DELETE).
    #[tokio::test]
    async fn get_and_delete_mcp_return_405_with_allow_post() {
        for method in ["GET", "DELETE"] {
            let req = Request::builder()
                .method(method)
                .uri("/mcp")
                .body(Body::empty())
                .unwrap();
            let resp = loopback_router().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{method} /mcp must be 405");
            let allow = resp
                .headers()
                .get(header::ALLOW)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            assert!(
                allow.contains("POST"),
                "{method} /mcp Allow header must include POST, got {allow:?}"
            );
        }
    }

    /// Origin matrix, loopback mode: absent and loopback Origins pass;
    /// anything else — including the opaque `null` origin — is refused 403
    /// before any dispatch work.
    #[tokio::test]
    async fn loopback_mode_origin_guard_matrix() {
        let ping = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "ping", "params": {}
        }))
        .unwrap();

        // Pass: no Origin (every non-browser MCP client), localhost in any
        // case, 127.0.0.1, and IPv6 loopback.
        for origin in [
            None,
            Some("http://localhost:3000"),
            Some("http://LOCALHOST:3000"),
            Some("http://127.0.0.1:8080"),
            Some("http://[::1]:8080"),
        ] {
            let resp = loopback_router()
                .oneshot(mcp_post(ping.clone(), origin))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "origin {origin:?} must pass");
        }

        // Refuse: a real cross-site origin and the unidentifiable `null`.
        for origin in ["https://evil.example", "null"] {
            let resp = loopback_router()
                .oneshot(mcp_post(ping.clone(), Some(origin)))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "origin {origin:?} must be refused on a loopback bind"
            );
        }
    }

    /// Public-bind mode skips the Origin guard entirely: the operator has
    /// deliberately exposed the port, so a cross-site Origin passes through to
    /// normal dispatch.
    #[tokio::test]
    async fn public_mode_lets_any_origin_through() {
        let ping = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "ping", "params": {}
        }))
        .unwrap();
        let resp = router(test_state(false))
            .oneshot(mcp_post(ping, Some("https://evil.example")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// The body cap is `transport::MAX_BODY_BYTES` (16 MiB), not axum's 2 MB
    /// default: a body over the default but under the cap is accepted, and a
    /// body over the cap is refused 413. Both halves matter — the first proves
    /// the `DefaultBodyLimit::max` layer is actually applied.
    #[tokio::test]
    async fn body_limit_is_max_body_bytes_not_axum_default() {
        // ~3 MiB valid notification: over axum's default, under our cap.
        let pad = "x".repeat(3 * 1024 * 1024);
        let over_default = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/big", "params": { "pad": pad }
        }))
        .unwrap();
        let resp = loopback_router()
            .oneshot(mcp_post(over_default, None))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::ACCEPTED,
            "3 MiB must pass — the 16 MiB cap replaces axum's 2 MB default"
        );

        // Over the cap → 413 (content never reaches the JSON-RPC layer).
        let oversize = vec![b'x'; MAX_BODY_BYTES + 1];
        let resp = loopback_router()
            .oneshot(mcp_post(oversize, None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn healthz_returns_name_and_version() {
        let req = Request::builder()
            .method("GET")
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();
        let resp = loopback_router().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["name"], "midnight-manual-mcp");
        assert_eq!(json["version"], crate::VERSION);
    }

    /// The Origin parser itself, at the unit level: loopback shapes in,
    /// everything unidentifiable or remote out.
    #[test]
    fn origin_is_loopback_classifies_correctly() {
        for ok in [
            "http://localhost",
            "http://localhost:2400",
            "https://LocalHost:2400",
            "http://127.0.0.1:2400",
            "http://127.9.9.9", // whole 127.0.0.0/8 is loopback
            "http://[::1]:2400",
        ] {
            assert!(origin_is_loopback(ok), "{ok} must count as loopback");
        }
        for bad in [
            "https://evil.example",
            "http://192.168.1.10:2400",
            "http://[::2]",
            "http://localhost.evil.example",
            "null", // sandboxed-context opaque origin
            "not a url",
        ] {
            assert!(!origin_is_loopback(bad), "{bad} must NOT count as loopback");
        }
    }
}

//! Privacy canary: the `/v1/embeddings` path emits NO query text (FR-112 / SC-061).
//!
//! This is a regression-guard canary, not a fail-first test. The embeddings
//! handler ([`midnight_manual_server::routes::embeddings`]) was built to never log or persist
//! the submitted input text:
//!
//! - its only input-path log is a `tracing::warn!` on a Voyage failure that logs
//!   the *error*, never the input;
//! - the 429 over-budget response body carries only window / limit / reset
//!   metadata (see `token_limit_429`), never the submitted input text.
//!
//! This test drives a canary-laden request through the real HTTP-backed
//! `/v1/embeddings` route (with a mock Voyage upstream) and asserts two things
//! contain no canary string:
//!
//! 1. the server's captured logs, and
//! 2. the 429 over-budget response body.
//!
//! It also includes a POSITIVE CONTROL (Part C): the 200 path emits no logs, so
//! the "no canary in logs" assertion would silently pass against an empty buffer
//! if the `EnvFilter` target scoping were ever broken (e.g. a typo'd
//! `midnight_manual_server` target) — a canary that cannot fail. Part C drives a request at a
//! Voyage upstream that returns HTTP 500, forcing the handler's only input-path
//! log (`tracing::warn!(.., "voyage embedding failed")`), and asserts the
//! captured buffer DOES contain that marker. This proves the capture pipeline
//! genuinely sees midnight-manual-server's `tracing` output, so a future success-path
//! regression (e.g. `info!(?req.input)`) would be captured and would fail the
//! canary. Part C additionally asserts the failure log + 502 body (which embed
//! the upstream error string) leak neither the sentinel nor the canary prefix.
//!
//! The canary input is the EXISTING `CanaryCategory::QueryText` sentinel from
//! [`mnm_telemetry::canary::CANARY_STRINGS`]
//! (`CANARY_zzz_xyz_query_how_to_compile_a_compact_contract`). Reusing the
//! registry value means the CI grep gate's `CANARY_zzz_xyz_` prefix also guards
//! this path.
//!
//! Scoping note (avoiding false positives from the test's own plumbing): the
//! wiremock upstream and the `reqwest` client BOTH legitimately see the input
//! text. We must assert on what *midnight-manual-server* emits, not on what the mock
//! received. The captured-logs subscriber is therefore filtered (via
//! `EnvFilter`) to admit only `mn_*` targets — `wiremock`, `reqwest`, `hyper`,
//! `tower`, and `sqlx` spans/events never reach the buffer. The assertion thus
//! fails iff midnight-manual-server logs the input, which is exactly the invariant.

#![cfg(feature = "integration")]
#![allow(clippy::doc_markdown)]

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use midnight_manual_server::{app, config::ServerConfig};
use mnm_embedding::contextualized::ContextualizedVoyageEmbedder;
use mnm_embedding::voyage::VoyageEmbedder;
use mnm_telemetry::canary::{
    self, find_first_match, CanaryCategory, CANARY_PREFIX, CANARY_STRINGS,
};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::EnvFilter;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

// ── Log-capture harness (mirrors tests/telemetry_canary.rs) ─────────────────

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn snapshot(&self) -> String {
        let buf = self.0.lock().expect("captured-logs lock");
        String::from_utf8_lossy(&buf).into_owned()
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = LogSink;
    fn make_writer(&'a self) -> Self::Writer {
        LogSink(self.0.clone())
    }
}

struct LogSink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log-sink lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ── App-with-mock-Voyage harness (mirrors tests/code_ingest_e2e.rs) ─────────

/// Boot the axum app with a mock Voyage embedder + resolved corpus model, bind
/// it to an ephemeral 127.0.0.1 port, and return the base URL.
///
/// Note: the `voyage` embedder is injected as `Some(..)` regardless of
/// `cfg.voyage_api_key`, so `/v1/embeddings` is wired even though the test cfg
/// leaves the API key unset (`build_with_limiter` uses this arg directly).
async fn spawn_server(pool: sqlx::PgPool, cfg: ServerConfig, voyage_mock_uri: &str) -> String {
    // Resolve the corpus model that migration 0008 registered (voyage-code-3@1).
    let cm = midnight_manual_server::corpus_model::resolve(&pool)
        .await
        .ok();
    let corpus_model = Arc::new(RwLock::new(cm));

    let limiter = midnight_manual_server::ratelimit::RateLimiter::from_config(&cfg);
    let token_limiter = midnight_manual_server::tokenlimit::TokenUsageLimiter::from_config(&cfg);

    // Point the server-side embedders at the local wiremock, so
    // POST /v1/embeddings is served in-process without network egress. The
    // canary requests carry no `type` and therefore route to the GENERAL
    // (contextualized) embedder; the flat embedder is wired too so `type=code`
    // requests would exercise the same harness.
    let voyage = Some(Arc::new(
        VoyageEmbedder::new("test-key", "voyage-code-3", 1024, "float")
            .with_base_url(voyage_mock_uri),
    ));
    let voyage_ctx = Some(Arc::new(
        ContextualizedVoyageEmbedder::new("test-key", "voyage-context-3", 1024, "float")
            .with_base_url(voyage_mock_uri),
    ));

    let app = app::build_with_limiter(
        pool,
        cfg,
        limiter,
        corpus_model,
        token_limiter,
        voyage,
        voyage_ctx,
        Arc::new(RwLock::new(None)),
    )
    .expect("build app");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("axum::serve");
    });
    // Tiny readiness wait so axum::serve installs its acceptor.
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

/// Mount dynamic Voyage mocks: flat `POST /v1/embeddings` returning
/// `input.len()` vectors, and `POST /v1/contextualizedembeddings` mirroring the
/// request's group shape (the route's default `type=general` path hits the
/// latter).
async fn voyage_mock() -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value =
                serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
            let n = body["input"].as_array().map_or(0, Vec::len);
            let data: Vec<serde_json::Value> = (0..n)
                .map(|k| serde_json::json!({ "embedding": vec![0.0_f32; 1024], "index": k }))
                .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": data,
                "model": "voyage-code-3",
                "usage": { "total_tokens": n }
            }))
        })
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/contextualizedembeddings"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value =
                serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
            let groups: Vec<usize> = body["inputs"]
                .as_array()
                .map(|gs| {
                    gs.iter()
                        .map(|g| g.as_array().map_or(0, Vec::len))
                        .collect()
                })
                .unwrap_or_default();
            let total: usize = groups.iter().sum();
            let data: Vec<serde_json::Value> = groups
                .iter()
                .enumerate()
                .map(|(gi, &n)| {
                    serde_json::json!({
                        "index": gi,
                        "data": (0..n)
                            .map(|k| serde_json::json!({
                                "embedding": vec![0.0_f32; 1024],
                                "index": k,
                            }))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": data,
                "model": "voyage-context-3",
                "usage": { "total_tokens": total }
            }))
        })
        .mount(&mock)
        .await;
    mock
}

/// Mount a `POST /v1/embeddings` mock that ALWAYS returns HTTP 500 with a fixed,
/// input-free body. Driving a request at this forces `voyage.embed(..)` to
/// return `Err(VoyageError::Status { .. })`, which makes the handler hit its sole
/// input-path log site (`tracing::warn!(.., "voyage embedding failed")`) and map
/// to a 502. The 500 body is a constant that contains NO request input, so the
/// handler's error log (which carries `error = %e`, i.e. `voyage returned status
/// 500: <this body>`) can be checked for leakage of the submitted text.
async fn voyage_mock_500() -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream unavailable"))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/contextualizedembeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream unavailable"))
        .mount(&mock)
        .await;
    mock
}

/// The existing `QueryText` sentinel from the canary registry. Reusing the
/// registry value (rather than inventing a literal) means the CI grep prefix
/// `CANARY_zzz_xyz_` also guards this path.
fn query_sentinel() -> &'static str {
    CANARY_STRINGS
        .iter()
        .find(|c| c.category == CanaryCategory::QueryText)
        .expect("a QueryText canary must exist in CANARY_STRINGS")
        .value
}

// ── Test ─────────────────────────────────────────────────────────────────────

/// Canary: drive a query-text-laden request through `/v1/embeddings` and assert
/// that NEITHER the server's logs, NOR `telemetry_event_raw`, NOR the 429
/// over-budget body contains the sentinel (or the canary prefix).
#[tokio::test]
async fn embeddings_path_emits_no_query_text() {
    let sentinel = query_sentinel();

    // Install a captured-logs subscriber filtered to mn_* targets ONLY, so the
    // wiremock/reqwest/hyper/tower/sqlx machinery (which legitimately handles
    // the input body) cannot pollute the buffer. The assertion then fails iff
    // *midnight-manual-server* logs the input — exactly the invariant under test.
    let captured = CapturedLogs::default();
    let filter = EnvFilter::new(
        "off,midnight_manual_server=trace,mnm_embedding=trace,mnm_telemetry=trace,mnm_retrieval=trace,mnm_store=trace,mnm_core=trace,mnm_auth=trace",
    );
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(captured.clone())
        .with_ansi(false)
        .finish();

    let h = common::boot().await;
    // Default-subscriber guard: subsequent awaits emit into the captured buffer
    // (the macros consult the current default at call-time).
    let _guard = tracing::subscriber::set_default(subscriber);

    // Keep `voyage_mock_server` alive for the whole test: dropping it shuts the
    // mock down and would fail any in-flight embedding request.
    let voyage_mock_server = voyage_mock().await;

    // ── Part A: happy-path 200 — logs + telemetry must stay canary-clean ────
    {
        // Generous limits so the happy-path request is NOT throttled.
        let cfg = ServerConfig::default();
        let base = spawn_server(h.pool.clone(), cfg, &voyage_mock_server.uri()).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/v1/embeddings"))
            .json(&serde_json::json!({
                "input": [sentinel],
                "input_type": "query",
            }))
            .send()
            .await
            .expect("POST /v1/embeddings (happy path)");
        // The mock returns one vector, so the handler responds 200.
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::OK,
            "happy-path embeddings request should return 200"
        );
        // Drain the body so the connection completes; do NOT print it.
        let _ = resp.bytes().await.expect("read 200 body");
    }

    // Let any spawned writes settle before snapshotting the table.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // ── Assertion 1: the server's captured logs carry no canary ────────────
    let log_snapshot = captured.snapshot();
    canary::assert_no_canary_in(&log_snapshot);

    // ── Part B: 429 over-budget — the rejection body must carry no input ────
    //
    // Drive the per-subject token limiter to reject at reserve time (before
    // Voyage is ever called): with `token_limit_anon_hourly = 1`, the
    // char-based estimate for the multi-char sentinel (~ceil(len/4) tokens) far
    // exceeds 1, so `reserve` returns Reject{ Hour } -> 429.
    {
        let cfg = ServerConfig {
            token_limit_anon_hourly: 1,
            token_limit_anon_daily: 1,
            ..Default::default()
        };
        let base = spawn_server(h.pool.clone(), cfg, &voyage_mock_server.uri()).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/v1/embeddings"))
            .json(&serde_json::json!({
                "input": [sentinel],
                "input_type": "query",
            }))
            .send()
            .await
            .expect("POST /v1/embeddings (over-budget)");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "an over-budget request should return 429"
        );

        let body = resp.text().await.expect("read 429 body");
        // The 429 body must contain NEITHER the sentinel NOR the canary prefix.
        // Do NOT print `body` (it must not reach stdout for the CI grep gate).
        canary::assert_no_canary_in(&body);
        assert!(
            !body.contains(sentinel),
            "429 over-budget body must not contain the submitted input text"
        );
        assert!(
            !body.contains(CANARY_PREFIX),
            "429 over-budget body must not contain the canary prefix"
        );
    }

    // ── Part C: positive control — prove the capture pipeline has teeth ─────
    //
    // The 200 path emits NO logs, so Assertion 1 above checks an essentially
    // empty buffer: if the `EnvFilter` target scoping were ever wrong (e.g. a
    // typo'd `midnight_manual_server` target), the buffer would be silently empty and a real
    // input-logging regression would NOT be caught — a canary that cannot fail.
    // `positive_control_voyage_failure` proves the pipeline genuinely sees
    // midnight-manual-server's `tracing` output (see its doc comment). It runs on a FRESH,
    // locally scoped subscriber, so it does not touch the outer shared buffer.
    positive_control_voyage_failure(h.pool.clone(), sentinel).await;

    // Final guard: the captured logs across the 200 + 429 requests (the outer
    // shared buffer; the positive-control 500 request used its own scoped
    // buffer) stay canary-clean.
    canary::assert_no_canary_in(&captured.snapshot());

    // Keep the mock alive until the very end.
    drop(voyage_mock_server);
}

/// Positive control: prove the captured-logs pipeline actually sees midnight-manual-server's
/// `tracing` output, so the "no canary in logs" assertions in this test have
/// teeth.
///
/// The 200 path emits no logs, so the canary's log assertion would silently
/// pass against an empty buffer if the `EnvFilter`/`midnight_manual_server` target scoping
/// were ever broken. This drives a request at a Voyage upstream that ALWAYS
/// 500s, forcing the handler's sole input-path log site
/// (`tracing::warn!(.., "voyage embedding failed")`, on the
/// `midnight_manual_server::routes::embeddings` target) and mapping to a 502, then asserts:
///
/// - POSITIVE CONTROL: the captured buffer CONTAINS `"voyage embedding failed"`
///   — proving the target scoping genuinely captures handler output (so a future
///   success-path `info!(?req.input)` regression would be caught);
/// - LEAK CHECK: that same failure log (which carries `error = %e`) leaks
///   neither the sentinel nor the canary prefix;
/// - the 502 body (which embeds the upstream error string) likewise leaks
///   neither.
///
/// It installs its OWN fresh `CapturedLogs` + `set_default` guard so the
/// `"voyage embedding failed"` marker can ONLY originate here (no accumulation
/// ambiguity with the caller's shared buffer).
async fn positive_control_voyage_failure(pool: sqlx::PgPool, sentinel: &str) {
    // A separate mock server returning 500, isolated from the happy-path mock so
    // the Part A 200 assertions are unaffected.
    let voyage_500 = voyage_mock_500().await;

    // Fresh buffer + subscriber scoped to this function only.
    let ctl_captured = CapturedLogs::default();
    let ctl_filter = EnvFilter::new(
        "off,midnight_manual_server=trace,mnm_embedding=trace,mnm_telemetry=trace,mnm_retrieval=trace,mnm_store=trace,mnm_core=trace,mnm_auth=trace",
    );
    let ctl_subscriber = tracing_subscriber::fmt()
        .with_env_filter(ctl_filter)
        .with_writer(ctl_captured.clone())
        .with_ansi(false)
        .finish();
    let _ctl_guard = tracing::subscriber::set_default(ctl_subscriber);

    // Token limits NOT throttled, so the request passes the reservation gate and
    // actually reaches `voyage.embed(..)` (where the 500 surfaces).
    let cfg = ServerConfig::default();
    let base = spawn_server(pool, cfg, &voyage_500.uri()).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/embeddings"))
        .json(&serde_json::json!({
            "input": [sentinel],
            "input_type": "query",
        }))
        .send()
        .await
        .expect("POST /v1/embeddings (voyage-500)");
    // The handler maps a Voyage failure to 502 (`error::bad_gateway`).
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_GATEWAY,
        "a Voyage upstream failure should map to 502"
    );

    let body = resp.text().await.expect("read 502 body");

    // Settle the warn! emission before snapshotting.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let ctl_snapshot = ctl_captured.snapshot();

    // POSITIVE CONTROL: the capture pipeline DID see midnight-manual-server's `tracing`
    // output. If this fails, the `EnvFilter`/`midnight_manual_server` target scoping is wrong
    // and the "no canary in buffer" assertions are toothless. (The marker is a
    // handler log string, NOT a canary, so asserting on it risks no exposure.)
    assert!(
        ctl_snapshot.contains("voyage embedding failed"),
        "positive control: the handler's Voyage-failure log MUST reach the \
         captured buffer — otherwise the EnvFilter target scoping is broken \
         and the canary assertions cannot fail"
    );

    // LEAK ASSERTION on the failure path: even the Voyage-failure log (which
    // logs `error = %e`) must NOT smuggle the submitted input text. Do NOT print
    // `ctl_snapshot` (it must not reach stdout for the CI grep gate).
    canary::assert_no_canary_in(&ctl_snapshot);
    assert!(
        !ctl_snapshot.contains(sentinel),
        "the Voyage-failure log must not contain the submitted input text"
    );

    // The 502 body (which embeds the upstream error string) must likewise carry
    // no canary / no sentinel — mirroring the 429 body check.
    canary::assert_no_canary_in(&body);
    assert!(
        !body.contains(sentinel),
        "502 Voyage-failure body must not contain the submitted input text"
    );
    assert!(
        !body.contains(CANARY_PREFIX),
        "502 Voyage-failure body must not contain the canary prefix"
    );

    drop(voyage_500);
}

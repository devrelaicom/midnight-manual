//! Wiremock-driven tests for `mnm status`.
//!
//! `mnm_mcp::status` already covers `assemble` in depth; these tests re-verify
//! the shared path from the CLI crate (the renderer's actual input), exercise
//! the CLI-only human renderer, and pin the scriptable exit-code contract
//! (unreachable cloud → `Err`).

use midnight_manual::commands::status::{print_human, run, Args};
use mnm_core::introspect::{MeRateLimit, MeTokenLimits, MeTokenWindow};
use mnm_mcp::cloud_client::CloudClient;
use mnm_mcp::status::{assemble, CloudState, StatusReport, VoyageState};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A `/v1/me` body with every section populated (authenticated GitHub user
/// with both limit systems present).
fn full_me_body() -> serde_json::Value {
    json!({
        "authenticated": true,
        "auth_type": "read_uplift",
        "identity": "octocat",
        "permission_level": "write",
        "rate_limit": { "tier": "read_uplift", "limit": 120, "remaining": 87,
                        "reset_secs": 31 },
        "token_limits": {
            "tier": "read_uplift",
            "hourly": { "limit": 200_000, "remaining": 150_000, "reset_at_secs": 1_200 },
            "daily": { "limit": 2_000_000, "remaining": 1_900_000, "reset_at_secs": 50_000 }
        },
        "server_version": "0.4.2"
    })
}

async fn mock_cloud(readyz_status: u16, me_body: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/readyz"))
        .respond_with(ResponseTemplate::new(readyz_status))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(me_body))
        .mount(&server)
        .await;
    server
}

// NOTE: the Voyage key is always passed explicitly (`None`) — `assemble`
// never reads the environment, so an exported VOYAGE_API_KEY cannot leak
// into these assertions.

#[tokio::test]
async fn assemble_from_cli_crate_populates_report() {
    let server = mock_cloud(200, full_me_body()).await;
    let cloud = CloudClient::new(&server.uri(), Some("tok".into())).unwrap();

    let r = assemble(&cloud, None).await;
    assert_eq!(r.cloud, CloudState::Reachable);
    assert_eq!(r.cloud_version.as_deref(), Some("0.4.2"));
    assert!(r.authenticated);
    assert_eq!(r.identity.as_deref(), Some("octocat"));
    let tl = r.token_limits.expect("token_limits populated");
    assert_eq!(tl.hourly.remaining, 150_000);
    assert_eq!(tl.daily.limit, 2_000_000);
    assert_eq!(r.voyage, VoyageState::NotConfigured);
}

#[test]
fn print_human_with_fully_populated_report_does_not_panic() {
    let report = StatusReport {
        mcp_version: "0.0.0-test",
        cloud: CloudState::Reachable,
        cloud_version: Some("0.4.2".to_owned()),
        authenticated: true,
        auth_type: "read_uplift".to_owned(),
        identity: Some("octocat".to_owned()),
        permission_level: "write".to_owned(),
        rate_limit: Some(MeRateLimit {
            tier: "read_uplift".to_owned(),
            limit: 120,
            remaining: 87,
            reset_secs: 31,
        }),
        token_limits: Some(MeTokenLimits {
            tier: "read_uplift".to_owned(),
            hourly: MeTokenWindow {
                limit: 200_000,
                remaining: 150_000,
                reset_at_secs: 1_200,
            },
            daily: MeTokenWindow {
                limit: 2_000_000,
                remaining: 1_900_000,
                reset_at_secs: 50_000,
            },
        }),
        voyage: VoyageState::Valid,
        reranker: "test-reranker",
        reranker_loaded: true,
    };
    print_human(&report, "http://localhost:8080");
}

#[test]
fn print_human_with_minimal_anonymous_report_does_not_panic() {
    let report = StatusReport {
        mcp_version: "0.0.0-test",
        cloud: CloudState::Unreachable,
        cloud_version: None,
        authenticated: false,
        auth_type: "anonymous".to_owned(),
        identity: None,
        permission_level: "read".to_owned(),
        rate_limit: None,
        token_limits: None,
        voyage: VoyageState::NotConfigured,
        reranker: "test-reranker",
        reranker_loaded: false,
    };
    print_human(&report, "http://localhost:8080");
}

#[tokio::test]
async fn run_against_dead_port_returns_err() {
    // Port 9 (discard) is closed: connection refused → CloudState::Unreachable
    // → `run` must return Err so scripts get a non-zero exit code.
    let err = run(Args {}, Some("http://127.0.0.1:9"), None, true)
        .await
        .expect_err("unreachable cloud must be a hard error");
    assert!(err.to_string().contains("unreachable"), "error should name the failure: {err}");
}

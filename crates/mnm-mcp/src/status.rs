//! Shared status assembly for the MCP `status` tool and `mnm status`.
//! Probes run concurrently with a 3s budget each; a failed probe degrades
//! that section, never the whole report.

use std::time::Duration;

use mnm_core::injection::SecurityLevel;
use mnm_core::introspect::{MeRateLimit, MeResponse, MeTokenLimits};
use serde::Serialize;

use crate::cloud_client::CloudClient;

/// The reranker model reported by `status`. Reranking is VoyageAI now (server
/// inline or client BYOK), so this is the default Voyage rerank model name —
/// sourced from the shared [`mnm_core::rerank::RerankParam`] vocabulary so the
/// status report and the rerank request never drift.
const RERANKER_MODEL_NAME: &str = match mnm_core::rerank::RerankParam::Rerank25.model_name() {
    Some(name) => name,
    None => "rerank-2.5",
};

/// Cloud reachability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudState {
    /// `/readyz` returned 200.
    Reachable,
    /// `/readyz` returned non-200 (server up, dependencies not ready).
    Degraded,
    /// Transport failure.
    Unreachable,
}

/// VoyageAI key state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoyageState {
    /// Key present and accepted by the Voyage API.
    Valid,
    /// Key present but rejected (401/403).
    InvalidKey,
    /// Key present but the probe failed (network/timeout/5xx).
    Unreachable,
    /// No key configured — embedding goes through the server proxy.
    NotConfigured,
}

/// Full status report. One struct, two renderers (MCP projector, CLI).
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    /// This binary's mnm-mcp version.
    pub mcp_version: &'static str,
    /// Cloud reachability.
    pub cloud: CloudState,
    /// Cloud server version (from `/v1/me`), when reachable.
    pub cloud_version: Option<String>,
    /// `true` when a bearer was presented and accepted.
    pub authenticated: bool,
    /// `anonymous` / `read_uplift` / `admin` (from `/v1/me`).
    pub auth_type: String,
    /// Identity string (GitHub login or admin user id), when authenticated.
    pub identity: Option<String>,
    /// `read` / `write` / `admin`.
    pub permission_level: String,
    /// Request rate-limit bucket state (from `/v1/me`), when reachable and the
    /// limiter is enabled.
    pub rate_limit: Option<MeRateLimit>,
    /// Embedding token-budget windows (from `/v1/me`): `{tier, hourly, daily}`.
    pub token_limits: Option<MeTokenLimits>,
    /// Voyage key state.
    pub voyage: VoyageState,
    /// Reranker model name (VoyageAI — server inline or client BYOK).
    pub reranker: &'static str,
    /// Whether a local (BYOK) rerank has been exercised in this process.
    pub reranker_loaded: bool,
    /// Active client-side content-guard level resolved for this process
    /// (`disabled`/`low`/`moderate`/`high`/`strict`; default `moderate`).
    /// This is the level the response guard actually applies, so an agent can
    /// see why returned content is (or isn't) wrapped in `<<UNTRUSTED-…>>`
    /// blocks — and, at `strict`, why flagged items are removed.
    pub security_level: SecurityLevel,
}

/// Probe Voyage with the given key. `GET /v1/files` is a cheap authenticated
/// endpoint: 200 → valid, 401/403 → invalid key, anything else → unreachable.
async fn probe_voyage(key: &str) -> VoyageState {
    probe_voyage_at("https://api.voyageai.com/v1/files", key).await
}

/// Inner probe against an arbitrary base — separated so tests can wiremock it.
async fn probe_voyage_at(url: &str, key: &str) -> VoyageState {
    // `http1_only`: Voyage's HTTP/2 endpoint stalls/resets intermittently (see
    // mnm-embedding's VoyageEmbedder, which forces HTTP/1.1 for the same host).
    // The probe must use the same transport as the workload it diagnoses, or a
    // valid key can misreport as `Unreachable`.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .http1_only()
        .build()
    {
        Ok(c) => c,
        Err(_) => return VoyageState::Unreachable,
    };
    match client.get(url).bearer_auth(key).send().await {
        Ok(r) if r.status().is_success() => VoyageState::Valid,
        Ok(r) if r.status() == 401 || r.status() == 403 => VoyageState::InvalidKey,
        _ => VoyageState::Unreachable,
    }
}

/// Assemble the report. `voyage_key` is the resolved BYOK key (None = proxy
/// mode); `security_level` is the active content-guard level resolved for this
/// process (the same value the response guard applies), surfaced verbatim.
pub async fn assemble(
    cloud: &CloudClient,
    voyage_key: Option<&str>,
    security_level: SecurityLevel,
) -> StatusReport {
    // The wrappers tighten CloudClient's general-purpose 30s timeout down to
    // this module's 3s-per-probe status budget.
    let readyz = tokio::time::timeout(Duration::from_secs(3), cloud.readyz());
    let me = tokio::time::timeout(Duration::from_secs(3), cloud.get_me());
    let voyage = async {
        match voyage_key {
            None => VoyageState::NotConfigured,
            Some(k) => probe_voyage(k).await,
        }
    };
    let (readyz, me, voyage) = tokio::join!(readyz, me, voyage);

    let cloud_state = match readyz {
        Ok(Ok(200)) => CloudState::Reachable,
        Ok(Ok(_)) => CloudState::Degraded,
        _ => CloudState::Unreachable,
    };
    // Deserialize the `/v1/me` body once into the shared typed contract. A
    // malformed/partial body (or unreachable cloud) collapses to `None` and the
    // report falls back to anonymous defaults — the same degradation the
    // stringly-typed reader produced, but now pinned to one shape.
    let me: Option<MeResponse> = me
        .ok()
        .and_then(Result::ok)
        .and_then(|v| serde_json::from_value(v).ok());
    StatusReport {
        mcp_version: crate::VERSION,
        cloud: cloud_state,
        cloud_version: me.as_ref().map(|m| m.server_version.clone()),
        authenticated: me.as_ref().is_some_and(|m| m.authenticated),
        auth_type: me
            .as_ref()
            .map_or_else(|| "anonymous".to_owned(), |m| m.auth_type.clone()),
        identity: me.as_ref().and_then(|m| m.identity.clone()),
        permission_level: me
            .as_ref()
            .map_or_else(|| "read".to_owned(), |m| m.permission_level.clone()),
        rate_limit: me.as_ref().and_then(|m| m.rate_limit.clone()),
        token_limits: me.as_ref().map(|m| m.token_limits.clone()),
        voyage,
        reranker: RERANKER_MODEL_NAME,
        reranker_loaded: crate::tools::reranker_loaded(),
        security_level,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use mnm_core::injection::SecurityLevel;

    use super::{assemble, probe_voyage_at, CloudState, VoyageState};
    use crate::cloud_client::CloudClient;

    /// A `/v1/me` body with every section populated (authenticated GitHub
    /// user with both limit systems present).
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

    // NOTE: every test passes the Voyage key explicitly (here: `None`) —
    // `assemble` never reads the environment, so an exported VOYAGE_API_KEY
    // cannot leak into these assertions.

    #[tokio::test]
    async fn assemble_reachable_with_full_me_populates_all_sections() {
        let server = mock_cloud(200, full_me_body()).await;
        let cloud = CloudClient::new(&server.uri(), Some("tok".into())).unwrap();

        // A non-default level is threaded through so the report reflects the
        // resolved level verbatim, not the enum default.
        let r = assemble(&cloud, None, SecurityLevel::Strict).await;
        assert_eq!(r.cloud, CloudState::Reachable);
        assert_eq!(r.cloud_version.as_deref(), Some("0.4.2"));
        assert!(r.authenticated);
        assert_eq!(r.auth_type, "read_uplift");
        assert_eq!(r.identity.as_deref(), Some("octocat"));
        assert_eq!(r.permission_level, "write");
        let rl = r.rate_limit.expect("rate_limit populated");
        assert_eq!(rl.remaining, 87);
        assert_eq!(rl.tier, "read_uplift");
        let tl = r.token_limits.expect("token_limits populated");
        assert_eq!(tl.hourly.limit, 200_000);
        assert_eq!(r.voyage, VoyageState::NotConfigured);
        assert_eq!(r.reranker, super::RERANKER_MODEL_NAME);
        assert_eq!(r.security_level, SecurityLevel::Strict);
    }

    #[tokio::test]
    async fn assemble_readyz_non_200_is_degraded() {
        let server = mock_cloud(503, full_me_body()).await;
        let cloud = CloudClient::new(&server.uri(), None).unwrap();

        let r = assemble(&cloud, None, SecurityLevel::Moderate).await;
        assert_eq!(r.cloud, CloudState::Degraded);
        // `/v1/me` still populates its sections — a degraded readiness probe
        // does not blank the rest of the report.
        assert_eq!(r.identity.as_deref(), Some("octocat"));
    }

    #[tokio::test]
    async fn assemble_unreachable_cloud_degrades_to_defaults() {
        // Port 9 (discard) is closed: connection refused → transport error.
        let cloud = CloudClient::new("http://127.0.0.1:9", None).unwrap();

        let r = assemble(&cloud, None, SecurityLevel::Moderate).await;
        assert_eq!(r.cloud, CloudState::Unreachable);
        assert!(!r.authenticated);
        assert_eq!(r.auth_type, "anonymous");
        assert_eq!(r.permission_level, "read");
        assert!(r.identity.is_none());
        assert!(r.cloud_version.is_none());
        assert!(r.rate_limit.is_none());
        assert!(r.token_limits.is_none());
        assert_eq!(r.voyage, VoyageState::NotConfigured);
    }

    #[tokio::test]
    async fn assemble_me_null_rate_limit_collapses_to_none() {
        // Anonymous body the server actually emits when the limiter is
        // disabled: `rate_limit: null`, but `token_limits` always present.
        let body = json!({
            "authenticated": false,
            "auth_type": "anonymous",
            "permission_level": "read",
            "rate_limit": null,
            "token_limits": {
                "tier": "anonymous",
                "hourly": { "limit": 50_000, "remaining": 50_000, "reset_at_secs": 1_200 },
                "daily": { "limit": 500_000, "remaining": 500_000, "reset_at_secs": 50_000 }
            },
            "server_version": "0.4.2"
        });
        let server = mock_cloud(200, body).await;
        let cloud = CloudClient::new(&server.uri(), None).unwrap();

        let r = assemble(&cloud, None, SecurityLevel::Moderate).await;
        assert!(r.rate_limit.is_none(), "explicit null rate_limit must collapse to None");
        let tl = r
            .token_limits
            .expect("token_limits always present when reachable");
        assert_eq!(tl.tier, "anonymous");
    }

    #[tokio::test]
    async fn assemble_me_malformed_body_degrades_to_anonymous_defaults() {
        // A body missing a required field (here `token_limits`) no longer
        // deserializes into the shared contract, so the whole report falls back
        // to anonymous defaults rather than silently reading partial fields.
        let body = json!({
            "authenticated": true,
            "auth_type": "read_uplift",
            "permission_level": "write",
            "rate_limit": null,
            "server_version": "0.4.2"
            // no token_limits key — invalid against MeResponse
        });
        let server = mock_cloud(200, body).await;
        let cloud = CloudClient::new(&server.uri(), None).unwrap();

        let r = assemble(&cloud, None, SecurityLevel::Moderate).await;
        assert!(!r.authenticated, "malformed body cannot report authenticated");
        assert_eq!(r.auth_type, "anonymous");
        assert_eq!(r.permission_level, "read");
        assert!(r.rate_limit.is_none());
        assert!(r.token_limits.is_none());
    }

    #[tokio::test]
    async fn probe_voyage_at_200_is_valid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
            .mount(&server)
            .await;
        let url = format!("{}/v1/files", server.uri());
        assert_eq!(probe_voyage_at(&url, "k").await, VoyageState::Valid);
    }

    #[tokio::test]
    async fn probe_voyage_at_401_and_403_are_invalid_key() {
        for status in [401_u16, 403] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/v1/files"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let url = format!("{}/v1/files", server.uri());
            assert_eq!(
                probe_voyage_at(&url, "bad").await,
                VoyageState::InvalidKey,
                "status {status} must map to InvalidKey"
            );
        }
    }

    #[tokio::test]
    async fn probe_voyage_at_5xx_is_unreachable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/files"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let url = format!("{}/v1/files", server.uri());
        assert_eq!(probe_voyage_at(&url, "k").await, VoyageState::Unreachable);
    }

    #[tokio::test]
    async fn report_serializes_with_snake_case_states() {
        let cloud = CloudClient::new("http://127.0.0.1:9", None).unwrap();
        let r = assemble(&cloud, None, SecurityLevel::Moderate).await;
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["cloud"], "unreachable");
        assert_eq!(v["voyage"], "not_configured");
        assert!(v["reranker_loaded"].is_boolean());
        // `SecurityLevel` serializes as its lowercase wire string, matching the
        // `security_level` enum advertised in the status outputSchema.
        assert_eq!(v["security_level"], "moderate");
    }
}

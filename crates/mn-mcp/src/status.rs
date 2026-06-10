//! Shared status assembly for the MCP `status` tool and `mnm status`.
//! Probes run concurrently with a 3s budget each; a failed probe degrades
//! that section, never the whole report.

use std::time::Duration;

use serde::Serialize;

use crate::cloud_client::CloudClient;

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
    /// This binary's mn-mcp version.
    pub mcp_version: &'static str,
    /// Cloud reachability.
    pub cloud: CloudState,
    /// Cloud server version (from `/v1/me`), when reachable.
    pub cloud_version: Option<String>,
    /// `true` when a bearer was presented and accepted.
    pub authenticated: bool,
    /// `anonymous` / `github_oauth` / `admin` (from `/v1/me`).
    pub auth_type: String,
    /// Identity string (GitHub login or admin user id), when authenticated.
    pub identity: Option<String>,
    /// `read` / `write` / `admin`.
    pub permission_level: String,
    /// Request rate-limit bucket state (from `/v1/me`), when reachable.
    pub rate_limit: Option<serde_json::Value>,
    /// Embedding token-budget windows (from `/v1/me`): `{tier, hourly, daily}`.
    pub token_limits: Option<serde_json::Value>,
    /// Voyage key state.
    pub voyage: VoyageState,
    /// Local reranker model name.
    pub reranker: &'static str,
    /// Whether the local reranker is loaded into memory.
    pub reranker_loaded: bool,
}

/// Probe Voyage with the given key. `GET /v1/files` is a cheap authenticated
/// endpoint: 200 → valid, 401/403 → invalid key, anything else → unreachable.
async fn probe_voyage(key: &str) -> VoyageState {
    probe_voyage_at("https://api.voyageai.com/v1/files", key).await
}

/// Inner probe against an arbitrary base — separated so tests can wiremock it.
async fn probe_voyage_at(url: &str, key: &str) -> VoyageState {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
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

/// Assemble the report. `voyage_key` is the resolved BYOK key (None = proxy mode).
pub async fn assemble(cloud: &CloudClient, voyage_key: Option<&str>) -> StatusReport {
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
    let me = me.ok().and_then(Result::ok);
    let str_of = |v: &serde_json::Value, k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    StatusReport {
        mcp_version: crate::VERSION,
        cloud: cloud_state,
        cloud_version: me.as_ref().and_then(|m| str_of(m, "server_version")),
        authenticated: me
            .as_ref()
            .and_then(|m| m.get("authenticated").and_then(serde_json::Value::as_bool))
            .unwrap_or(false),
        auth_type: me
            .as_ref()
            .and_then(|m| str_of(m, "auth_type"))
            .unwrap_or_else(|| "anonymous".to_owned()),
        identity: me.as_ref().and_then(|m| str_of(m, "identity")),
        permission_level: me
            .as_ref()
            .and_then(|m| str_of(m, "permission_level"))
            .unwrap_or_else(|| "read".to_owned()),
        rate_limit: me
            .as_ref()
            .and_then(|m| m.get("rate_limit").cloned())
            .filter(|v| !v.is_null()),
        token_limits: me
            .as_ref()
            .and_then(|m| m.get("token_limits").cloned())
            .filter(|v| !v.is_null()),
        voyage,
        reranker: mn_embedding::RERANKER_MODEL_NAME,
        reranker_loaded: crate::tools::reranker_loaded(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{assemble, probe_voyage_at, CloudState, VoyageState};
    use crate::cloud_client::CloudClient;

    /// A `/v1/me` body with every section populated (authenticated GitHub
    /// user with both limit systems present).
    fn full_me_body() -> serde_json::Value {
        json!({
            "authenticated": true,
            "auth_type": "github_oauth",
            "identity": "octocat",
            "permission_level": "write",
            "rate_limit": { "tier": "authenticated", "limit": 120, "remaining": 87,
                            "reset_secs": 31 },
            "token_limits": {
                "tier": "authenticated",
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

        let r = assemble(&cloud, None).await;
        assert_eq!(r.cloud, CloudState::Reachable);
        assert_eq!(r.cloud_version.as_deref(), Some("0.4.2"));
        assert!(r.authenticated);
        assert_eq!(r.auth_type, "github_oauth");
        assert_eq!(r.identity.as_deref(), Some("octocat"));
        assert_eq!(r.permission_level, "write");
        let rl = r.rate_limit.expect("rate_limit populated");
        assert_eq!(rl["remaining"], 87);
        let tl = r.token_limits.expect("token_limits populated");
        assert_eq!(tl["hourly"]["limit"], 200_000);
        assert_eq!(r.voyage, VoyageState::NotConfigured);
        assert_eq!(r.reranker, mn_embedding::RERANKER_MODEL_NAME);
    }

    #[tokio::test]
    async fn assemble_readyz_non_200_is_degraded() {
        let server = mock_cloud(503, full_me_body()).await;
        let cloud = CloudClient::new(&server.uri(), None).unwrap();

        let r = assemble(&cloud, None).await;
        assert_eq!(r.cloud, CloudState::Degraded);
        // `/v1/me` still populates its sections — a degraded readiness probe
        // does not blank the rest of the report.
        assert_eq!(r.identity.as_deref(), Some("octocat"));
    }

    #[tokio::test]
    async fn assemble_unreachable_cloud_degrades_to_defaults() {
        // Port 9 (discard) is closed: connection refused → transport error.
        let cloud = CloudClient::new("http://127.0.0.1:9", None).unwrap();

        let r = assemble(&cloud, None).await;
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
    async fn assemble_me_missing_or_null_limits_yield_none() {
        let body = json!({
            "authenticated": false,
            "auth_type": "anonymous",
            "permission_level": "read",
            "rate_limit": null,
            "server_version": "0.4.2"
            // no token_limits key at all
        });
        let server = mock_cloud(200, body).await;
        let cloud = CloudClient::new(&server.uri(), None).unwrap();

        let r = assemble(&cloud, None).await;
        assert!(r.rate_limit.is_none(), "explicit null must collapse to None");
        assert!(r.token_limits.is_none(), "absent key must collapse to None");
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
        let r = assemble(&cloud, None).await;
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["cloud"], "unreachable");
        assert_eq!(v["voyage"], "not_configured");
        assert!(v["reranker_loaded"].is_boolean());
    }
}

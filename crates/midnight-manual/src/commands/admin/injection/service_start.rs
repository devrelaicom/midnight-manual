//! `mnm admin injection service-start` — warm up the hosted model detector.
//!
//! `POST /v1/admin/injection/service-start` long-polls the server (up to ~120s)
//! until the model detector is loaded, so the client uses a generous 180s
//! request timeout. The response reports whether the detector became ready, and
//! if not, why (not configured, or it timed out).

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::ServiceStartArgs;

/// Server response for `POST /v1/admin/injection/service-start`.
///
/// Mirrors the server's wire shape. Both the human and `--json` paths
/// deserialize into this; the `--json` path re-serializes it so the printed
/// payload tracks the typed contract rather than echoing the raw body.
#[derive(Debug, Deserialize, Serialize)]
struct ServiceStartResponse {
    /// Whether the model detector is loaded and ready to score.
    ready: bool,
    /// Why the detector is not ready (e.g. not configured). Absent when ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// Whether the warmup long-poll timed out before the detector loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timed_out: Option<bool>,
}

/// Run `mnm admin injection service-start`.
///
/// # Errors
///
/// Returns an error on transport failure, a non-2xx response, a response body
/// that does not parse, or when no admin bearer can be resolved from
/// `auth.toml`.
pub async fn run(_args: ServiceStartArgs, server: Option<&str>, json: bool) -> Result<()> {
    let server_url = crate::shared::resolve_server_url(server);
    let token = crate::commands::ratelimits::require_admin_token_from(&mnm_core::config::StdEnv)?;

    // The server long-polls up to ~120s for the model to load; allow headroom.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .context("build HTTP client")?;

    let resp = client
        .post(format!("{server_url}/v1/admin/injection/service-start"))
        .bearer_auth(&token)
        .send()
        .await
        .context("POST /v1/admin/injection/service-start")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(error_from_body(status, &body));
    }

    let parsed: ServiceStartResponse =
        serde_json::from_str(&body).context("parse injection service-start response")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&parsed)?);
    } else if parsed.ready {
        println!("Model detector ready.");
    } else if parsed.timed_out == Some(true) {
        println!("Timed out waiting for model detector.");
    } else {
        let reason = parsed.reason.as_deref().unwrap_or("(no reason given)");
        println!("Model detector not configured: {reason}");
    }

    Ok(())
}

/// Turn a non-2xx `(status, body)` into a clean `anyhow` error, preferring the
/// server's typed error envelope and falling back to the raw body.
fn error_from_body(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    crate::shared::decode_error_envelope(body).map_or_else(
        || anyhow::anyhow!("{status} from injection service-start: {body}"),
        |e| {
            anyhow::anyhow!(
                "{status} from injection service-start: {}: {} ({})",
                e.code,
                e.message,
                e.remediation
            )
        },
    )
}

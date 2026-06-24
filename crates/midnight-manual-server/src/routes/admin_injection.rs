//! Admin prompt-injection endpoints (issue #103).
//!
//! Two admin-gated endpoints support operating the injection model detector:
//!
//! 1. `POST /v1/admin/injection/service-start` — idempotently warm the hosted
//!    HF model endpoint (which scales to zero) and wait for it to answer, so the
//!    first real ingest scan doesn't pay the cold-start latency.
//! 2. `POST /v1/admin/injection/score` — score arbitrary content on demand and
//!    return the full [`mnm_core::injection::ScanReport`]. Persists nothing.
//!
//! Both require an admin-tier bearer (FR-058 + FR-117).

use std::collections::HashSet;

use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mnm_core::error::{Error as CoreError, ErrorCode};
use mnm_core::injection::ModelReport;
use serde::Deserialize;

use crate::app::AppState;
use crate::error;
use crate::middleware::bearer::AuthContext;
use crate::middleware::request_id::RequestId;

/// Mount the admin injection routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/injection/service-start", post(service_start))
        .route("/v1/admin/injection/score", post(score))
}

/// Body of `POST /v1/admin/injection/score`.
#[derive(Debug, Deserialize)]
struct ScoreRequest {
    /// The content to score (required, non-empty).
    content: String,
    /// Comma-separated detector list (e.g. `"pattern,model"`). `None` ⇒ both.
    #[serde(default)]
    detector: Option<String>,
}

/// Idempotently warm the model endpoint and wait for it to answer.
async fn service_start(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    let Some(model) = state
        .injection
        .model
        .as_ref()
        .filter(|_| state.injection.enabled)
    else {
        return Json(serde_json::json!({
            "ready": false,
            "reason": "model detector not configured"
        }))
        .into_response();
    };

    match model
        .service_start(std::time::Duration::from_secs(120))
        .await
    {
        Ok(true) => Json(serde_json::json!({ "ready": true })).into_response(),
        Ok(false) => Json(serde_json::json!({ "ready": false, "timed_out": true })).into_response(),
        Err(e) => error::bad_gateway(format!("injection model service-start failed: {e}"), rid),
    }
}

/// Score arbitrary content and return the full scan report. Persists nothing.
async fn score(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    Json(req): Json<ScoreRequest>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    if req.content.is_empty() {
        return error::bad_request(
            "content must not be empty",
            "supply non-empty content to score",
            rid,
        );
    }

    let detectors = parse_detectors(req.detector.as_deref());
    let run_model_requested = detectors.contains("model");

    let pattern = mnm_core::injection::detect(&req.content);

    let model_leg = if run_model_requested {
        match state
            .injection
            .model
            .as_ref()
            .filter(|_| state.injection.enabled)
        {
            Some(model) => match model
                .score(&req.content, state.injection.policy.model_threshold)
                .await
            {
                Ok(report) => Some(report),
                Err(e) => {
                    tracing::warn!(
                        request_id = rid,
                        error = %e,
                        "admin injection score: model leg failed"
                    );
                    // Requested but unavailable ⇒ available:false.
                    Some(ModelReport::default())
                }
            },
            // Requested but not configured/enabled ⇒ available:false.
            None => Some(ModelReport::default()),
        }
    } else {
        None
    };

    let report =
        crate::injection::scan::assemble_report(pattern, model_leg, &state.injection.policy);
    Json(report).into_response()
}

/// Parse the comma-separated `detector` field into a normalized set.
///
/// `None` defaults to `{"pattern", "model"}`. Each entry is trimmed,
/// lowercased, and empty entries are dropped.
fn parse_detectors(raw: Option<&str>) -> HashSet<String> {
    raw.unwrap_or("pattern,model")
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Returns `Some(response)` to short-circuit the handler with an auth-failure
/// response, or `None` when the caller is admin-authorised.
fn admin_reject(rid: &str, auth: Option<&Extension<AuthContext>>) -> Option<Response> {
    match auth {
        None => Some(error::into_response(
            CoreError::builder(ErrorCode::Unauthorized)
                .message("admin bearer required")
                .remediation("obtain an admin token via `mnm login` and retry")
                .build(),
            rid,
        )),
        Some(Extension(ctx)) if ctx.can_admin() => None,
        Some(_) => Some(error::into_response(
            CoreError::builder(ErrorCode::Forbidden)
                .message("admin tier required for injection endpoints")
                .remediation(
                    "read-uplift tokens may not access admin endpoints — request admin tier",
                )
                .build(),
            rid,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_detectors, ScoreRequest};

    #[test]
    fn score_request_deserializes() {
        let body = serde_json::json!({ "content": "hi", "detector": "pattern,model" });
        let req: ScoreRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.content, "hi");
        assert_eq!(req.detector.as_deref(), Some("pattern,model"));
    }

    #[test]
    fn score_request_defaults_detector_none() {
        let body = serde_json::json!({ "content": "hi" });
        let req: ScoreRequest = serde_json::from_value(body).unwrap();
        assert!(req.detector.is_none());
    }

    #[test]
    fn parse_detectors_defaults_to_both() {
        let set = parse_detectors(None);
        assert!(set.contains("pattern"));
        assert!(set.contains("model"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn parse_detectors_single() {
        let set = parse_detectors(Some("pattern"));
        assert!(set.contains("pattern"));
        assert!(!set.contains("model"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn parse_detectors_normalizes_case_and_whitespace() {
        let set = parse_detectors(Some("PATTERN, MODEL "));
        assert!(set.contains("pattern"));
        assert!(set.contains("model"));
        assert_eq!(set.len(), 2);
    }
}

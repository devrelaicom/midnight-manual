//! `GET /v1/models/active` — return the corpus's active embedding model
//! identifier (US4 acceptance #12, FR-039). Clients use this to detect they
//! need to pull a different model before issuing queries.

use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mnm_store::entities::embedding_model;
use serde::Serialize;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// The corpus's embedding dtype. The `embedding_model` registry has no dtype
/// column yet, so the active-model response reports this fixed constant —
/// `"float"`, which is the dtype the corpus is actually encoded with (Voyage's
/// default `output_dtype`). Clients derive their embedder's `output_dtype` from
/// this value (see `mnm_core::embedder_identity`) so the model used to COMPUTE a
/// vector cannot diverge from the model used to LABEL it. Promote this to a real
/// registry column if/when the corpus carries vectors of more than one dtype.
///
/// `pub(crate)` because the server-side proxy embedders ([`crate::app::resolved_embedders`])
/// build with this SAME constant, so the dtype the proxy computes with cannot
/// drift from the dtype `/v1/models/active` reports.
pub(crate) const CORPUS_DTYPE: &str = "float";

/// Mount the models routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/models/active", get(active_model))
}

/// Response shape for `/v1/models/active`.
#[derive(Debug, Serialize)]
struct ActiveModelResponse {
    name: String,
    revision: i32,
    dim: i32,
    provider: String,
    /// Output dtype the corpus is encoded with. Fixed constant ([`CORPUS_DTYPE`])
    /// until the registry tracks dtype; clients use it to build their embedder
    /// so the compute-side dtype matches the label.
    dtype: String,
    /// The corpus's code-embedding model, when resolved. `null`/absent means
    /// code search is unavailable server-side.
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<ActiveModelInfo>,
}

/// One embedding model's identity — the same `{name, revision, dim, provider,
/// dtype}` shape the top-level response uses for the general model.
#[derive(Debug, Serialize)]
struct ActiveModelInfo {
    name: String,
    revision: i32,
    dim: i32,
    provider: String,
    /// Output dtype (fixed [`CORPUS_DTYPE`]; see the top-level field).
    dtype: String,
}

async fn active_model(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match embedding_model::get_active(&state.pool).await {
        Ok(m) => Json(ActiveModelResponse {
            name: m.name,
            revision: m.revision,
            dim: m.dim,
            provider: m.provider,
            dtype: CORPUS_DTYPE.to_owned(),
            code: code_model_info(&state, rid).await,
        })
        .into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "active model lookup failed");
            error::service_unavailable("active model lookup failed", rid)
        }
    }
}

/// Snapshot the boot-resolved code model and hydrate its registry row (the
/// response includes `provider`, which the snapshot doesn't carry). `None` —
/// unresolved at boot, or a lookup failure here — means "code search
/// unavailable"; the general half of the response is unaffected.
async fn code_model_info(state: &AppState, rid: &str) -> Option<ActiveModelInfo> {
    let snapshot = state
        .code_model
        .read()
        .expect("code_model lock poisoned")
        .clone()?;
    match embedding_model::get_by_id(&state.pool, snapshot.id).await {
        Ok(m) => Some(ActiveModelInfo {
            name: m.name,
            revision: m.revision,
            dim: m.dim,
            provider: m.provider,
            dtype: CORPUS_DTYPE.to_owned(),
        }),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "code model lookup failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ActiveModelResponse {
        ActiveModelResponse {
            name: "voyage-context-3".to_owned(),
            revision: 1,
            dim: 1024,
            provider: "voyageai".to_owned(),
            dtype: CORPUS_DTYPE.to_owned(),
            code: None,
        }
    }

    #[test]
    fn code_key_is_omitted_when_unresolved() {
        let v = serde_json::to_value(base()).unwrap();
        assert_eq!(v["name"], "voyage-context-3");
        assert!(v.get("code").is_none());
    }

    /// The top-level response carries `dtype` and it is the corpus's fixed
    /// `"float"`. Clients derive their embedder dtype from this field, so it
    /// must always be present and stable.
    #[test]
    fn top_level_dtype_is_float() {
        let v = serde_json::to_value(base()).unwrap();
        assert_eq!(v["dtype"], "float");
    }

    #[test]
    fn code_key_carries_the_full_model_shape_when_resolved() {
        let mut resp = base();
        resp.code = Some(ActiveModelInfo {
            name: "voyage-code-3".to_owned(),
            revision: 1,
            dim: 1024,
            provider: "voyageai".to_owned(),
            dtype: CORPUS_DTYPE.to_owned(),
        });
        let v = serde_json::to_value(resp).unwrap();
        assert_eq!(v["code"]["name"], "voyage-code-3");
        assert_eq!(v["code"]["revision"], 1);
        assert_eq!(v["code"]["dim"], 1024);
        assert_eq!(v["code"]["provider"], "voyageai");
        // The code half carries dtype too, so a code embedder is built from the
        // same source as the code wire-id label.
        assert_eq!(v["code"]["dtype"], "float");
    }
}

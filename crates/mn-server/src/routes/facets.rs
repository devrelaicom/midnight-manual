//! `GET /v1/facets` — advertise the filterable facets, their types, and (for
//! closed enums) their allowed values, so clients can construct valid filters.
//! Open-set values are filled from the active corpus in `corpus_values`.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use mn_retrieval::facets::{self, FacetType};
use serde_json::json;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Mount the `GET /v1/facets` route.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/facets", get(get_facets))
}

async fn get_facets(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let open = match corpus_values(&state.pool).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "facet value query failed");
            return error::service_unavailable("facet enumeration failed", rid);
        }
    };

    let filters: Vec<_> = facets::facets()
        .iter()
        .map(|d| {
            let type_str = match d.facet_type {
                FacetType::Enum => "enum",
                FacetType::OpenSet => "open_set",
                FacetType::ObjectSet => "object_set",
                FacetType::Bool => "bool",
                FacetType::RangeTemporal => "range_temporal",
                FacetType::RangeNumeric => "range_numeric",
            };
            let mut entry = json!({ "key": d.key, "type": type_str, "negatable": d.negatable });
            if let Some(vals) = d.closed_values {
                entry["values"] = json!(vals);
            } else if let Some(v) = open.get(d.key) {
                entry["values"] = v.values.clone();
                entry["truncated"] = json!(v.truncated);
                entry["total"] = json!(v.total);
            }
            entry
        })
        .collect();

    Json(json!({
        "modes": ["hybrid", "vector", "fts"],
        "filters": filters,
    }))
    .into_response()
}

/// Bounded distinct values for open-set facets, keyed by facet name.
struct OpenValues {
    values: serde_json::Value,
    truncated: bool,
    total: i64,
}

/// Corpus-derived open-set values. STUB in B4 (returns empty); the real bounded
/// DISTINCT queries land in Task B5. An empty map means open-set facets
/// advertise their type only.
// B5 replaces this body with `await`ing bounded-DISTINCT queries against `pool`.
#[allow(clippy::unused_async)]
async fn corpus_values(
    _pool: &sqlx::PgPool,
) -> Result<std::collections::HashMap<String, OpenValues>, sqlx::Error> {
    Ok(std::collections::HashMap::new())
}

//! `GET /v1/facets` — advertise the filterable facets, their types, and (for
//! closed enums) their allowed values, so clients can construct valid filters.
//! Open-set values are filled from the active corpus in `corpus_values` and
//! served from a short-lived TTL cache held in [`crate::app::AppState`].

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use mn_retrieval::facets::{self, FacetType};
use serde_json::json;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Shared 60s TTL cache for the assembled `/v1/facets` body. Held in
/// [`crate::app::AppState`] (NOT a module-global static) so each constructed
/// app — including each integration test's app — gets an isolated cache.
pub type FacetsCache = Arc<RwLock<Option<(Instant, serde_json::Value)>>>;

/// A fresh, empty facets cache.
#[must_use]
pub fn new_cache() -> FacetsCache {
    Arc::new(RwLock::new(None))
}

/// How long an assembled `/v1/facets` body is served before re-querying.
const TTL: Duration = Duration::from_secs(60);
/// Per-facet cap on enumerated open-set values for high-cardinality facets.
const VALUE_CAP: i64 = 200;

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

    // Serve a fresh-enough cached body if present.
    if let Some(body) = cached_body(&state.facets_cache) {
        return Json(body).into_response();
    }

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

    let body = json!({
        "modes": ["hybrid", "vector", "fts"],
        "filters": filters,
    });

    // Two concurrent cold-cache misses may both query + write; last-writer-wins
    // is fine (bodies are equivalent) and avoids holding a lock across the await.
    if let Ok(mut guard) = state.facets_cache.write() {
        *guard = Some((Instant::now(), body.clone()));
    }
    Json(body).into_response()
}

/// Return a clone of the cached body if it exists and is within the TTL.
/// Splitting this out keeps the read-lock guard's scope tight (avoids holding
/// it across the DB query / body assembly). The guard is consumed inside the
/// single `and_then`, so the lock is released before the function returns
/// (satisfies `clippy::significant_drop_tightening`). A poisoned lock is
/// treated as a cache miss rather than a panic.
fn cached_body(cache: &FacetsCache) -> Option<serde_json::Value> {
    cache.read().ok().and_then(|guard| {
        guard
            .as_ref()
            .filter(|(at, _)| at.elapsed() < TTL)
            .map(|(_, body)| body.clone())
    })
}

/// Bounded distinct values for open-set facets, keyed by facet name.
struct OpenValues {
    /// The enumerated values (≤ `VALUE_CAP` for high-cardinality facets).
    values: serde_json::Value,
    /// `true` when more distinct values exist than are listed in `values`.
    truncated: bool,
    /// Count of distinct values found, capped at `VALUE_CAP + 1` for
    /// high-cardinality facets — so when `truncated` is true this reads as
    /// "`VALUE_CAP + 1` or more", not the exact cardinality.
    total: i64,
}

/// Corpus-derived open-set values, queried from the active source versions.
///
/// Low-cardinality facets (`language`, `source_slug`) are enumerated in full;
/// high-cardinality facets (`tags`, `package`) are capped at [`VALUE_CAP`] by
/// document frequency and flagged `truncated` when more exist. `symbol` and
/// `heading_path` are intentionally NOT enumerated (extreme cardinality; the
/// facet type plus `symbol.kind`'s closed enum suffice for agents) — they
/// advertise their type only.
///
/// # Errors
///
/// Returns the underlying [`sqlx::Error`] if any DISTINCT query fails; the
/// handler maps that to a 503 so a transient DB hiccup doesn't surface as a 500.
async fn corpus_values(
    pool: &sqlx::PgPool,
) -> Result<std::collections::HashMap<String, OpenValues>, sqlx::Error> {
    use sqlx::Row as _;
    let mut out = std::collections::HashMap::new();

    // language (low cardinality, no cap)
    let rows = sqlx::query(
        "SELECT DISTINCT d.language FROM document d \
         JOIN source_version sv ON sv.id = d.source_version_id \
         WHERE sv.is_active = true AND d.language IS NOT NULL ORDER BY d.language",
    )
    .fetch_all(pool)
    .await?;
    let langs: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("language").ok())
        .collect();
    out.insert(
        "language".into(),
        OpenValues {
            total: i64::try_from(langs.len()).unwrap_or(i64::MAX),
            truncated: false,
            values: serde_json::json!(langs),
        },
    );

    // source_slug (no cap)
    let rows =
        sqlx::query("SELECT s.slug FROM source s WHERE s.retired_at IS NULL ORDER BY s.slug")
            .fetch_all(pool)
            .await?;
    let slugs: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("slug").ok())
        .collect();
    out.insert(
        "source_slug".into(),
        OpenValues {
            total: i64::try_from(slugs.len()).unwrap_or(i64::MAX),
            truncated: false,
            values: serde_json::json!(slugs),
        },
    );

    // tags (high cardinality -> top-N by document frequency)
    let rows = sqlx::query(
        "SELECT tag, count(*) AS n FROM document d \
         JOIN source_version sv ON sv.id = d.source_version_id \
         CROSS JOIN LATERAL jsonb_array_elements_text(COALESCE(d.provenance->'tags','[]'::jsonb)) AS tag \
         WHERE sv.is_active = true GROUP BY tag ORDER BY n DESC, tag LIMIT $1",
    )
    .bind(VALUE_CAP + 1)
    .fetch_all(pool)
    .await?;
    let mut tags: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("tag").ok())
        .collect();
    // Capture the count BEFORE truncate. The query is LIMIT VALUE_CAP + 1, so
    // this is at most VALUE_CAP + 1 (201) — i.e. "201 or more" when truncated.
    let total = i64::try_from(tags.len()).unwrap_or(i64::MAX);
    let truncated = tags.len() > usize::try_from(VALUE_CAP).unwrap_or(usize::MAX);
    tags.truncate(usize::try_from(VALUE_CAP).unwrap_or(usize::MAX));
    out.insert(
        "tags".into(),
        OpenValues {
            total,
            truncated,
            values: serde_json::json!(tags),
        },
    );

    // package names (top-N)
    let rows = sqlx::query(
        "SELECT p.name, count(*) AS n FROM package p \
         JOIN source_version sv ON sv.id = p.source_version_id \
         WHERE sv.is_active = true GROUP BY p.name ORDER BY n DESC, p.name LIMIT $1",
    )
    .bind(VALUE_CAP + 1)
    .fetch_all(pool)
    .await?;
    let mut pkgs: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("name").ok())
        .collect();
    // Capture the count BEFORE truncate (see the tags block above).
    let pkg_total = i64::try_from(pkgs.len()).unwrap_or(i64::MAX);
    let pkg_trunc = pkgs.len() > usize::try_from(VALUE_CAP).unwrap_or(usize::MAX);
    pkgs.truncate(usize::try_from(VALUE_CAP).unwrap_or(usize::MAX));
    out.insert(
        "package".into(),
        OpenValues {
            total: pkg_total,
            truncated: pkg_trunc,
            values: serde_json::json!(pkgs),
        },
    );

    Ok(out)
}

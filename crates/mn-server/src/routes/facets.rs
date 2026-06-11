//! `GET /v1/facets` — advertise the filterable facets, their types, and (for
//! closed enums) their allowed values, so clients can construct valid filters.
//! Open-set values are filled from the active corpus in `corpus_values` and
//! served from a short-lived TTL cache held in [`crate::app::AppState`].
//!
//! Two modes:
//! - **Overview** (no params): the cached facet catalogue with ≤`SAMPLE_CAP`
//!   sample values per open-set facet plus exact `total` counts.
//! - **Drill-down** (`?facet=<key>&cursor=&limit=`): a keyset-paginated value
//!   list for one drillable open-set facet, never cached.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use mn_retrieval::facets::{self, FacetType};
use serde::Deserialize;
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
/// Per-facet cap on sample values listed in the overview body. Full value
/// lists are available via the `?facet=` drill-down.
const SAMPLE_CAP: i64 = 10;
/// Drill-down page size when `limit` is omitted.
const DRILL_DEFAULT_LIMIT: i64 = 50;
/// Hard cap on drill-down page size.
const DRILL_MAX_LIMIT: i64 = 200;

/// Mount the `GET /v1/facets` route.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/facets", get(get_facets))
}

#[derive(Debug, Deserialize)]
struct FacetsQuery {
    /// When present: return a paginated value list for this one facet.
    facet: Option<String>,
    /// Opaque resume token from a previous drill-down page's `next_cursor`.
    cursor: Option<String>,
    /// Drill-down page size, 1..=[`DRILL_MAX_LIMIT`].
    limit: Option<i64>,
}

async fn get_facets(
    State(state): State<AppState>,
    Query(q): Query<FacetsQuery>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();

    // Drill-down mode bypasses the overview cache entirely: it must never be
    // answered FROM the cached overview body, and must never write INTO it.
    if let Some(f) = q.facet.as_deref() {
        return facet_values_page(&state, rid, f, q.cursor.as_deref(), q.limit).await;
    }

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

/// Exact distinct-value count for the `tags` facet. Shared by the drill-down
/// and the overview so both report the same `total` (identical join/filter
/// shape — drift here would make the numbers lie).
const TAGS_COUNT_SQL: &str = "SELECT count(DISTINCT tag) FROM document d \
     JOIN source_version sv ON sv.id = d.source_version_id \
     CROSS JOIN LATERAL jsonb_array_elements_text(COALESCE(d.provenance->'tags','[]'::jsonb)) AS tag \
     WHERE sv.is_active = true";

/// Exact distinct-value count for the `package` facet (see [`TAGS_COUNT_SQL`]
/// on why this is shared between drill-down and overview).
const PACKAGE_COUNT_SQL: &str = "SELECT count(DISTINCT p.name) FROM package p \
     JOIN source_version sv ON sv.id = p.source_version_id \
     WHERE sv.is_active = true";

/// Per-facet `(sql_page, sql_count)` for the drillable open-set facets. The
/// page query takes (`$1` = after-value keyset bound, `$2` = limit+1) and
/// yields a `v` text column ordered ascending. Join/filter shapes mirror the
/// corresponding `corpus_values` queries exactly so totals agree. `symbol` and
/// `heading_path` are intentionally not drillable (extreme cardinality, same
/// rationale as their omission from the overview).
fn drill_queries(facet: &str) -> Option<(&'static str, &'static str)> {
    match facet {
        "source_slug" => Some((
            "SELECT s.slug AS v FROM source s WHERE s.retired_at IS NULL AND s.slug > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(*) FROM source s WHERE s.retired_at IS NULL",
        )),
        "language" => Some((
            "SELECT DISTINCT d.language AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             WHERE sv.is_active = true AND d.language IS NOT NULL AND d.language > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT d.language) FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             WHERE sv.is_active = true AND d.language IS NOT NULL",
        )),
        "tags" => Some((
            "SELECT DISTINCT tag AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements_text(COALESCE(d.provenance->'tags','[]'::jsonb)) AS tag \
             WHERE sv.is_active = true AND tag > $1 ORDER BY v LIMIT $2",
            TAGS_COUNT_SQL,
        )),
        "package" => Some((
            "SELECT DISTINCT p.name AS v FROM package p \
             JOIN source_version sv ON sv.id = p.source_version_id \
             WHERE sv.is_active = true AND p.name > $1 ORDER BY v LIMIT $2",
            PACKAGE_COUNT_SQL,
        )),
        _ => None,
    }
}

/// Drill-down mode: one keyset-paginated page of distinct values for a single
/// drillable facet, ordered by value text. The cursor is the opaque encoding
/// of the last value on the previous page; `""` (the no-cursor bound) sorts
/// before all non-empty text, so page one starts at the smallest value.
async fn facet_values_page(
    state: &AppState,
    rid: &str,
    facet: &str,
    cursor: Option<&str>,
    limit: Option<i64>,
) -> Response {
    use sqlx::Row as _;
    let limit = limit.unwrap_or(DRILL_DEFAULT_LIMIT);
    if !(1..=DRILL_MAX_LIMIT).contains(&limit) {
        return error::bad_request(
            format!("limit must be in 1..={DRILL_MAX_LIMIT}"),
            "pass `limit` between 1 and 200, or omit it for the default",
            rid,
        );
    }
    let Some((page_sql, count_sql)) = drill_queries(facet) else {
        return error::bad_request(
            format!("facet `{facet}` is not drillable"),
            "drillable facets: source_slug, language, tags, package \
             (closed-enum facets list all values in the overview)",
            rid,
        );
    };
    let after = match cursor {
        None => String::new(), // "" sorts before all non-empty text
        Some(c) => match crate::pagination::decode_cursor(c) {
            Some(s) => s,
            None => {
                return error::bad_request(
                    "cursor is malformed",
                    "pass the `next_cursor` value from the previous page verbatim",
                    rid,
                )
            }
        },
    };
    let total: i64 = match sqlx::query_scalar(count_sql).fetch_one(&state.pool).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "facet count failed");
            return error::service_unavailable("facet value count failed", rid);
        }
    };
    // Over-fetch by one row to learn whether another page exists without a
    // second query.
    let rows = match sqlx::query(page_sql)
        .bind(&after)
        .bind(limit.saturating_add(1))
        .fetch_all(&state.pool)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "facet page failed");
            return error::service_unavailable("facet value page failed", rid);
        }
    };
    let mut values: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("v").ok())
        .collect();
    let has_more = values.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    values.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next_cursor = if has_more {
        values.last().map(|v| crate::pagination::encode_cursor(v))
    } else {
        None
    };
    Json(json!({
        "facet": facet,
        "values": values,
        "total": total,
        "next_cursor": next_cursor,
    }))
    .into_response()
}

/// Bounded sample values for open-set facets, keyed by facet name.
struct OpenValues {
    /// The sampled values (≤ [`SAMPLE_CAP`]).
    values: serde_json::Value,
    /// `true` when more distinct values exist than are listed in `values`
    /// (fetch the rest via the `?facet=` drill-down).
    truncated: bool,
    /// Exact count of distinct values in the active corpus.
    total: i64,
}

/// Corpus-derived open-set value samples, queried from the active source
/// versions.
///
/// Every open-set facet lists at most [`SAMPLE_CAP`] sample values alongside
/// an exact `total`: `language` / `source_slug` are enumerated in full and
/// truncated in-process; `tags` / `package` sample the top-N by document
/// frequency with a separate exact `count(DISTINCT …)`. `symbol` and
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

    let sample_cap = usize::try_from(SAMPLE_CAP).unwrap_or(usize::MAX);

    // language (low cardinality: enumerate in full, sample in-process)
    let rows = sqlx::query(
        "SELECT DISTINCT d.language FROM document d \
         JOIN source_version sv ON sv.id = d.source_version_id \
         WHERE sv.is_active = true AND d.language IS NOT NULL ORDER BY d.language",
    )
    .fetch_all(pool)
    .await?;
    let mut langs: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("language").ok())
        .collect();
    // Count BEFORE truncating to the sample cap: this is the exact total.
    let lang_total = i64::try_from(langs.len()).unwrap_or(i64::MAX);
    let lang_trunc = langs.len() > sample_cap;
    langs.truncate(sample_cap);
    out.insert(
        "language".into(),
        OpenValues {
            total: lang_total,
            truncated: lang_trunc,
            values: serde_json::json!(langs),
        },
    );

    // source_slug (low cardinality: enumerate in full, sample in-process)
    let rows =
        sqlx::query("SELECT s.slug FROM source s WHERE s.retired_at IS NULL ORDER BY s.slug")
            .fetch_all(pool)
            .await?;
    let mut slugs: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("slug").ok())
        .collect();
    // Count BEFORE truncating (see the language block above).
    let slug_total = i64::try_from(slugs.len()).unwrap_or(i64::MAX);
    let slug_trunc = slugs.len() > sample_cap;
    slugs.truncate(sample_cap);
    out.insert(
        "source_slug".into(),
        OpenValues {
            total: slug_total,
            truncated: slug_trunc,
            values: serde_json::json!(slugs),
        },
    );

    // tags (high cardinality -> exact distinct count + top-N sample by
    // document frequency)
    let tag_total: i64 = sqlx::query_scalar(TAGS_COUNT_SQL).fetch_one(pool).await?;
    let rows = sqlx::query(
        "SELECT tag, count(*) AS n FROM document d \
         JOIN source_version sv ON sv.id = d.source_version_id \
         CROSS JOIN LATERAL jsonb_array_elements_text(COALESCE(d.provenance->'tags','[]'::jsonb)) AS tag \
         WHERE sv.is_active = true GROUP BY tag ORDER BY n DESC, tag LIMIT $1",
    )
    .bind(SAMPLE_CAP)
    .fetch_all(pool)
    .await?;
    let tags: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("tag").ok())
        .collect();
    out.insert(
        "tags".into(),
        OpenValues {
            total: tag_total,
            truncated: tag_total > SAMPLE_CAP,
            values: serde_json::json!(tags),
        },
    );

    // package names (exact distinct count + top-N sample by frequency)
    let pkg_total: i64 = sqlx::query_scalar(PACKAGE_COUNT_SQL)
        .fetch_one(pool)
        .await?;
    let rows = sqlx::query(
        "SELECT p.name, count(*) AS n FROM package p \
         JOIN source_version sv ON sv.id = p.source_version_id \
         WHERE sv.is_active = true GROUP BY p.name ORDER BY n DESC, p.name LIMIT $1",
    )
    .bind(SAMPLE_CAP)
    .fetch_all(pool)
    .await?;
    let pkgs: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("name").ok())
        .collect();
    out.insert(
        "package".into(),
        OpenValues {
            total: pkg_total,
            truncated: pkg_total > SAMPLE_CAP,
            values: serde_json::json!(pkgs),
        },
    );

    Ok(out)
}

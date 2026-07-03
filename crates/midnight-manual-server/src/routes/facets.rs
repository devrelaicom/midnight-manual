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
use mnm_retrieval::facets::{self, FacetType};
use serde::Deserialize;
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

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

// --- Cold-start `corpus` overview caps (issue #139) ---------------------------
// Every list in the no-arg `corpus` block is hard-capped so the block stays
// within a small serialized budget (≤2 KB, asserted by the unit tests below).
/// Cap on `corpus.languages` (top-N languages by document count). Typed `i64`
/// because it is bound directly as a SQL `LIMIT`; the others cap in-process and
/// are `usize`.
const CORPUS_LANGUAGES_CAP: i64 = 10;
/// Cap on `corpus.version_coverage` targets (top-N by document count).
const CORPUS_VERSION_TARGETS_CAP: usize = 6;
/// Cap on declared constraints listed per `version_coverage` target.
const CORPUS_CONSTRAINTS_PER_TARGET_CAP: usize = 5;

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
    /// Second drill level: the level-1 value to enumerate within (e.g. the
    /// language-target name, the `kind:name` dependency composite, or the
    /// package name).
    within: Option<String>,
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
        return facet_values_page(
            &state,
            rid,
            f,
            q.cursor.as_deref(),
            q.limit,
            q.within.as_deref(),
        )
        .await;
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
            // Advertise the two-level drill ordering for facets that support a
            // `within` second drill (spec §4): level-1 enumerates the name,
            // level-2 the version value within it. Drives discoverability of
            // the `?facet=…&within=…` path without the client guessing keys.
            if let Some(levels) = match d.key {
                "language_target" | "sdk_dependency" => Some(["name", "version_constraint"]),
                "package" => Some(["name", "version"]),
                _ => None,
            } {
                entry["drill_levels"] = json!(levels);
            }
            entry
        })
        .collect();

    // Cold-start overview block (issue #139). Rides this same 60s cache — it is
    // only ever built here, on an overview cache miss, never on a drill-down.
    let corpus = match corpus_overview(&state.pool, &open).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "corpus overview query failed");
            return error::service_unavailable("corpus overview failed", rid);
        }
    };

    let body = json!({
        "modes": ["hybrid", "vector", "fts"],
        "filters": filters,
        "corpus": corpus,
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

/// Per-facet drill SQL. Level 1 (`within = false`) enumerates names; level 2
/// (`within = true`) enumerates version values inside one name (spec §4).
/// Returns `(page_sql, count_sql, takes_within)`.
///
/// The page query takes (`$1` = after-value keyset bound, `$2` = limit+1) and
/// yields a `v` text column ordered ascending; level-2 page queries also take
/// `$3` = the `within` value. The count query takes no binds for level 1 and
/// `$1` = the `within` value for level 2. Join/filter shapes mirror the
/// corresponding `corpus_values` queries exactly so totals agree. `symbol` and
/// `heading_path` are intentionally not drillable (extreme cardinality, same
/// rationale as their omission from the overview).
fn drill_queries(facet: &str, within: bool) -> Option<(&'static str, &'static str, bool)> {
    match (facet, within) {
        ("source_slug", false) => Some((
            "SELECT s.slug AS v FROM source s WHERE s.retired_at IS NULL AND s.slug > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(*) FROM source s WHERE s.retired_at IS NULL",
            false,
        )),
        ("language", false) => Some((
            "SELECT DISTINCT d.language AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             WHERE sv.is_active = true AND d.language IS NOT NULL AND d.language > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT d.language) FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             WHERE sv.is_active = true AND d.language IS NOT NULL",
            false,
        )),
        ("tags", false) => Some((
            "SELECT DISTINCT tag AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements_text(COALESCE(d.provenance->'tags','[]'::jsonb)) AS tag \
             WHERE sv.is_active = true AND tag > $1 ORDER BY v LIMIT $2",
            TAGS_COUNT_SQL,
            false,
        )),
        ("package", false) => Some((
            "SELECT DISTINCT p.name AS v FROM package p \
             JOIN source_version sv ON sv.id = p.source_version_id \
             WHERE sv.is_active = true AND p.name > $1 ORDER BY v LIMIT $2",
            PACKAGE_COUNT_SQL,
            false,
        )),
        ("package", true) => Some((
            "SELECT DISTINCT p.version AS v FROM package p \
             JOIN source_version sv ON sv.id = p.source_version_id \
             WHERE sv.is_active = true AND p.name = $3 AND p.version IS NOT NULL \
             AND p.version > $1 ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT p.version) FROM package p \
             JOIN source_version sv ON sv.id = p.source_version_id \
             WHERE sv.is_active = true AND p.name = $1 AND p.version IS NOT NULL",
            true,
        )),
        ("language_target", false) => Some((
            "SELECT DISTINCT lt->>'name' AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt \
             WHERE sv.is_active = true AND lt->>'name' IS NOT NULL AND lt->>'name' > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT lt->>'name') FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt \
             WHERE sv.is_active = true AND lt->>'name' IS NOT NULL",
            false,
        )),
        ("language_target", true) => Some((
            "SELECT DISTINCT lt->>'version_constraint' AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt \
             WHERE sv.is_active = true AND lt->>'name' = $3 \
             AND lt->>'version_constraint' IS NOT NULL AND lt->>'version_constraint' > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT lt->>'version_constraint') FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt \
             WHERE sv.is_active = true AND lt->>'name' = $1 \
             AND lt->>'version_constraint' IS NOT NULL",
            true,
        )),
        ("sdk_dependency", false) => Some((
            "SELECT DISTINCT (dep->>'kind') || ':' || (dep->>'name') AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'sdk_dependencies','[]'::jsonb)) dep \
             WHERE sv.is_active = true AND dep->>'name' IS NOT NULL \
             AND (dep->>'kind') || ':' || (dep->>'name') > $1 ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT (dep->>'kind') || ':' || (dep->>'name')) FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'sdk_dependencies','[]'::jsonb)) dep \
             WHERE sv.is_active = true AND dep->>'name' IS NOT NULL",
            false,
        )),
        ("sdk_dependency", true) => Some((
            "SELECT DISTINCT dep->>'version_constraint' AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'sdk_dependencies','[]'::jsonb)) dep \
             WHERE sv.is_active = true AND (dep->>'kind') || ':' || (dep->>'name') = $3 \
             AND dep->>'version_constraint' IS NOT NULL AND dep->>'version_constraint' > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT dep->>'version_constraint') FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'sdk_dependencies','[]'::jsonb)) dep \
             WHERE sv.is_active = true AND (dep->>'kind') || ':' || (dep->>'name') = $1 \
             AND dep->>'version_constraint' IS NOT NULL",
            true,
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
    within: Option<&str>,
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
    let Some((page_sql, count_sql, takes_within)) = drill_queries(facet, within.is_some()) else {
        // A `within` request on a facet that has no second drill level (e.g.
        // `language`, `tags`, `source_slug`) is a distinct error from a facet
        // that is not drillable at all.
        if within.is_some() && drill_queries(facet, false).is_some() {
            return error::bad_request(
                format!("facet `{facet}` has no `within` drill level"),
                "drop the `within` param, or drill a version facet: \
                 language_target, sdk_dependency, package",
                rid,
            );
        }
        return error::bad_request(
            format!("facet `{facet}` is not drillable"),
            "drillable facets: source_slug, language, tags, package, language_target, \
             sdk_dependency (closed-enum facets list all values in the overview)",
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
    // Level-2 drills bind the `within` value: as `$1` for the count query (its
    // only bind) and as `$3` for the page query (after `$1` = keyset, `$2` =
    // limit+1). `within` is `Some` whenever `takes_within` is true (the level-2
    // arms are only reachable via `within.is_some()`).
    let within = within.unwrap_or_default();
    let mut count_query = sqlx::query_scalar(count_sql);
    if takes_within {
        count_query = count_query.bind(within);
    }
    let total: i64 = match count_query.fetch_one(&state.pool).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "facet count failed");
            return error::service_unavailable("facet value count failed", rid);
        }
    };
    // Over-fetch by one row to learn whether another page exists without a
    // second query.
    let mut page_query = sqlx::query(page_sql)
        .bind(&after)
        .bind(limit.saturating_add(1));
    if takes_within {
        page_query = page_query.bind(within);
    }
    let rows = match page_query.fetch_all(&state.pool).await {
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
    let mut body = json!({
        "facet": facet,
        "values": values,
        "total": total,
        "next_cursor": next_cursor,
    });
    // Echo the level-2 anchor so callers can correlate a page with its drill
    // value. `takes_within` is true iff a level-2 arm matched, which only
    // happens when `within` was supplied.
    if takes_within {
        body["within"] = json!(within);
    }
    Json(body).into_response()
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

/// Map the attribution rank used by the `by_attribution` rollup back to its
/// wire name (mirrors `mnm_retrieval::facets::ATTRIBUTION_VALUES` and the
/// `Foundation(1) → Partner(2) → ThirdParty(3) → Community(4) → Unknown(5)`
/// ordering used by `mnm_store::entities::source`).
const fn attribution_rank_name(rank: i32) -> &'static str {
    match rank {
        1 => "foundation",
        2 => "partner",
        3 => "third_party",
        4 => "community",
        _ => "unknown",
    }
}

/// One `(version_constraint, document_count)` pair for a coverage target.
type ConstraintCount = (String, i64);
/// One coverage target: `(name, total_document_count, per-constraint counts)`.
type TargetCoverage = (String, i64, Vec<ConstraintCount>);

/// Pre-serialization inputs for the cold-start `corpus` overview block. Split
/// from the SQL in [`corpus_overview`] so the block's shape and serialized-size
/// budget can be unit-tested without a database.
struct CorpusInputs {
    /// Exact count of non-retired sources (reused from the `source_slug` total).
    sources_total: i64,
    /// `(source_kind, count)` over the same non-retired-source base set.
    by_kind: Vec<(String, i64)>,
    /// `(attribution, count)` — sources counted once under their representative
    /// (best/min-rank) document attribution, in rank order.
    by_attribution: Vec<(String, i64)>,
    /// Top languages by document count (already query-ordered, ≤ cap).
    languages: Vec<String>,
    /// `(target, declared_constraints)` from `language_target` provenance.
    version_coverage: Vec<(String, Vec<String>)>,
    /// RFC3339 min `ingested_at` over active source versions (`None` if empty).
    oldest_ingested_at: Option<String>,
    /// RFC3339 max `ingested_at` over active source versions (`None` if empty).
    newest_ingested_at: Option<String>,
    /// Top tags by document frequency (reused from the `tags` overview sample).
    tags_sample: serde_json::Value,
}

/// Rank the raw `(target, constraint, doc_count)` rows into the capped
/// `version_coverage` list: targets ordered by total document count, each
/// target's constraints ordered by document count, both hard-capped and with a
/// deterministic name/value tiebreak.
fn rank_version_coverage(rows: Vec<(String, String, i64)>) -> Vec<(String, Vec<String>)> {
    use std::collections::HashMap;
    let mut per_target: HashMap<String, (i64, Vec<ConstraintCount>)> = HashMap::new();
    for (name, vc, n) in rows {
        let e = per_target.entry(name).or_insert((0, Vec::new()));
        e.0 += n;
        e.1.push((vc, n));
    }
    let mut targets: Vec<TargetCoverage> = per_target
        .into_iter()
        .map(|(name, (total, vcs))| (name, total, vcs))
        .collect();
    // Top targets by total document count; deterministic tiebreak by name.
    targets.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    targets.truncate(CORPUS_VERSION_TARGETS_CAP);
    targets
        .into_iter()
        .map(|(name, _total, mut vcs)| {
            // Top constraints by document count; deterministic tiebreak by value.
            vcs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            vcs.truncate(CORPUS_CONSTRAINTS_PER_TARGET_CAP);
            (name, vcs.into_iter().map(|(vc, _)| vc).collect())
        })
        .collect()
}

/// Serialize [`CorpusInputs`] into the `corpus` JSON block, re-applying every
/// list cap defensively so the block stays within its budget regardless of what
/// the queries returned.
fn build_corpus_block(inputs: CorpusInputs) -> serde_json::Value {
    let to_map = |pairs: Vec<(String, i64)>| {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            m.insert(k, json!(v));
        }
        serde_json::Value::Object(m)
    };

    let mut languages = inputs.languages;
    languages.truncate(usize::try_from(CORPUS_LANGUAGES_CAP).unwrap_or(usize::MAX));

    let version_coverage: Vec<serde_json::Value> = inputs
        .version_coverage
        .into_iter()
        .take(CORPUS_VERSION_TARGETS_CAP)
        .map(|(target, mut constraints)| {
            constraints.truncate(CORPUS_CONSTRAINTS_PER_TARGET_CAP);
            json!({ "target": target, "declared_constraints": constraints })
        })
        .collect();

    json!({
        "sources": {
            "total": inputs.sources_total,
            "by_kind": to_map(inputs.by_kind),
            "by_attribution": to_map(inputs.by_attribution),
        },
        "languages": languages,
        "version_coverage": version_coverage,
        "freshness": {
            "oldest_ingested_at": inputs.oldest_ingested_at,
            "newest_ingested_at": inputs.newest_ingested_at,
        },
        "tags_sample": inputs.tags_sample,
    })
}

/// Assemble the cold-start `corpus` overview block for the NO-ARG facets
/// response (issue #139). Only ever called on an overview cache miss, so it
/// rides the same 60s [`FacetsCache`] as the rest of the body.
///
/// Sub-fields are derived from existing queries wherever possible:
/// - `sources.total` and `tags_sample` reuse the [`OpenValues`] already
///   computed by [`corpus_values`] (no extra query).
/// - `sources.by_kind` — one trivial GROUP BY over the small `source` table.
/// - `sources.by_attribution` — one aggregate mirroring the representative
///   (best/min-rank) attribution convention in `mnm_store::entities::source`;
///   every non-retired source is counted exactly once, so the counts sum to
///   `sources.total`.
/// - `languages` — top-N by document count (same frequency-sample shape the
///   `tags`/`package` overview samples use).
/// - `version_coverage` — declared `language_target` constraints, top targets
///   and constraints by document count (same join shape as the
///   `language_target` drill; combined into a single pass).
/// - `freshness` — min/max `ingested_at` over active source versions.
///
/// # Errors
///
/// Propagates the underlying [`sqlx::Error`]; the caller maps it to a 503 so a
/// transient DB hiccup doesn't surface as a 500 (same policy as the rest of the
/// overview).
async fn corpus_overview(
    pool: &sqlx::PgPool,
    open: &std::collections::HashMap<String, OpenValues>,
) -> Result<serde_json::Value, sqlx::Error> {
    use sqlx::Row as _;

    // Reuse what corpus_values already computed: source_slug is enumerated in
    // full (its `total` is exact) and `tags` is the top-N frequency sample.
    let sources_total = open.get("source_slug").map_or(0, |o| o.total);
    let tags_sample = open
        .get("tags")
        .map_or_else(|| json!([]), |o| o.values.clone());

    // by_kind: cheap GROUP BY over the same non-retired-source base set as
    // source_slug, so the counts sum to sources_total.
    //
    // Note: these three source rollups (`total` via corpus_values, `by_kind`,
    // `by_attribution`) are read in separate, non-transactional statements, so
    // the advertised "each sums to total" holds only under a stable corpus —
    // it is an eventually-consistent property of this advisory, cache-backed
    // overview, not a hard transactional guarantee across a concurrent ingest.
    //
    // Decode failures are propagated (not filtered away) so a future column-type
    // drift fails LOUD as the 503 the caller already returns, rather than
    // silently dropping rows and quietly breaking the sum==total invariant.
    let rows = sqlx::query(
        "SELECT s.kind AS k, count(*) AS n FROM source s \
         WHERE s.retired_at IS NULL GROUP BY s.kind ORDER BY s.kind",
    )
    .fetch_all(pool)
    .await?;
    let mut by_kind: Vec<(String, i64)> = Vec::with_capacity(rows.len());
    for r in &rows {
        by_kind.push((r.try_get::<String, _>("k")?, r.try_get::<i64, _>("n")?));
    }

    // by_attribution: each non-retired source counted once under its
    // representative (best/min-rank) document attribution. A source with no
    // active version / no documents ranks as `unknown` (rank 5) via the LEFT
    // JOINs, so these counts also sum to sources_total.
    let rows = sqlx::query(
        "SELECT rank, count(*) AS n FROM ( \
            SELECT MIN(CASE d.provenance->>'attribution' \
                         WHEN 'foundation' THEN 1 WHEN 'partner' THEN 2 \
                         WHEN 'third_party' THEN 3 WHEN 'community' THEN 4 ELSE 5 END) AS rank \
            FROM source s \
            LEFT JOIN source_version sv ON sv.source_id = s.id AND sv.is_active = true \
            LEFT JOIN document d ON d.source_version_id = sv.id \
            WHERE s.retired_at IS NULL GROUP BY s.id \
         ) t GROUP BY rank ORDER BY rank",
    )
    .fetch_all(pool)
    .await?;
    // Decode failures propagate (same invariant-protection rationale as by_kind).
    let mut by_attribution: Vec<(String, i64)> = Vec::with_capacity(rows.len());
    for r in &rows {
        let rank = r.try_get::<i32, _>("rank")?;
        let n = r.try_get::<i64, _>("n")?;
        by_attribution.push((attribution_rank_name(rank).to_owned(), n));
    }

    // languages: top-N by document count.
    let rows = sqlx::query(
        "SELECT d.language AS lang, count(*) AS n FROM document d \
         JOIN source_version sv ON sv.id = d.source_version_id \
         WHERE sv.is_active = true AND d.language IS NOT NULL \
         GROUP BY d.language ORDER BY n DESC, d.language LIMIT $1",
    )
    .bind(CORPUS_LANGUAGES_CAP)
    .fetch_all(pool)
    .await?;
    let languages: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("lang").ok())
        .collect();

    // version_coverage: declared language_target constraints, one (name,
    // constraint, doc_count) row per pair; rank_version_coverage caps + orders.
    // Intentionally no SQL LIMIT: targets are ranked by their summed count
    // ACROSS constraints, which needs the full grouped set in hand before the
    // cap can be applied. The intermediate is bounded by the curated corpus's
    // (name, constraint) cardinality, which is small.
    let rows = sqlx::query(
        "SELECT lt->>'name' AS name, lt->>'version_constraint' AS vc, count(*) AS n \
         FROM document d \
         JOIN source_version sv ON sv.id = d.source_version_id \
         CROSS JOIN LATERAL jsonb_array_elements(COALESCE(d.provenance->'language_targets','[]'::jsonb)) lt \
         WHERE sv.is_active = true AND lt->>'name' IS NOT NULL \
         AND lt->>'version_constraint' IS NOT NULL \
         GROUP BY name, vc",
    )
    .fetch_all(pool)
    .await?;
    let coverage_rows: Vec<(String, String, i64)> = rows
        .iter()
        .filter_map(|r| {
            Some((
                r.try_get::<String, _>("name").ok()?,
                r.try_get::<String, _>("vc").ok()?,
                r.try_get::<i64, _>("n").ok()?,
            ))
        })
        .collect();
    let version_coverage = rank_version_coverage(coverage_rows);

    // freshness: min/max ingested_at across active source versions.
    let row = sqlx::query(
        "SELECT min(ingested_at) AS oldest, max(ingested_at) AS newest \
         FROM source_version WHERE is_active = true",
    )
    .fetch_one(pool)
    .await?;
    let fmt = |t: Option<OffsetDateTime>| t.and_then(|t| t.format(&Rfc3339).ok());
    let oldest = fmt(row
        .try_get::<Option<OffsetDateTime>, _>("oldest")
        .ok()
        .flatten());
    let newest = fmt(row
        .try_get::<Option<OffsetDateTime>, _>("newest")
        .ok()
        .flatten());

    Ok(build_corpus_block(CorpusInputs {
        sources_total,
        by_kind,
        by_attribution,
        languages,
        version_coverage,
        oldest_ingested_at: oldest,
        newest_ingested_at: newest,
        tags_sample,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `CorpusInputs` filled to (or past) every list cap with realistically-
    /// sized strings — the worst realistic case for the serialized-size budget.
    /// Languages / version_coverage are deliberately over-cap so the defensive
    /// truncation in [`build_corpus_block`] is exercised.
    fn max_inputs() -> CorpusInputs {
        let by_kind = vec![
            ("code_repo".to_owned(), 61),
            ("docs_site".to_owned(), 14),
            ("mixed".to_owned(), 5),
            ("standalone".to_owned(), 2),
        ];
        let by_attribution = vec![
            ("foundation".to_owned(), 12),
            ("partner".to_owned(), 8),
            ("third_party".to_owned(), 20),
            ("community".to_owned(), 40),
            ("unknown".to_owned(), 2),
        ];
        let languages = (0..CORPUS_LANGUAGES_CAP + 5)
            .map(|i| format!("language-name-{i:02}"))
            .collect();
        let version_coverage = (0..CORPUS_VERSION_TARGETS_CAP + 3)
            .map(|t| {
                let constraints = (0..CORPUS_CONSTRAINTS_PER_TARGET_CAP + 3)
                    .map(|c| format!(">=0.{t}{c}.0"))
                    .collect();
                (format!("target-name-{t:02}"), constraints)
            })
            .collect();
        let tags_sample = json!((0..10)
            .map(|i| format!("tag-sample-{i:02}"))
            .collect::<Vec<_>>());
        CorpusInputs {
            sources_total: 82,
            by_kind,
            by_attribution,
            languages,
            version_coverage,
            oldest_ingested_at: Some("2026-01-02T03:04:05Z".to_owned()),
            newest_ingested_at: Some("2026-07-02T03:04:05Z".to_owned()),
            tags_sample,
        }
    }

    #[test]
    fn corpus_block_respects_caps_and_shape() {
        let block = build_corpus_block(max_inputs());
        assert_eq!(block["sources"]["total"], json!(82));
        assert_eq!(block["sources"]["by_kind"]["code_repo"], json!(61));
        assert_eq!(block["sources"]["by_attribution"]["community"], json!(40));
        // languages hard-capped.
        assert_eq!(
            block["languages"].as_array().unwrap().len(),
            usize::try_from(CORPUS_LANGUAGES_CAP).unwrap()
        );
        // version_coverage targets + per-target constraints hard-capped.
        let vc = block["version_coverage"].as_array().unwrap();
        assert_eq!(vc.len(), CORPUS_VERSION_TARGETS_CAP);
        for t in vc {
            assert!(t["target"].is_string(), "{t}");
            assert!(
                t["declared_constraints"].as_array().unwrap().len()
                    <= CORPUS_CONSTRAINTS_PER_TARGET_CAP,
                "{t}"
            );
        }
        assert!(block["freshness"]["oldest_ingested_at"].is_string());
        assert!(block["freshness"]["newest_ingested_at"].is_string());
        assert_eq!(block["tags_sample"].as_array().unwrap().len(), 10);
    }

    #[test]
    fn corpus_block_within_2kb_budget() {
        // Acceptance criterion: the block stays ≤2 KB serialized even at every
        // cap. Measured on the worst-realistic-case fixture above.
        let block = build_corpus_block(max_inputs());
        let bytes = serde_json::to_vec(&block)
            .expect("serialize corpus block")
            .len();
        assert!(
            bytes <= 2048,
            "corpus block must stay within the 2 KB budget, got {bytes} bytes"
        );
    }

    #[test]
    fn empty_corpus_block_is_well_formed() {
        // A brand-new / empty corpus yields zero counts and null freshness, not
        // missing keys — agents can rely on the shape.
        let block = build_corpus_block(CorpusInputs {
            sources_total: 0,
            by_kind: Vec::new(),
            by_attribution: Vec::new(),
            languages: Vec::new(),
            version_coverage: Vec::new(),
            oldest_ingested_at: None,
            newest_ingested_at: None,
            tags_sample: json!([]),
        });
        assert_eq!(block["sources"]["total"], json!(0));
        assert_eq!(block["sources"]["by_kind"], json!({}));
        assert_eq!(block["languages"], json!([]));
        assert_eq!(block["version_coverage"], json!([]));
        assert!(block["freshness"]["oldest_ingested_at"].is_null());
        assert_eq!(block["tags_sample"], json!([]));
    }

    #[test]
    fn rank_version_coverage_orders_by_frequency() {
        // The higher total-count target sorts first; within a target the higher-
        // count constraint sorts first.
        let rows = vec![
            ("compact".to_owned(), ">=0.23".to_owned(), 5),
            ("compact".to_owned(), "0.31".to_owned(), 50),
            ("typescript".to_owned(), "^5".to_owned(), 3),
        ];
        let out = rank_version_coverage(rows);
        assert_eq!(out[0].0, "compact");
        assert_eq!(out[0].1[0], "0.31", "higher-count constraint first");
        assert_eq!(out[1].0, "typescript");
    }

    #[test]
    fn rank_version_coverage_caps_targets_and_constraints_keeping_highest_count() {
        // Feed MORE targets than the target cap, and on the surviving top target
        // MORE constraints than the per-target cap — so BOTH `truncate`s fire —
        // then assert the highest-count entries are the ones kept.
        let n_targets = CORPUS_VERSION_TARGETS_CAP + 3;
        let n_constraints = CORPUS_CONSTRAINTS_PER_TARGET_CAP + 3;
        let mut rows: Vec<(String, String, i64)> = Vec::new();

        // t00 dominates via its own constraint set (counts 1000, 990, … each
        // distinct + descending, so c00 is the highest). Its summed total (~7.7k)
        // exceeds every other target below, so it sorts first.
        for c in 0..n_constraints {
            #[allow(clippy::cast_possible_wrap)]
            let count = 1000 - (c as i64) * 10;
            rows.push(("t00".to_owned(), format!("c{c:02}"), count));
        }
        // t01..t{N-1}: one constraint each, totals descending and all < t00's,
        // so the target cap keeps t00 + the highest few and drops the tail.
        for t in 1..n_targets {
            #[allow(clippy::cast_possible_wrap)]
            let count = 800 - (t as i64) * 10;
            rows.push((format!("t{t:02}"), "only".to_owned(), count));
        }

        let out = rank_version_coverage(rows);

        // Target cap enforced, ordered by summed count (t00 first).
        assert_eq!(out.len(), CORPUS_VERSION_TARGETS_CAP, "target cap must fire");
        assert_eq!(out[0].0, "t00", "highest-total target survives + sorts first");
        let survivors: std::collections::HashSet<&str> =
            out.iter().map(|(name, _)| name.as_str()).collect();
        assert!(
            !survivors.contains(format!("t{:02}", n_targets - 1).as_str()),
            "lowest-total target must be dropped by the cap: {survivors:?}"
        );

        // Per-target constraint cap enforced on t00, keeping the highest counts.
        let top = &out[0].1;
        assert_eq!(top.len(), CORPUS_CONSTRAINTS_PER_TARGET_CAP, "constraint cap must fire");
        assert_eq!(top[0], "c00", "highest-count constraint kept + first");
        assert!(
            !top.contains(&format!("c{:02}", n_constraints - 1)),
            "lowest-count constraint must be dropped by the cap: {top:?}"
        );
    }

    #[test]
    fn attribution_rank_names_cover_all_ranks() {
        assert_eq!(attribution_rank_name(1), "foundation");
        assert_eq!(attribution_rank_name(2), "partner");
        assert_eq!(attribution_rank_name(3), "third_party");
        assert_eq!(attribution_rank_name(4), "community");
        assert_eq!(attribution_rank_name(5), "unknown");
        assert_eq!(attribution_rank_name(99), "unknown");
    }
}

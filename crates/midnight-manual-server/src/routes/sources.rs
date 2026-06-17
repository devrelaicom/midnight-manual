//! `/v1/sources` — list and show. (Write endpoints land in Phase 7.)
//!
//! The list endpoint is keyset-paginated on `slug` (unique, matches the
//! `ORDER BY`) and returns `{sources, total, next_cursor}` — `next_cursor`
//! is an opaque token (see [`crate::pagination`]); `null` means last page.

use axum::extract::{Extension, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mnm_store::{entities::source, StoreError};
use serde::Deserialize;

use crate::app::AppState;
use crate::error;
use crate::middleware::request_id::RequestId;

/// Mount the sources read routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sources", get(list_sources))
        .route("/v1/sources/:slug", get(get_source))
}

/// Page size when `limit` is omitted.
const SOURCES_DEFAULT_LIMIT: i64 = 20;
/// Hard cap on page size.
const SOURCES_MAX_LIMIT: i64 = 100;

#[derive(Debug, Deserialize)]
struct SourcesQuery {
    /// Opaque resume token from a previous page's `next_cursor`.
    cursor: Option<String>,
    /// Page size, 1..=100. Defaults to [`SOURCES_DEFAULT_LIMIT`].
    limit: Option<i64>,
    /// RFC3339 timestamp; only sources created strictly after it.
    created_after: Option<String>,
    /// RFC3339 timestamp; only sources created strictly before it.
    created_before: Option<String>,
    /// Source-kind wire string (`docs_site` | `code_repo` | `standalone` | `mixed`).
    kind: Option<String>,
    /// Include retired sources (default false = active only).
    #[serde(default)]
    retired: bool,
}

/// Parse an optional RFC3339 query param. Exposed (crate-private) for unit tests.
fn parse_rfc3339(
    name: &str,
    v: Option<&str>,
) -> std::result::Result<Option<time::OffsetDateTime>, String> {
    v.map(|s| {
        time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
            .map_err(|_| format!("`{name}` must be an RFC3339 timestamp"))
    })
    .transpose()
}

async fn list_sources(
    Query(q): Query<SourcesQuery>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let limit = q.limit.unwrap_or(SOURCES_DEFAULT_LIMIT);
    if !(1..=SOURCES_MAX_LIMIT).contains(&limit) {
        return error::bad_request(
            format!("limit must be in 1..={SOURCES_MAX_LIMIT}"),
            "pass `limit` between 1 and 100, or omit it for the default",
            rid,
        );
    }
    let after_slug = match q.cursor.as_deref() {
        None => None,
        Some(c) => match crate::pagination::decode_cursor(c) {
            Some(s) => Some(s),
            None => {
                return error::bad_request(
                    "cursor is malformed",
                    "pass the `next_cursor` token from the previous page verbatim",
                    rid,
                )
            }
        },
    };
    let created_after = match parse_rfc3339("created_after", q.created_after.as_deref()) {
        Ok(v) => v,
        Err(m) => return error::bad_request(m, "e.g. `2026-01-01T00:00:00Z`", rid),
    };
    let created_before = match parse_rfc3339("created_before", q.created_before.as_deref()) {
        Ok(v) => v,
        Err(m) => return error::bad_request(m, "e.g. `2026-01-01T00:00:00Z`", rid),
    };
    // Validate `kind` against the enum's serde wire form so
    // `mnm_core::types::SourceKind` stays the single source of truth (the
    // remediation list below is display-only).
    if let Some(k) = q.kind.as_deref() {
        if serde_json::from_value::<mnm_core::types::SourceKind>(serde_json::Value::String(
            k.to_owned(),
        ))
        .is_err()
        {
            return error::bad_request(
                format!("`{k}` is not a known source kind"),
                "pass one of: docs_site, code_repo, standalone, mixed",
                rid,
            );
        }
    }
    let page_q = source::SourcePageQuery {
        after_slug,
        limit,
        created_after,
        created_before,
        kind: q.kind,
        include_retired: q.retired,
    };
    match source::list_paged(&state.pool, &page_q).await {
        Ok(page) => Json(serde_json::json!({
            "sources": page.sources,
            "total": page.total,
            "next_cursor": page.next_after_slug.as_deref().map(crate::pagination::encode_cursor),
        }))
        .into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "list_sources", error = %e, "store error");
            error::service_unavailable("list_sources failed", rid)
        }
    }
}

async fn get_source(
    Path(slug): Path<String>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    match source::get_by_slug(&state.pool, &slug).await {
        Ok(row) => Json(row).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("source `{slug}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "get_source", error = %e, "store error");
            error::service_unavailable("source lookup failed", rid)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rfc3339_accepts_valid_timestamp() {
        let parsed = parse_rfc3339("created_after", Some("2026-01-02T03:04:05Z"))
            .unwrap()
            .unwrap();
        assert_eq!(parsed.year(), 2026);
        assert_eq!(parsed.unix_timestamp(), 1_767_323_045);
    }

    #[test]
    fn parse_rfc3339_accepts_offset_form() {
        let parsed = parse_rfc3339("created_before", Some("2026-01-02T03:04:05+02:00"))
            .unwrap()
            .unwrap();
        assert_eq!(parsed.unix_timestamp(), 1_767_323_045 - 7200);
    }

    #[test]
    fn parse_rfc3339_none_passes_through() {
        assert_eq!(parse_rfc3339("created_after", None).unwrap(), None);
    }

    #[test]
    fn parse_rfc3339_rejects_garbage_with_param_name() {
        let err = parse_rfc3339("created_after", Some("yesterday")).unwrap_err();
        assert!(err.contains("`created_after`"), "{err}");
        // Date-only is not RFC3339 either.
        assert!(parse_rfc3339("created_before", Some("2026-01-02")).is_err());
    }
}

//! Admin source CRUD (Phase 12).
//!
//! Four endpoints for operator-side source registry management; all
//! admin-tier gated (FR-058 + FR-117).
//!
//! 1. `POST   /v1/admin/sources` — create a new source.
//! 2. `PATCH  /v1/admin/sources/:slug` — update display name, origin URL, or
//!    retention count. Each field is independently optional.
//! 3. `DELETE /v1/admin/sources/:slug` — retire a source (soft delete: sets
//!    `retired_at = now()`). Source versions and chunks are NOT cascaded;
//!    the retention sweep handles those out-of-band.
//! 4. `GET    /v1/admin/sources` — list every source including retired ones
//!    (the public `GET /v1/sources` filters retired rows for anonymous
//!    callers).

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{patch, post};
use axum::{Json, Router};
use mn_core::error::{Error as CoreError, ErrorCode};
use mn_core::model_id::EmbeddingModelId;
use mn_core::types::SourceKind;
use mn_store::entities::source::SourcePatch;
use mn_store::entities::{embedding_model, source};
use mn_store::StoreError;
use serde::Deserialize;

use crate::app::AppState;
use crate::error;
use crate::middleware::bearer::AuthContext;
use crate::middleware::request_id::RequestId;

/// Mount the admin sources routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/sources", post(create_source).get(list_sources))
        .route("/v1/admin/sources/:slug", patch(update_source).delete(retire_source))
}

/// Query parameters for `GET /v1/admin/sources`.
#[derive(Debug, Deserialize)]
pub struct ListSourcesQuery {
    /// When present, restrict the result to sources whose active version is
    /// NOT on this model. Wire format: `{name}@{revision}` (e.g.
    /// `voyage-code-3@1`).
    #[serde(default)]
    pub not_model: Option<String>,
}

/// Response shape for the `not_model` filtered list.
#[derive(Debug, serde::Serialize)]
pub struct SourcesNotOnModelResponse {
    /// Sources whose active version is not on the requested model.
    pub sources: Vec<SourceSummary>,
}

/// Compact source summary returned by the `not_model` filter.
#[derive(Debug, serde::Serialize)]
pub struct SourceSummary {
    /// URL-safe slug.
    pub slug: String,
    /// Canonical origin URL, if set.
    pub origin_url: Option<String>,
}

/// Body of `POST /v1/admin/sources`.
#[derive(Debug, Deserialize)]
pub struct CreateSourceRequest {
    /// URL-safe slug. Must match `^[a-z0-9][a-z0-9-]*[a-z0-9]$` and be at
    /// most 63 chars.
    pub slug: String,
    /// Human-readable label. Defaults to `slug` when omitted.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Source kind discriminator.
    pub kind: SourceKind,
    /// Canonical origin URL (git URL, docs site URL, etc.).
    #[serde(default)]
    pub origin_url: Option<String>,
    /// Historical-version retention count. Defaults to 5; clamped to
    /// `[1, 50]` per the DB CHECK constraint.
    #[serde(default)]
    pub retention_count: Option<i32>,
}

/// Body of `PATCH /v1/admin/sources/:slug`. All fields optional; an empty
/// body returns the current row unchanged.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateSourceRequest {
    /// New display label.
    #[serde(default)]
    pub display_name: Option<String>,
    /// New origin URL.
    #[serde(default)]
    pub origin_url: Option<String>,
    /// New retention count.
    #[serde(default)]
    pub retention_count: Option<i32>,
}

async fn create_source(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    Json(req): Json<CreateSourceRequest>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    if let Some(resp) = validate_slug(&req.slug, rid) {
        return resp;
    }
    let retention = req.retention_count.unwrap_or(5);
    if !(1..=50).contains(&retention) {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message("retention_count must be between 1 and 50 inclusive")
                .remediation("supply an integer in [1, 50] or omit the field for the default (5)")
                .build(),
            rid,
        );
    }
    let display_name = req
        .display_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&req.slug);

    match source::insert(
        &state.pool,
        &req.slug,
        display_name,
        req.kind,
        req.origin_url.as_deref(),
        retention,
    )
    .await
    {
        Ok(_) => match source::get_by_slug(&state.pool, &req.slug).await {
            Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
            Err(e) => {
                tracing::warn!(request_id = rid, op = "create_source", error = %e, "post-insert fetch failed");
                error::service_unavailable("source readback failed", rid)
            }
        },
        Err(StoreError::UniqueViolation(_)) => error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!("source slug `{}` is already registered", req.slug))
                .remediation("pick a different slug or PATCH the existing row")
                .build(),
            rid,
        ),
        Err(StoreError::CheckViolation(msg)) => error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(msg)
                .remediation("check the request body against the source schema constraints")
                .build(),
            rid,
        ),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "create_source", error = %e, "insert failed");
            error::service_unavailable("source insert failed", rid)
        }
    }
}

async fn update_source(
    Path(slug): Path<String>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    Json(req): Json<UpdateSourceRequest>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    if let Some(rc) = req.retention_count {
        if !(1..=50).contains(&rc) {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message("retention_count must be between 1 and 50 inclusive")
                    .remediation("supply an integer in [1, 50]")
                    .build(),
                rid,
            );
        }
    }

    let patch = SourcePatch {
        display_name: req.display_name,
        origin_url: req.origin_url,
        retention_count: req.retention_count,
    };

    match source::update(&state.pool, &slug, patch).await {
        Ok(row) => Json(row).into_response(),
        Err(StoreError::NotFound) => error::not_found(format!("source `{slug}` not found"), rid),
        Err(StoreError::CheckViolation(msg)) => error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(msg)
                .remediation("check the patch body against the source schema constraints")
                .build(),
            rid,
        ),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "update_source", error = %e, "update failed");
            error::service_unavailable("source update failed", rid)
        }
    }
}

async fn retire_source(
    Path(slug): Path<String>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    let existing = match source::get_by_slug(&state.pool, &slug).await {
        Ok(row) => row,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("source `{slug}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "retire_source", error = %e, "lookup failed");
            return error::service_unavailable("source lookup failed", rid);
        }
    };
    if existing.retired_at.is_some() {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!("source `{slug}` is already retired"))
                .remediation("retired sources are restored only by direct SQL")
                .build(),
            rid,
        );
    }

    match source::retire(&state.pool, &slug).await {
        Ok(()) => match source::get_by_slug(&state.pool, &slug).await {
            Ok(row) => Json(row).into_response(),
            Err(e) => {
                tracing::warn!(request_id = rid, op = "retire_source", error = %e, "readback failed");
                error::service_unavailable("source readback failed", rid)
            }
        },
        Err(StoreError::NotFound) => error::not_found(format!("source `{slug}` not found"), rid),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "retire_source", error = %e, "retire failed");
            error::service_unavailable("source retire failed", rid)
        }
    }
}

async fn list_sources(
    State(state): State<AppState>,
    Query(q): Query<ListSourcesQuery>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    if let Some(wire) = q.not_model {
        // Parse the wire id (e.g. "voyage-code-3@1") using EmbeddingModelId's
        // FromStr implementation so we get consistent validation.
        let model_id_parsed: EmbeddingModelId = match wire.parse() {
            Ok(id) => id,
            Err(e) => {
                return error::into_response(
                    CoreError::builder(ErrorCode::InvalidRequest)
                        .message(format!("invalid not_model `{wire}`: {e}"))
                        .remediation("use the wire format `name@revision`, e.g. `voyage-code-3@1`")
                        .build(),
                    rid,
                );
            }
        };
        // The revision is stored as i32 in the DB; EmbeddingModelId uses u32.
        // Overflow is practically impossible (revision would have to exceed 2^31)
        // but we guard it explicitly to avoid a silent cast.
        let Ok(revision) = i32::try_from(model_id_parsed.revision) else {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message(format!(
                        "revision {} is out of range for a stored model",
                        model_id_parsed.revision
                    ))
                    .remediation("revision must fit in a 32-bit signed integer")
                    .build(),
                rid,
            );
        };
        let model = match embedding_model::get_by_name_revision(
            &state.pool,
            &model_id_parsed.name,
            revision,
        )
        .await
        {
            Ok(m) => m,
            Err(StoreError::NotFound) => {
                return error::into_response(
                    CoreError::builder(ErrorCode::NotFound)
                        .message(format!("unknown model `{wire}`"))
                        .remediation("register the model first or check the wire id")
                        .build(),
                    rid,
                );
            }
            Err(e) => {
                tracing::warn!(request_id = rid, op = "list_admin_sources_not_model", error = %e, "model lookup failed");
                return error::service_unavailable("model lookup failed", rid);
            }
        };
        return match source::list_active_not_on_model(&state.pool, model.id).await {
            Ok(rows) => {
                let summaries: Vec<SourceSummary> = rows
                    .into_iter()
                    .map(|s| SourceSummary {
                        slug: s.slug,
                        origin_url: s.origin_url,
                    })
                    .collect();
                Json(SourcesNotOnModelResponse { sources: summaries }).into_response()
            }
            Err(e) => {
                tracing::warn!(request_id = rid, op = "list_admin_sources_not_model", error = %e, "query failed");
                error::service_unavailable("source list failed", rid)
            }
        };
    }

    match source::list_all(&state.pool).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "list_admin_sources", error = %e, "list failed");
            error::service_unavailable("source list failed", rid)
        }
    }
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
                .message("admin tier required for source registry writes")
                .remediation("read-uplift tokens may not write — request admin tier")
                .build(),
            rid,
        )),
    }
}

/// Validate a slug per `^[a-z0-9][a-z0-9-]*[a-z0-9]$` with `len <= 63`.
fn validate_slug(slug: &str, rid: &str) -> Option<Response> {
    let bad = |reason: &str| -> Response {
        let err = CoreError::builder(ErrorCode::InvalidRequest)
            .message(format!("invalid slug `{slug}`: {reason}"))
            .remediation("slugs match ^[a-z0-9][a-z0-9-]*[a-z0-9]$ and are at most 63 chars")
            .context("slug", slug.to_owned())
            .build();
        error::into_response(err, rid)
    };
    if slug.is_empty() {
        return Some(bad("must not be empty"));
    }
    if slug.len() > 63 {
        return Some(bad("must be at most 63 characters"));
    }
    let mut chars = slug.chars();
    let first = chars.next().expect("non-empty");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Some(bad("must start with [a-z0-9]"));
    }
    let last = slug.chars().last().expect("non-empty");
    if !(last.is_ascii_lowercase() || last.is_ascii_digit()) {
        return Some(bad("must end with [a-z0-9]"));
    }
    for c in slug.chars() {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Some(bad("may only contain [a-z0-9-]"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validator_accepts_valid_slugs() {
        assert!(validate_slug("docs", "rid").is_none());
        assert!(validate_slug("docs-v2", "rid").is_none());
        assert!(validate_slug("a1", "rid").is_none());
        assert!(validate_slug("a", "rid").is_none());
        assert!(validate_slug(&"a".repeat(63), "rid").is_none());
    }

    #[test]
    fn slug_validator_rejects_invalid_slugs() {
        assert!(validate_slug("", "rid").is_some());
        assert!(validate_slug("-leading", "rid").is_some());
        assert!(validate_slug("trailing-", "rid").is_some());
        assert!(validate_slug("UPPER", "rid").is_some());
        assert!(validate_slug("under_score", "rid").is_some());
        assert!(validate_slug("with space", "rid").is_some());
        assert!(validate_slug(&"a".repeat(64), "rid").is_some());
    }
}

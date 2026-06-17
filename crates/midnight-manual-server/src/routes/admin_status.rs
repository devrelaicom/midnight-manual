//! `GET /v1/admin/ingest/status` — admin-tier summary of the corpus's
//! ingest health (Phase 11c). Powers `mnm doctor` so operators can spot
//! incomplete embed work without running ad-hoc SQL.
//!
//! Returns:
//!
//! - The active embedding model identifier (mirror of `/v1/models/active`
//!   for one-stop diagnostics).
//! - One row per registered source: active source_version revision (or
//!   `None` when no version is active yet), total chunks, ready chunks,
//!   embed_failed chunks. Sources with no version at all return zeroes.

use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mnm_core::error::{Error as CoreError, ErrorCode};
use mnm_store::entities::{embedding_model, source};
use serde::Serialize;

use crate::app::AppState;
use crate::error;
use crate::middleware::bearer::AuthContext;
use crate::middleware::request_id::RequestId;

/// Mount the admin ingest-status route.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/admin/ingest/status", get(ingest_status))
}

/// Response shape for `GET /v1/admin/ingest/status`.
#[derive(Debug, Serialize)]
pub struct IngestStatusResponse {
    /// The corpus's active embedding model wire id (`name@revision`).
    pub active_embedding_model: String,
    /// Per-source summary. Ordered by `slug` ASC for stable rendering.
    pub sources: Vec<SourceStatus>,
}

/// One row in [`IngestStatusResponse::sources`].
#[derive(Debug, Serialize)]
pub struct SourceStatus {
    /// Source slug.
    pub slug: String,
    /// Active source_version's `revision`, or `None` when no version is
    /// currently active.
    pub active_revision: Option<i32>,
    /// Total chunks under the active source_version (0 when no SV is
    /// active).
    pub total_chunks: i64,
    /// Chunks with `status = 'ready'` — visible to search.
    pub ready_chunks: i64,
    /// Chunks with `status = 'embed_failed'` — pending embedder work.
    pub embed_failed_chunks: i64,
}

async fn ingest_status(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    match auth {
        Some(Extension(ctx)) if ctx.can_admin() => {}
        Some(_) => {
            return error::into_response(
                CoreError::builder(ErrorCode::Forbidden)
                    .message("admin tier required")
                    .remediation("read-uplift tokens may not read admin diagnostics")
                    .build(),
                rid,
            );
        }
        None => {
            return error::into_response(
                CoreError::builder(ErrorCode::Unauthorized)
                    .message("admin bearer required")
                    .remediation("obtain an admin token via `mnm login` and retry")
                    .build(),
                rid,
            );
        }
    }

    let active = match embedding_model::get_active(&state.pool).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "active model lookup failed");
            return error::service_unavailable("active model lookup failed", rid);
        }
    };
    let model_wire = format!("{}@{}", active.name, active.revision);

    let sources = match source::list_active(&state.pool).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "source list failed");
            return error::service_unavailable("source list failed", rid);
        }
    };

    let mut summaries: Vec<SourceStatus> = Vec::with_capacity(sources.len());
    for src in sources {
        let summary: Option<(i32, i64, i64, i64)> = match sqlx::query_as(
            "SELECT sv.revision::int, \
                    COUNT(c.id)::bigint AS total, \
                    COUNT(c.id) FILTER (WHERE c.status = 'ready')::bigint AS ready, \
                    COUNT(c.id) FILTER (WHERE c.status = 'embed_failed')::bigint AS pending \
             FROM source_version sv \
             LEFT JOIN chunk c ON c.source_version_id = sv.id \
             WHERE sv.source_id = $1 AND sv.is_active = true \
             GROUP BY sv.revision",
        )
        .bind(src.id)
        .fetch_optional(&state.pool)
        .await
        {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(request_id = rid, slug = %src.slug, error = %e, "summary query failed");
                return error::service_unavailable("ingest summary query failed", rid);
            }
        };
        let (active_revision, total_chunks, ready_chunks, embed_failed_chunks) = match summary {
            Some((rev, total, ready, pending)) => (Some(rev), total, ready, pending),
            None => (None, 0, 0, 0),
        };
        summaries.push(SourceStatus {
            slug: src.slug,
            active_revision,
            total_chunks,
            ready_chunks,
            embed_failed_chunks,
        });
    }
    summaries.sort_by(|a, b| a.slug.cmp(&b.slug));

    Json(IngestStatusResponse {
        active_embedding_model: model_wire,
        sources: summaries,
    })
    .into_response()
}

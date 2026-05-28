//! Admin ingest write protocol (Story 10).
//!
//! Four endpoints carry the ingest lifecycle:
//!
//! 1. `POST /v1/admin/sources/:slug/ingest-runs` — allocate a new
//!    [`source_version`] in `building` state. Returns its UUID and revision.
//! 2. `PUT  /v1/admin/sources/:slug/ingest-runs/:id/documents` — upload one
//!    batch of documents (with their chunks). May be called multiple times for
//!    one run. Carries out the FR-014 carry-forward against the prior active
//!    version when a per-path content_hash matches.
//! 3. `POST /v1/admin/sources/:slug/ingest-runs/:id/finalize` — atomically
//!    promote the building version to `active`, demoting the prior active
//!    version to `inactive` in a single transaction.
//! 4. `POST /v1/admin/sources/:slug/ingest-runs/:id/abort` — mark the run
//!    `aborted`; subsequent writes against this id return 409 `run_aborted`
//!    (FR-022).
//!
//! Every endpoint requires an admin-tier bearer (FR-058 + FR-117). New chunks
//! land in `embed_failed` state and are excluded from search until an
//! out-of-band embedder fills `chunk.embedding` and flips them to `ready`.
//! Carried chunks inherit their prior `status` and `embedding`, preserving the
//! work the embedder did on the prior version.
//!
//! [`source_version`]: mn_store::entities::source_version

use std::str::FromStr;

use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use axum::{Json, Router};
use mn_core::error::{Error as CoreError, ErrorCode};
use mn_core::model_id::EmbeddingModelId;
use mn_core::provenance::Provenance;
use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceVersionStatus};
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use mn_store::StoreError;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::error;
use crate::middleware::bearer::AuthContext;
use crate::middleware::request_id::RequestId;

/// Mount the admin ingest routes.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/sources/:slug/ingest-runs", post(start_ingest_run))
        .route("/v1/admin/sources/:slug/ingest-runs/:id/documents", put(upload_documents))
        .route("/v1/admin/sources/:slug/ingest-runs/:id/finalize", post(finalize_run))
        .route("/v1/admin/sources/:slug/ingest-runs/:id/abort", post(abort_run))
}

/// Body of `POST /v1/admin/sources/:slug/ingest-runs`.
#[derive(Debug, Deserialize)]
pub struct StartIngestRunRequest {
    /// CLI version that produced the run (FR-019 reproducibility).
    pub ingest_cli_version: String,
    /// Embedding model wire id (`name@revision`). MUST match the corpus's
    /// active model; otherwise the request 409s with `embedding_model_mismatch`.
    pub embedding_model: String,
    /// Optional notes captured at run start.
    #[serde(default)]
    pub note: Option<String>,
}

/// Body of `POST /v1/admin/sources/:slug/ingest-runs`'s response.
#[derive(Debug, Serialize)]
pub struct StartIngestRunResponse {
    /// Identifier for subsequent calls — also the `source_version.id`.
    pub ingest_run_id: Uuid,
    /// Convenience alias for `ingest_run_id`.
    pub source_version_id: Uuid,
    /// Auto-assigned monotonic revision for the new version.
    pub source_version_revision: i32,
}

/// One document to upload in a batch.
#[derive(Debug, Clone, Deserialize)]
pub struct DocumentUpload {
    /// Repo-relative path. The carry-forward join key.
    pub path: String,
    /// Document kind discriminator.
    pub kind: DocumentKind,
    /// SHA-256 over the normalized content (`document_hash`).
    pub content_hash: String,
    /// Public source URL.
    #[serde(default)]
    pub source_url: Option<String>,
    /// Public published URL.
    #[serde(default)]
    pub published_url: Option<String>,
    /// ISO language tag.
    #[serde(default)]
    pub language: Option<String>,
    /// Last-modified timestamp from the source.
    #[serde(default)]
    pub source_modified_at: Option<OffsetDateTime>,
    /// Verbatim frontmatter (YAML → JSON).
    #[serde(default)]
    pub frontmatter: Option<serde_json::Value>,
    /// Materialized provenance.
    #[serde(default)]
    pub provenance: Provenance,
    /// Character count.
    #[serde(default)]
    pub char_count: i32,
    /// Token count.
    #[serde(default)]
    pub token_count: i32,
    /// Chunks for this document.
    pub chunks: Vec<ChunkUpload>,
}

/// One chunk to upload.
#[derive(Debug, Clone, Deserialize)]
pub struct ChunkUpload {
    /// 0-indexed chunk position.
    pub chunk_index: i32,
    /// Total chunks in the parent document.
    pub total_chunks: i32,
    /// Chunk text.
    pub content: String,
    /// SHA-256 over `content`.
    pub content_hash: String,
    /// Heading path (Markdown).
    #[serde(default)]
    pub heading_path: Vec<String>,
    /// Symbol path (code, structured segments).
    #[serde(default)]
    pub symbol_path: Vec<mn_core::types::SymbolSegment>,
    /// Start byte in source.
    #[serde(default)]
    pub start_byte: i32,
    /// End byte in source.
    #[serde(default)]
    pub end_byte: i32,
    /// Token count.
    #[serde(default)]
    pub token_count: i32,
}

/// Body of `PUT .../documents`.
#[derive(Debug, Deserialize)]
pub struct UploadDocumentsRequest {
    /// Documents in this batch.
    pub documents: Vec<DocumentUpload>,
    /// Optional batch index (0-indexed position in a multi-batch upload).
    #[serde(default)]
    pub batch_index: Option<usize>,
    /// Optional total batch count for the ingest run.
    #[serde(default)]
    pub batch_count: Option<usize>,
}

/// Response from `PUT .../documents`.
#[derive(Debug, Serialize)]
pub struct UploadDocumentsResponse {
    /// Documents accepted in this batch.
    pub accepted: usize,
    /// Documents carried forward (chunks cloned from prior).
    pub carried: usize,
    /// Per-document conflicts (e.g. duplicate path).
    pub conflicts: Vec<UploadConflict>,
}

/// One per-document conflict surfaced by the upload handler.
#[derive(Debug, Serialize)]
pub struct UploadConflict {
    /// Repo-relative path of the offending document.
    pub path: String,
    /// Free-form reason.
    pub reason: String,
}

/// Response from `POST .../finalize`.
#[derive(Debug, Serialize)]
pub struct FinalizeResult {
    /// The newly-active source_version.
    pub source_version_id: Uuid,
    /// Its revision.
    pub revision: i32,
    /// Always `true` on success.
    pub is_active: bool,
    /// Revision of the previously-active version, if any.
    pub demoted_revision: Option<i32>,
}

async fn start_ingest_run(
    Path(slug): Path<String>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    Json(req): Json<StartIngestRunRequest>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    let src = match source::get_by_slug(&state.pool, &slug).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("source `{slug}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "start_ingest_run", error = %e, "store error");
            return error::service_unavailable("source lookup failed", rid);
        }
    };

    let wire: EmbeddingModelId = match EmbeddingModelId::from_str(&req.embedding_model) {
        Ok(id) => id,
        Err(e) => {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message(format!("embedding_model parse failed: {e}"))
                    .remediation("supply name@revision (e.g. bge-base-en-v1.5@1)")
                    .build(),
                rid,
            );
        }
    };

    let Ok(revision) = i32::try_from(wire.revision) else {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message("embedding_model revision overflows i32")
                .build(),
            rid,
        );
    };
    let model = match embedding_model::get_by_name_revision(&state.pool, &wire.name, revision).await
    {
        Ok(m) => m,
        Err(StoreError::NotFound) => {
            return error::into_response(
                CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                    .message(format!("embedding model `{}` is not registered", req.embedding_model))
                    .remediation("run `mnm models list` to see the corpus's active model")
                    .build(),
                rid,
            );
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "start_ingest_run", error = %e, "model lookup failed");
            return error::service_unavailable("embedding model lookup failed", rid);
        }
    };

    // SV's content_hash is filled at finalize from the aggregate of its
    // documents; on create we stamp a placeholder.
    match source_version::create_building(
        &state.pool,
        src.id,
        model.id,
        &req.ingest_cli_version,
        "pending",
    )
    .await
    {
        Ok((sv_id, sv_rev)) => Json(StartIngestRunResponse {
            ingest_run_id: sv_id,
            source_version_id: sv_id,
            source_version_revision: sv_rev,
        })
        .into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "start_ingest_run", error = %e, "create sv failed");
            error::service_unavailable("could not allocate source_version", rid)
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn upload_documents(
    Path((slug, run_id)): Path<(String, Uuid)>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
    Json(req): Json<UploadDocumentsRequest>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    if let (Some(i), Some(n)) = (req.batch_index, req.batch_count) {
        tracing::info!(
            ingest_run_id = %run_id,
            batch_index = i,
            batch_count = n,
            documents = req.documents.len(),
            "received batch"
        );
    }

    // Look up the run and confirm it's in `building`.
    let src = match source::get_by_slug(&state.pool, &slug).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("source `{slug}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "upload_documents", error = %e, "source lookup failed");
            return error::service_unavailable("source lookup failed", rid);
        }
    };

    let sv = match source_version::get_by_id(&state.pool, run_id).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("ingest run `{run_id}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "upload_documents", error = %e, "sv lookup failed");
            return error::service_unavailable("ingest run lookup failed", rid);
        }
    };
    if sv.source_id != src.id {
        return error::not_found(
            format!("ingest run `{run_id}` is not under source `{slug}`"),
            rid,
        );
    }
    match sv.status {
        SourceVersionStatus::Building => {}
        SourceVersionStatus::Aborted => {
            return error::into_response(
                CoreError::builder(ErrorCode::RunAborted)
                    .message(format!("ingest run `{run_id}` is aborted"))
                    .remediation("start a new run with POST /v1/admin/sources/:slug/ingest-runs")
                    .build(),
                rid,
            );
        }
        _ => {
            return error::into_response(
                CoreError::builder(ErrorCode::InvalidRequest)
                    .message(format!(
                        "ingest run `{run_id}` is in state `{:?}`, not `building`",
                        sv.status
                    ))
                    .build(),
                rid,
            );
        }
    }

    // Build a path → (prior_doc_id, prior_hash) map for carry-forward.
    let prior_by_path: std::collections::HashMap<String, (Uuid, String)> =
        match prior_active_documents(&state.pool, src.id).await {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(request_id = rid, op = "upload_documents", error = %e, "prior lookup failed");
                return error::service_unavailable("prior version lookup failed", rid);
            }
        };

    // Cache the root node; lazily-created.
    let mut root_node: Option<Uuid> = None;
    let mut accepted = 0_usize;
    let mut carried = 0_usize;
    let mut conflicts: Vec<UploadConflict> = Vec::new();
    let mut seen_in_batch: std::collections::HashSet<String> = std::collections::HashSet::new();

    for doc in req.documents {
        if !seen_in_batch.insert(doc.path.clone()) {
            conflicts.push(UploadConflict {
                path: doc.path.clone(),
                reason: "duplicate path in this batch".to_owned(),
            });
            continue;
        }

        // Ensure root.
        if root_node.is_none() {
            match ensure_root_node(&state.pool, sv.id).await {
                Ok(id) => root_node = Some(id),
                Err(e) => {
                    tracing::warn!(request_id = rid, op = "upload_documents", error = %e, "root node create failed");
                    return error::service_unavailable("root node create failed", rid);
                }
            }
        }
        let root = root_node.expect("root cached");

        // Decide carry vs new.
        let carry_source = prior_by_path
            .get(&doc.path)
            .filter(|(_, prior_hash)| prior_hash == &doc.content_hash)
            .map(|(prior_id, _)| *prior_id);

        let result = if let Some(prior_doc_id) = carry_source {
            carry_forward_one(&state.pool, sv.id, sv.embedding_model_id, root, prior_doc_id, &doc)
                .await
        } else {
            insert_new_document(&state.pool, sv.id, sv.embedding_model_id, root, &doc).await
        };

        match result {
            Ok(was_carried) => {
                accepted += 1;
                if was_carried {
                    carried += 1;
                }
            }
            Err(e) => {
                tracing::warn!(
                    request_id = rid,
                    op = "upload_documents",
                    path = doc.path,
                    error = %e,
                    "document insert failed",
                );
                conflicts.push(UploadConflict {
                    path: doc.path.clone(),
                    reason: format!("insert failed: {e}"),
                });
            }
        }
    }

    Json(UploadDocumentsResponse { accepted, carried, conflicts }).into_response()
}

async fn finalize_run(
    Path((slug, run_id)): Path<(String, Uuid)>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    let src = match source::get_by_slug(&state.pool, &slug).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("source `{slug}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "finalize_run", error = %e, "source lookup failed");
            return error::service_unavailable("source lookup failed", rid);
        }
    };

    let sv = match source_version::get_by_id(&state.pool, run_id).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("ingest run `{run_id}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "finalize_run", error = %e, "sv lookup failed");
            return error::service_unavailable("ingest run lookup failed", rid);
        }
    };
    if sv.source_id != src.id {
        return error::not_found(
            format!("ingest run `{run_id}` is not under source `{slug}`"),
            rid,
        );
    }
    if sv.status == SourceVersionStatus::Aborted {
        return error::into_response(
            CoreError::builder(ErrorCode::RunAborted)
                .message(format!("ingest run `{run_id}` is aborted"))
                .remediation("start a new run with POST /v1/admin/sources/:slug/ingest-runs")
                .build(),
            rid,
        );
    }

    match source_version::finalize(&state.pool, run_id).await {
        Ok((promoted, demoted)) => Json(FinalizeResult {
            source_version_id: run_id,
            revision: promoted,
            is_active: true,
            demoted_revision: demoted,
        })
        .into_response(),
        Err(StoreError::CheckViolation(msg)) => error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(msg)
                .build(),
            rid,
        ),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "finalize_run", error = %e, "finalize failed");
            error::service_unavailable("finalize failed", rid)
        }
    }
}

async fn abort_run(
    Path((slug, run_id)): Path<(String, Uuid)>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let rid = req_id.as_str();
    if let Some(resp) = admin_reject(rid, auth.as_ref()) {
        return resp;
    }

    let src = match source::get_by_slug(&state.pool, &slug).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("source `{slug}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "abort_run", error = %e, "source lookup failed");
            return error::service_unavailable("source lookup failed", rid);
        }
    };
    let sv = match source_version::get_by_id(&state.pool, run_id).await {
        Ok(s) => s,
        Err(StoreError::NotFound) => {
            return error::not_found(format!("ingest run `{run_id}` not found"), rid);
        }
        Err(e) => {
            tracing::warn!(request_id = rid, op = "abort_run", error = %e, "sv lookup failed");
            return error::service_unavailable("ingest run lookup failed", rid);
        }
    };
    if sv.source_id != src.id {
        return error::not_found(
            format!("ingest run `{run_id}` is not under source `{slug}`"),
            rid,
        );
    }

    match source_version::abort(&state.pool, run_id).await {
        Ok(()) => axum::http::StatusCode::OK.into_response(),
        Err(StoreError::NotFound) => error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!("ingest run `{run_id}` is not in `building` state"))
                .build(),
            rid,
        ),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "abort_run", error = %e, "abort failed");
            error::service_unavailable("abort failed", rid)
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
                .message("admin tier required for ingest")
                .remediation("read-uplift tokens may not write — request admin tier")
                .build(),
            rid,
        )),
    }
}

async fn ensure_root_node(pool: &sqlx::PgPool, sv_id: Uuid) -> Result<Uuid, StoreError> {
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM node WHERE source_version_id = $1 AND parent_node_id IS NULL AND kind = 'root' LIMIT 1",
    )
    .bind(sv_id)
    .fetch_optional(pool)
    .await?;
    if let Some((id,)) = existing {
        return Ok(id);
    }
    node::insert(pool, sv_id, None, NodeKind::Root, "root", 0).await
}

async fn prior_active_documents(
    pool: &sqlx::PgPool,
    source_id: Uuid,
) -> Result<std::collections::HashMap<String, (Uuid, String)>, StoreError> {
    match source_version::get_active(pool, source_id).await {
        Ok(active) => {
            let docs = document::list_for_source_version(pool, active.id).await?;
            Ok(docs
                .into_iter()
                .map(|d| (d.source_path, (d.id, d.content_hash)))
                .collect())
        }
        Err(StoreError::NotFound) => Ok(std::collections::HashMap::new()),
        Err(e) => Err(e),
    }
}

async fn insert_new_document(
    pool: &sqlx::PgPool,
    sv_id: Uuid,
    embedding_model_id: Uuid,
    root: Uuid,
    doc: &DocumentUpload,
) -> Result<bool, StoreError> {
    // Sanity-check the client's claimed hash matches what we'd compute over
    // their chunks' concatenated text — for v1 we trust the client's hash
    // (the manifest validator + CLI compute it deterministically). Future
    // hardening: rehash server-side against a canonicalised body.
    let doc_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, &doc.path, 0).await?;
    let new_doc_id = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node,
            kind: doc.kind,
            source_url: doc.source_url.as_deref(),
            published_url: doc.published_url.as_deref(),
            source_path: &doc.path,
            language: doc.language.as_deref(),
            content_hash: &doc.content_hash,
            source_modified_at: doc.source_modified_at,
            frontmatter: doc.frontmatter.clone(),
            provenance: &doc.provenance,
            package_id: None,
            char_count: doc.char_count,
            token_count: doc.token_count,
        },
    )
    .await?;
    for chunk_upload in &doc.chunks {
        let chunk_node = node::insert(
            pool,
            sv_id,
            Some(doc_node),
            NodeKind::Chunk,
            &format!("chunk-{}", chunk_upload.chunk_index),
            chunk_upload.chunk_index,
        )
        .await?;
        chunk::insert(
            pool,
            chunk::NewChunk {
                source_version_id: sv_id,
                document_id: new_doc_id,
                node_id: chunk_node,
                chunk_index: chunk_upload.chunk_index,
                total_chunks: chunk_upload.total_chunks,
                content: &chunk_upload.content,
                content_hash: &chunk_upload.content_hash,
                embedding: None,
                embedding_model_id,
                heading_path: &chunk_upload.heading_path,
                symbol_path: &chunk_upload.symbol_path,
                start_byte: chunk_upload.start_byte,
                end_byte: chunk_upload.end_byte,
                token_count: chunk_upload.token_count,
                status: ChunkStatus::EmbedFailed,
            },
        )
        .await?;
    }
    Ok(false)
}

async fn carry_forward_one(
    pool: &sqlx::PgPool,
    sv_id: Uuid,
    embedding_model_id: Uuid,
    root: Uuid,
    prior_doc_id: Uuid,
    doc: &DocumentUpload,
) -> Result<bool, StoreError> {
    let prior_chunks = chunk::list_for_carry_forward(pool, prior_doc_id).await?;

    let doc_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, &doc.path, 0).await?;
    let new_doc_id = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node,
            kind: doc.kind,
            source_url: doc.source_url.as_deref(),
            published_url: doc.published_url.as_deref(),
            source_path: &doc.path,
            language: doc.language.as_deref(),
            content_hash: &doc.content_hash,
            source_modified_at: doc.source_modified_at,
            frontmatter: doc.frontmatter.clone(),
            provenance: &doc.provenance,
            package_id: None,
            char_count: doc.char_count,
            token_count: doc.token_count,
        },
    )
    .await?;

    for prior in prior_chunks {
        let chunk_node = node::insert(
            pool,
            sv_id,
            Some(doc_node),
            NodeKind::Chunk,
            &format!("chunk-{}", prior.chunk_index),
            prior.chunk_index,
        )
        .await?;
        chunk::insert(
            pool,
            chunk::NewChunk {
                source_version_id: sv_id,
                document_id: new_doc_id,
                node_id: chunk_node,
                chunk_index: prior.chunk_index,
                total_chunks: prior.total_chunks,
                content: &prior.content,
                content_hash: &prior.content_hash,
                embedding: prior.embedding.clone(),
                embedding_model_id,
                heading_path: &prior.heading_path,
                symbol_path: &prior.symbol_path,
                start_byte: prior.start_byte,
                end_byte: prior.end_byte,
                token_count: prior.token_count,
                status: prior.status,
            },
        )
        .await?;
    }
    Ok(true)
}

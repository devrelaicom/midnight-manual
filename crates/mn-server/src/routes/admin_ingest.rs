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
//! Every endpoint requires an admin-tier bearer (FR-058 + FR-117). The CLI
//! embeds chunks with VoyageAI before upload and sends ready vectors, so new
//! chunks normally arrive `ready`. A chunk uploaded without an embedding lands
//! in `embed_failed` and is excluded from search (there is no server-side
//! embedder worker to backfill it; re-ingest the source to fix it). Carried
//! chunks inherit their prior `status` and `embedding`, preserving the prior
//! version's vectors.
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
use mn_store::entities::{chunk, document, embedding_model, node, package, source, source_version};
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
    /// Detected package membership (rust/npm) for this document, if any.
    #[serde(default)]
    pub package: Option<mn_core::types::PackageRef>,
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
    /// Precomputed embedding vector (the CLI embeds via VoyageAI before
    /// upload). `None` means the chunk could not be embedded and lands in
    /// `embed_failed`; it stays excluded from search until the source is
    /// re-ingested (there is no server-side embedder worker to backfill it).
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
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
    /// Wire id (`name@revision`) of the model that produced any supplied
    /// `ChunkUpload.embedding` vectors. Required when embeddings are present;
    /// must match the run's model. `None` only for text-only batches (chunks
    /// with no embedding, which then land in `embed_failed`).
    #[serde(default)]
    pub embedding_model: Option<String>,
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

    // If any chunk in this batch carries a precomputed embedding, validate the
    // declared model + dimension contract via `check_embedded_batch` (pure +
    // unit-tested). Only the run-model lookup needs the DB; we gate on
    // `has_embeddings` so text-only batches skip the lookup entirely.
    //
    // The expected dimension is the corpus model's dim (resolved into AppState,
    // re-resolved after each ingest finalize) — NOT a hardcoded literal. Voyage
    // vectors are 1024-dim; a hardcoded 768 would 400 every valid upload. If the
    // corpus model is unresolved we 503, matching how /v1/search behaves with no
    // resolved model rather than guessing a dimension.
    let has_embeddings = req
        .documents
        .iter()
        .any(|d| d.chunks.iter().any(|c| c.embedding.is_some()));
    if has_embeddings {
        let expected_dim = {
            let snapshot = state
                .corpus_model
                .read()
                .expect("corpus_model lock poisoned")
                .clone();
            match snapshot {
                Some(cm) => cm.dim,
                None => {
                    return error::service_unavailable(
                        "server has no resolved corpus_model; cannot validate embedding dimension",
                        rid,
                    );
                }
            }
        };
        let run_model = match embedding_model::get_by_id(&state.pool, sv.embedding_model_id).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(request_id = rid, op = "upload_documents", error = %e, "run model lookup failed");
                return error::service_unavailable("run model lookup failed", rid);
            }
        };
        let expected = format!("{}@{}", run_model.name, run_model.revision);
        if let Err(e) = check_embedded_batch(
            &req.documents,
            req.embedding_model.as_deref(),
            &expected,
            expected_dim,
        ) {
            return error::into_response(e, rid);
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
        Ok((promoted, demoted)) => {
            // A finalize may promote a source_version onto a different embedding
            // model; re-resolve the corpus model so search reflects it without a
            // restart (Task 3.4).
            crate::corpus_model::refresh(&state.pool, &state.corpus_model).await;
            Json(FinalizeResult {
                source_version_id: run_id,
                revision: promoted,
                is_active: true,
                demoted_revision: demoted,
            })
            .into_response()
        }
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

/// Validate a batch that carries precomputed embeddings: it must declare the
/// model it used (matching the run's `expected` `name@revision`), and every
/// supplied vector must match `expected_dim` — the corpus model's dimension,
/// threaded in by the caller from the resolved [`crate::corpus_model::CorpusModel`]
/// (1024 for Voyage's `voyage-code-3`). Text-only batches (no embeddings) pass.
///
/// Pure and DB-free so every error path — including the dimension rejection,
/// whose error MUST carry a `.remediation()` or [`CoreError::build`] panics —
/// is unit-testable without Postgres.
///
/// # Errors
///
/// Returns a built [`CoreError`] (`EmbeddingModelMismatch` or `InvalidRequest`)
/// describing the first violation found.
fn check_embedded_batch(
    documents: &[DocumentUpload],
    declared_model: Option<&str>,
    expected: &str,
    expected_dim: usize,
) -> Result<(), CoreError> {
    let has_embeddings = documents
        .iter()
        .any(|d| d.chunks.iter().any(|c| c.embedding.is_some()));
    if !has_embeddings {
        return Ok(());
    }
    let Some(provided) = declared_model else {
        return Err(CoreError::builder(ErrorCode::EmbeddingModelMismatch)
            .message("upload supplies embeddings but no embedding_model")
            .remediation("send the wire id (name@revision) the CLI embedded with")
            .build());
    };
    let provided_norm = EmbeddingModelId::from_str(provided)
        .map(|m| format!("{}@{}", m.name, m.revision))
        .map_err(|e| {
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!("embedding_model parse failed: {e}"))
                .remediation("supply name@revision (e.g. bge-base-en-v1.5@1)")
                .build()
        })?;
    if provided_norm != expected {
        return Err(CoreError::builder(ErrorCode::EmbeddingModelMismatch)
            .message(format!(
                "upload embeddings declare model `{provided_norm}` but run uses `{expected}`"
            ))
            .remediation("re-run ingest with --embedding-model matching the corpus")
            .build());
    }
    for d in documents {
        for c in &d.chunks {
            if let Some(v) = &c.embedding {
                if v.len() != expected_dim {
                    return Err(CoreError::builder(ErrorCode::InvalidRequest)
                        .message(format!(
                            "chunk {}#{} embedding dim {} != {expected_dim}",
                            d.path,
                            c.chunk_index,
                            v.len(),
                        ))
                        .remediation(format!(
                            "re-embed with the corpus model `{expected}` ({expected_dim}-dim)"
                        ))
                        .build());
                }
            }
        }
    }
    Ok(())
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
    let package_id = if let Some(pkg) = &doc.package {
        let kind = match pkg.kind.as_str() {
            "rust" => mn_core::types::PackageKind::Rust,
            "npm" => mn_core::types::PackageKind::Npm,
            "compact" => mn_core::types::PackageKind::Compact,
            _ => mn_core::types::PackageKind::Other,
        };
        Some(
            package::upsert(pool, sv_id, kind, &pkg.name, None, pkg.manifest_path.as_deref())
                .await?,
        )
    } else {
        None
    };
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
            package_id,
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
        let (embedding, status) = chunk_upload
            .embedding
            .as_ref()
            .map_or((None, ChunkStatus::EmbedFailed), |v| (Some(v.clone()), ChunkStatus::Ready));
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
                embedding,
                embedding_model_id,
                heading_path: &chunk_upload.heading_path,
                symbol_path: &chunk_upload.symbol_path,
                start_byte: chunk_upload.start_byte,
                end_byte: chunk_upload.end_byte,
                token_count: chunk_upload.token_count,
                status,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_request_deserializes_embedding_and_model() {
        let body = serde_json::json!({
            "embedding_model": "bge-base-en-v1.5@1",
            "documents": [{
                "path": "a.md",
                "kind": "markdown",
                "content_hash": "h",
                "provenance": {},
                "chunks": [{
                    "chunk_index": 0,
                    "total_chunks": 1,
                    "content": "hello",
                    "content_hash": "c",
                    "embedding": [0.1_f32, 0.2, 0.3]
                }]
            }]
        });
        let req: UploadDocumentsRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.embedding_model.as_deref(), Some("bge-base-en-v1.5@1"));
        assert_eq!(req.documents[0].chunks[0].embedding.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn upload_request_defaults_embedding_fields_to_none() {
        let body = serde_json::json!({
            "documents": [{
                "path": "a.md", "kind": "markdown", "content_hash": "h", "provenance": {},
                "chunks": [{ "chunk_index": 0, "total_chunks": 1, "content": "x", "content_hash": "c" }]
            }]
        });
        let req: UploadDocumentsRequest = serde_json::from_value(body).unwrap();
        assert!(req.embedding_model.is_none());
        assert!(req.documents[0].chunks[0].embedding.is_none());
    }

    const EXPECTED_MODEL: &str = "voyage-code-3@1";
    /// Corpus dimension threaded in from the resolved `CorpusModel` — Voyage's
    /// `voyage-code-3` is 1024-dim (the hardcoded 768 this fix removed would
    /// have 400'd every valid upload).
    const EXPECTED_DIM: usize = 1024;

    fn chunk_with_dim(idx: i32, dim: Option<usize>) -> ChunkUpload {
        ChunkUpload {
            chunk_index: idx,
            total_chunks: 1,
            content: "x".to_owned(),
            content_hash: "c".to_owned(),
            heading_path: vec![],
            symbol_path: vec![],
            start_byte: 0,
            end_byte: 0,
            token_count: 0,
            embedding: dim.map(|d| vec![0.0_f32; d]),
        }
    }

    fn doc_with(chunks: Vec<ChunkUpload>) -> DocumentUpload {
        DocumentUpload {
            path: "a.md".to_owned(),
            kind: DocumentKind::Markdown,
            content_hash: "h".to_owned(),
            source_url: None,
            published_url: None,
            language: None,
            source_modified_at: None,
            frontmatter: None,
            provenance: Provenance::default(),
            char_count: 0,
            token_count: 0,
            chunks,
            package: None,
        }
    }

    #[test]
    fn check_text_only_batch_is_ok() {
        let docs = vec![doc_with(vec![chunk_with_dim(0, None)])];
        assert!(check_embedded_batch(&docs, None, EXPECTED_MODEL, EXPECTED_DIM).is_ok());
    }

    #[test]
    fn check_valid_embedded_batch_is_ok() {
        // A correctly-sized Voyage (1024-dim) vector passes.
        let docs = vec![doc_with(vec![chunk_with_dim(0, Some(EXPECTED_DIM))])];
        assert!(
            check_embedded_batch(&docs, Some(EXPECTED_MODEL), EXPECTED_MODEL, EXPECTED_DIM).is_ok()
        );
    }

    #[test]
    fn check_embeddings_without_model_is_mismatch() {
        let docs = vec![doc_with(vec![chunk_with_dim(0, Some(EXPECTED_DIM))])];
        let err = check_embedded_batch(&docs, None, EXPECTED_MODEL, EXPECTED_DIM).unwrap_err();
        assert_eq!(err.code, ErrorCode::EmbeddingModelMismatch);
    }

    #[test]
    fn check_wrong_model_is_mismatch() {
        let docs = vec![doc_with(vec![chunk_with_dim(0, Some(EXPECTED_DIM))])];
        let err =
            check_embedded_batch(&docs, Some("voyage-code-3@2"), EXPECTED_MODEL, EXPECTED_DIM)
                .unwrap_err();
        assert_eq!(err.code, ErrorCode::EmbeddingModelMismatch);
    }

    #[test]
    fn check_wrong_dim_is_invalid_request_and_does_not_panic() {
        // Regression guard: this error path previously omitted `.remediation()`,
        // which panics in `CoreError::build()`. `unwrap_err()` would re-panic if
        // that regressed — it must return a well-formed error instead.
        let docs = vec![doc_with(vec![chunk_with_dim(0, Some(3))])];
        let err = check_embedded_batch(&docs, Some(EXPECTED_MODEL), EXPECTED_MODEL, EXPECTED_DIM)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidRequest);
        assert!(!err.remediation.is_empty());
    }

    #[test]
    fn check_768_dim_is_rejected_when_corpus_is_1024() {
        // Direct regression guard for the hardcoded-768 bug: a 768-dim vector
        // must now be REJECTED against a 1024-dim corpus model (previously the
        // hardcoded check accepted 768 and rejected the real 1024 Voyage dim).
        let docs = vec![doc_with(vec![chunk_with_dim(0, Some(768))])];
        let err = check_embedded_batch(&docs, Some(EXPECTED_MODEL), EXPECTED_MODEL, EXPECTED_DIM)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidRequest);
    }
}

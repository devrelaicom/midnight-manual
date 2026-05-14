//! `source_version` entity queries — including the atomic finalize that flips
//! the active version in one transaction (FR-061, EC-04).

use mn_core::types::{SourceVersion, SourceVersionStatus};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::Result;

/// Create a new source_version in the `building` state. Returns the new id and
/// the auto-assigned monotonic revision.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::ForeignKeyViolation`] if `source_id`
/// or `embedding_model_id` are unknown, or
/// [`crate::error::StoreError::UniqueViolation`] on a revision collision (rare —
/// only if two ingests race past the SELECT-then-INSERT window).
pub async fn create_building(
    pool: &PgPool,
    source_id: Uuid,
    embedding_model_id: Uuid,
    ingest_cli_version: &str,
    content_hash: &str,
) -> Result<(Uuid, i32)> {
    // Auto-assign revision = max(existing) + 1 in a single statement.
    let row: (Uuid, i32) = sqlx::query_as(
        "INSERT INTO source_version (source_id, revision, status, embedding_model_id, \
                                     ingest_cli_version, content_hash) \
         SELECT $1, COALESCE(MAX(revision), 0) + 1, 'building', $2, $3, $4 \
         FROM source_version WHERE source_id = $1 \
         RETURNING id, revision",
    )
    .bind(source_id)
    .bind(embedding_model_id)
    .bind(ingest_cli_version)
    .bind(content_hash)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Atomically finalize a building source_version: flip it active, demote the
/// previously-active version (if any) to inactive.
///
/// Implemented as a single transaction so the partial-unique active-version
/// index (FR-003 / EC-04) never sees two active rows for the same source.
///
/// Returns `(promoted_revision, Some(demoted_revision))` on success;
/// `demoted_revision` is `None` for the first-ever ingest of a source.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if the row id is unknown,
/// [`crate::error::StoreError::CheckViolation`] if the row is not in `building`
/// state, or [`crate::error::StoreError::Database`] for any tx failure.
pub async fn finalize(pool: &PgPool, source_version_id: Uuid) -> Result<(i32, Option<i32>)> {
    let mut tx = pool.begin().await?;

    // Confirm the version is in the building state and capture its source_id.
    let row: (Uuid, i32, String) = sqlx::query_as(
        "SELECT source_id, revision, status FROM source_version WHERE id = $1 FOR UPDATE",
    )
    .bind(source_version_id)
    .fetch_one(&mut *tx)
    .await?;
    let (source_id, promoted_revision, status) = row;
    if status != "building" {
        return Err(crate::error::StoreError::CheckViolation(format!(
            "source_version {source_version_id} is not in building state (current: {status})"
        )));
    }

    // Demote any currently-active version for the same source.
    let demoted: Option<(i32,)> = sqlx::query_as(
        "UPDATE source_version SET is_active = false, status = 'inactive' \
         WHERE source_id = $1 AND is_active = true AND id <> $2 \
         RETURNING revision",
    )
    .bind(source_id)
    .bind(source_version_id)
    .fetch_optional(&mut *tx)
    .await?;

    // Promote the target version.
    sqlx::query(
        "UPDATE source_version SET is_active = true, status = 'active', ingested_at = now() \
         WHERE id = $1",
    )
    .bind(source_version_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok((promoted_revision, demoted.map(|r| r.0)))
}

/// Abort an in-progress ingest: mark the source_version as `aborted` and
/// release the `building` slot. Subsequent attempts to upload to this run id
/// return `RunAborted` per FR-022.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if the row id is unknown.
pub async fn abort(pool: &PgPool, source_version_id: Uuid) -> Result<()> {
    let r = sqlx::query(
        "UPDATE source_version SET status = 'aborted' WHERE id = $1 AND status = 'building'",
    )
    .bind(source_version_id)
    .execute(pool)
    .await?;
    if r.rows_affected() == 0 {
        return Err(crate::error::StoreError::NotFound);
    }
    Ok(())
}

/// Mark a source_version retired (eligible for sweep).
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if the row id is unknown.
pub async fn retire(pool: &PgPool, source_version_id: Uuid) -> Result<()> {
    let r = sqlx::query(
        "UPDATE source_version SET status = 'retired', is_active = false, retired_at = now() \
         WHERE id = $1",
    )
    .bind(source_version_id)
    .execute(pool)
    .await?;
    if r.rows_affected() == 0 {
        return Err(crate::error::StoreError::NotFound);
    }
    Ok(())
}

/// Fetch the currently-active source_version for a source, if any.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] when no version is active
/// (e.g. immediately after retiring a source or before its first ingest).
pub async fn get_active(pool: &PgPool, source_id: Uuid) -> Result<SourceVersion> {
    let row = sqlx::query_as::<_, SourceVersionRow>(
        "SELECT id, source_id, revision, status, is_active, ingested_at, ingest_cli_version, \
                embedding_model_id, content_hash, notes, retired_at \
         FROM source_version WHERE source_id = $1 AND is_active = true",
    )
    .bind(source_id)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

/// Fetch a source_version by id.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if id is unknown.
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> Result<SourceVersion> {
    let row = sqlx::query_as::<_, SourceVersionRow>(
        "SELECT id, source_id, revision, status, is_active, ingested_at, ingest_cli_version, \
                embedding_model_id, content_hash, notes, retired_at \
         FROM source_version WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

/// Fetch a source_version by its monotonic revision.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if no matching row exists.
pub async fn get_by_revision(
    pool: &PgPool,
    source_id: Uuid,
    revision: i32,
) -> Result<SourceVersion> {
    let row = sqlx::query_as::<_, SourceVersionRow>(
        "SELECT id, source_id, revision, status, is_active, ingested_at, ingest_cli_version, \
                embedding_model_id, content_hash, notes, retired_at \
         FROM source_version WHERE source_id = $1 AND revision = $2",
    )
    .bind(source_id)
    .bind(revision)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

#[derive(sqlx::FromRow)]
struct SourceVersionRow {
    id: Uuid,
    source_id: Uuid,
    revision: i32,
    status: String,
    is_active: bool,
    ingested_at: OffsetDateTime,
    ingest_cli_version: String,
    embedding_model_id: Uuid,
    content_hash: String,
    notes: Option<String>,
    retired_at: Option<OffsetDateTime>,
}

impl TryFrom<SourceVersionRow> for SourceVersion {
    type Error = crate::error::StoreError;

    fn try_from(r: SourceVersionRow) -> std::result::Result<Self, Self::Error> {
        let status: SourceVersionStatus =
            serde_json::from_value(serde_json::Value::String(r.status))
                .map_err(|e| crate::error::StoreError::Json(e.to_string()))?;
        Ok(Self {
            id: r.id,
            source_id: r.source_id,
            revision: r.revision,
            status,
            is_active: r.is_active,
            ingested_at: r.ingested_at,
            ingest_cli_version: r.ingest_cli_version,
            embedding_model_id: r.embedding_model_id,
            content_hash: r.content_hash,
            notes: r.notes,
            retired_at: r.retired_at,
        })
    }
}

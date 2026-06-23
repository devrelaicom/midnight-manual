//! `source_version` entity queries — including the atomic finalize that flips
//! the active version in one transaction (FR-061, EC-04).

use mnm_core::types::{SourceVersion, SourceVersionStatus};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::Result;

/// Create a new source_version in the `building` state. Returns the new id and
/// the auto-assigned monotonic revision.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::ForeignKeyViolation`] if `source_id`,
/// `embedding_model_id`, or `code_embedding_model_id` are unknown, or
/// [`crate::error::StoreError::UniqueViolation`] on a revision collision (rare —
/// only if two ingests race past the SELECT-then-INSERT window).
pub async fn create_building(
    pool: &PgPool,
    source_id: Uuid,
    embedding_model_id: Uuid,
    code_embedding_model_id: Option<Uuid>,
    ingest_cli_version: &str,
    content_hash: &str,
) -> Result<(Uuid, i32)> {
    // Auto-assign revision = max(existing) + 1 in a single statement.
    let row: (Uuid, i32) = sqlx::query_as(
        "INSERT INTO source_version (source_id, revision, status, embedding_model_id, \
                                     code_embedding_model_id, ingest_cli_version, content_hash) \
         SELECT $1, COALESCE(MAX(revision), 0) + 1, 'building', $2, $3, $4, $5 \
         FROM source_version WHERE source_id = $1 \
         RETURNING id, revision",
    )
    .bind(source_id)
    .bind(embedding_model_id)
    .bind(code_embedding_model_id)
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
                embedding_model_id, code_embedding_model_id, content_hash, notes, retired_at \
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
                embedding_model_id, code_embedding_model_id, content_hash, notes, retired_at \
         FROM source_version WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

/// List every source_version for `source_id`, ordered by `revision DESC`
/// (newest first). Excludes nothing — operators inspecting history want to
/// see `aborted` and `retired` rows too.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn list_for_source(pool: &PgPool, source_id: Uuid) -> Result<Vec<SourceVersion>> {
    let rows = sqlx::query_as::<_, SourceVersionRow>(
        "SELECT id, source_id, revision, status, is_active, ingested_at, ingest_cli_version, \
                embedding_model_id, code_embedding_model_id, content_hash, notes, retired_at \
         FROM source_version WHERE source_id = $1 ORDER BY revision DESC",
    )
    .bind(source_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// Promote a previously-active (now `inactive`) source_version back to
/// active, demoting the currently-active version. Used for rollback
/// (FR-072, US8 acceptance #8).
///
/// The target version must be in `inactive` state; `building`, `aborted`,
/// `retired`, or the already-active version are rejected with
/// [`crate::error::StoreError::CheckViolation`]. Returns
/// `(promoted_revision, Some(demoted_revision))` on success;
/// `demoted_revision` is `None` only in the corner case where no other
/// version is currently active.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if no version with that
/// `(source_id, revision)` pair exists; [`crate::error::StoreError::CheckViolation`]
/// if the target is not in `inactive` state.
pub async fn promote_by_revision(
    pool: &PgPool,
    source_id: Uuid,
    revision: i32,
) -> Result<(i32, Option<i32>)> {
    let mut tx = pool.begin().await?;

    // Resolve + lock the target row.
    let row: (Uuid, String) = sqlx::query_as(
        "SELECT id, status FROM source_version \
         WHERE source_id = $1 AND revision = $2 FOR UPDATE",
    )
    .bind(source_id)
    .bind(revision)
    .fetch_one(&mut *tx)
    .await?;
    let (target_id, status) = row;
    if status != "inactive" {
        return Err(crate::error::StoreError::CheckViolation(format!(
            "source_version revision {revision} is in `{status}` — \
             only `inactive` versions can be promoted (active is already current)"
        )));
    }

    // Demote any currently-active version for the same source.
    let demoted: Option<(i32,)> = sqlx::query_as(
        "UPDATE source_version SET is_active = false, status = 'inactive' \
         WHERE source_id = $1 AND is_active = true AND id <> $2 \
         RETURNING revision",
    )
    .bind(source_id)
    .bind(target_id)
    .fetch_optional(&mut *tx)
    .await?;

    // Promote the target. We do NOT update `ingested_at` — the row's
    // identity is the historical content snapshot; only its current role
    // changes.
    sqlx::query(
        "UPDATE source_version SET is_active = true, status = 'active', retired_at = NULL \
         WHERE id = $1",
    )
    .bind(target_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok((revision, demoted.map(|r| r.0)))
}

/// Hard-delete aged-out historical source_version rows (FR-063).
///
/// Targets `inactive` and `retired` rows that fall outside their source's
/// `retention_count` window AND whose `ingested_at` is older than
/// `grace_seconds`. The active version of each source is never swept;
/// `building` and `aborted` versions are also left alone (the former is
/// in-progress, the latter is handled by the aborted-ingest sweep).
///
/// Cascades through `node` / `package` / `document` / `chunk` via the
/// existing `ON DELETE CASCADE` foreign keys.
///
/// Returns the deleted `(source_id, revision)` pairs sorted by source_id
/// then revision for stable logging.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn sweep_aged_inactive(pool: &PgPool, grace_seconds: i64) -> Result<Vec<(Uuid, i32)>> {
    let grace = grace_seconds.max(0);
    let rows: Vec<(Uuid, i32)> = sqlx::query_as(
        "WITH ranked AS ( \
             SELECT sv.id, sv.source_id, sv.revision, sv.ingested_at, sv.status, \
                    ROW_NUMBER() OVER ( \
                        PARTITION BY sv.source_id ORDER BY sv.revision DESC \
                    ) AS rn \
             FROM source_version sv \
             WHERE sv.status IN ('active','inactive','retired') \
         ), \
         eligible AS ( \
             SELECT r.id, r.source_id, r.revision \
             FROM ranked r \
             JOIN source s ON s.id = r.source_id \
             WHERE r.rn > s.retention_count \
               AND r.status IN ('inactive','retired') \
               AND r.ingested_at < now() - ($1::bigint * interval '1 second') \
         ) \
         DELETE FROM source_version \
         WHERE id IN (SELECT id FROM eligible) \
         RETURNING source_id, revision",
    )
    .bind(grace)
    .fetch_all(pool)
    .await?;
    let mut pairs: Vec<(Uuid, i32)> = rows;
    pairs.sort();
    Ok(pairs)
}

/// Hard-delete aborted ingest runs older than `grace_seconds` (FR-063).
///
/// Targets `source_version` rows in `aborted` state whose `ingested_at`
/// is past the grace window. Cascades through any nodes / documents /
/// chunks the aborted run managed to upload before being aborted.
/// Returns the deleted `(source_id, revision)` pairs sorted ascending.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn sweep_aborted(pool: &PgPool, grace_seconds: i64) -> Result<Vec<(Uuid, i32)>> {
    let grace = grace_seconds.max(0);
    let rows: Vec<(Uuid, i32)> = sqlx::query_as(
        "DELETE FROM source_version \
         WHERE status = 'aborted' \
           AND ingested_at < now() - ($1::bigint * interval '1 second') \
         RETURNING source_id, revision",
    )
    .bind(grace)
    .fetch_all(pool)
    .await?;
    let mut pairs: Vec<(Uuid, i32)> = rows;
    pairs.sort();
    Ok(pairs)
}

/// Count the documents currently persisted under a source_version (used by the
/// finalize completeness guard).
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn count_documents(pool: &PgPool, source_version_id: Uuid) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM document WHERE source_version_id = $1")
        .bind(source_version_id)
        .fetch_one(pool)
        .await?;
    Ok(n)
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
                embedding_model_id, code_embedding_model_id, content_hash, notes, retired_at \
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
    code_embedding_model_id: Option<Uuid>,
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
            code_embedding_model_id: r.code_embedding_model_id,
            content_hash: r.content_hash,
            notes: r.notes,
            retired_at: r.retired_at,
        })
    }
}

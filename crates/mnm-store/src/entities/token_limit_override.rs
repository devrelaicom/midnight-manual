//! Per-subject (CIDR or user) embedding token-limit overrides.
//!
//! The `subject` column is plain `text` in Postgres (not the `cidr` type), so
//! reads need no `::text` cast. On insert, when `subject_kind == "cidr"`, the
//! subject is normalised via `network($2::inet)::text` to canonicalise the CIDR
//! block (host bits masked off). For `subject_kind == "user"` the value is
//! bound directly.

use mnm_core::types::TokenLimitOverride;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{Result, StoreError};

/// `SELECT` column list shared by every read query.
///
/// `subject` is plain `text`, so no `::text` cast is required here.
const COLS: &str =
    "id, subject_kind, subject, hourly, daily, expires_at, note, created_by, created_at";

/// Insert a new token-limit override, returning the freshly-stored row.
///
/// When `subject_kind == "cidr"`, the `subject` is normalised to its network
/// address (host bits dropped) via `network($2::inet)::text`. Callers should
/// validate that CIDR strings are well-formed before calling so that
/// malformed input surfaces as a clean caller error rather than a database error.
///
/// # Errors
///
/// Returns [`StoreError::CheckViolation`] if `hourly` or `daily` is negative,
/// or [`StoreError::Database`] if a `"cidr"` subject is not a parseable network
/// address.
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &PgPool,
    subject_kind: &str,
    subject: &str,
    hourly: i64,
    daily: i64,
    expires_at: OffsetDateTime,
    note: Option<&str>,
    created_by: &str,
) -> Result<TokenLimitOverride> {
    let normalised = if subject_kind == "cidr" {
        "network($2::inet)::text"
    } else {
        "$2"
    };
    let sql = format!(
        "INSERT INTO token_limit_override \
             (subject_kind, subject, hourly, daily, expires_at, note, created_by) \
         VALUES ($1, {normalised}, $3, $4, $5, $6, $7) \
         RETURNING {COLS}"
    );
    let row: Row = sqlx::query_as(&sql)
        .bind(subject_kind)
        .bind(subject)
        .bind(hourly)
        .bind(daily)
        .bind(expires_at)
        .bind(note)
        .bind(created_by)
        .fetch_one(pool)
        .await?;
    Ok(row.into())
}

/// List overrides that are still in effect (`expires_at > now()`), newest first.
///
/// # Errors
///
/// Returns [`StoreError::Database`] on driver failure.
pub async fn list_active(pool: &PgPool) -> Result<Vec<TokenLimitOverride>> {
    let rows = sqlx::query_as::<_, Row>(&format!(
        "SELECT {COLS} FROM token_limit_override \
         WHERE expires_at > now() ORDER BY created_at DESC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Fetch one override by id.
///
/// # Errors
///
/// Returns [`StoreError::NotFound`] if `id` does not exist.
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> Result<TokenLimitOverride> {
    let row =
        sqlx::query_as::<_, Row>(&format!("SELECT {COLS} FROM token_limit_override WHERE id = $1"))
            .bind(id)
            .fetch_one(pool)
            .await?;
    Ok(row.into())
}

/// Sparse patch applied by [`update`] — `Some(value)` updates the column,
/// `None` leaves it untouched.
#[derive(Debug, Default, Clone)]
pub struct Patch {
    /// New expiry, when set.
    pub expires_at: Option<OffsetDateTime>,
    /// New per-hour token ceiling, when set.
    pub hourly: Option<i64>,
    /// New per-day token ceiling, when set.
    pub daily: Option<i64>,
    /// New operator note, when set.
    pub note: Option<String>,
}

/// Apply a sparse patch to one override by id, returning the updated row.
///
/// Uses `COALESCE` per column so a single `UPDATE … RETURNING` covers every
/// patch shape — no dynamic SQL.
///
/// # Errors
///
/// Returns [`StoreError::NotFound`] if `id` is unknown, or
/// [`StoreError::CheckViolation`] if the patched `hourly`/`daily` is negative.
pub async fn update(pool: &PgPool, id: Uuid, patch: Patch) -> Result<TokenLimitOverride> {
    let row = sqlx::query_as::<_, Row>(
        "UPDATE token_limit_override SET \
            expires_at = COALESCE($2, expires_at), \
            hourly = COALESCE($3, hourly), \
            daily = COALESCE($4, daily), \
            note = COALESCE($5, note) \
         WHERE id = $1 \
         RETURNING id, subject_kind, subject, hourly, daily, expires_at, note, created_by, created_at",
    )
    .bind(id)
    .bind(patch.expires_at)
    .bind(patch.hourly)
    .bind(patch.daily)
    .bind(patch.note)
    .fetch_optional(pool)
    .await?;
    row.map_or(Err(StoreError::NotFound), |r| Ok(r.into()))
}

/// Hard-delete one override by id, returning the row that was removed.
///
/// These are operational rows, not user content, so there is no soft-delete.
///
/// # Errors
///
/// Returns [`StoreError::NotFound`] if `id` does not exist.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<TokenLimitOverride> {
    let row = sqlx::query_as::<_, Row>(
        "DELETE FROM token_limit_override WHERE id = $1 \
         RETURNING id, subject_kind, subject, hourly, daily, expires_at, note, created_by, created_at",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map_or(Err(StoreError::NotFound), |r| Ok(r.into()))
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    subject_kind: String,
    subject: String,
    hourly: i64,
    daily: i64,
    expires_at: OffsetDateTime,
    note: Option<String>,
    created_by: String,
    created_at: OffsetDateTime,
}

impl From<Row> for TokenLimitOverride {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            subject_kind: r.subject_kind,
            subject: r.subject,
            hourly: r.hourly,
            daily: r.daily,
            expires_at: r.expires_at,
            note: r.note,
            created_by: r.created_by,
            created_at: r.created_at,
        }
    }
}

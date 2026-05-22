//! `rate_limit_override` entity queries.
//!
//! The `cidr` column uses the Postgres `cidr` type, which the workspace's sqlx
//! build cannot decode natively (the `ipnetwork` feature is intentionally
//! off). Every query therefore casts `cidr::text` on the way out and binds the
//! input as text on the way in. Inserts wrap the bind in `network($1::inet)`
//! rather than `$1::cidr`: the `cidr` type rejects any address with host bits
//! set to the right of the prefix (e.g. `169.155.237.15/25`), whereas
//! `network(inet)` masks those bits off and yields the canonical block
//! (`169.155.237.0/25`) — the correct semantics for a per-block override.

use mn_core::types::RateLimitOverride;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{Result, StoreError};

/// `SELECT` column list shared by every read, with `cidr` rendered as text.
const COLS: &str = "id, cidr::text AS cidr, limit_rps, expires_at, note, created_by, created_at";

/// Insert a new override, returning the freshly-stored row.
///
/// `cidr` is masked to its network address (host bits dropped). Callers should
/// validate the string is a well-formed `addr/prefix` first so malformed input
/// becomes a clean caller error rather than a database error.
///
/// # Errors
///
/// Returns [`StoreError::CheckViolation`] if `limit_rps <= 0`, or
/// [`StoreError::Database`] if `cidr` is not a parseable network address.
pub async fn insert(
    pool: &PgPool,
    cidr: &str,
    limit_rps: i32,
    expires_at: OffsetDateTime,
    note: Option<&str>,
    created_by: &str,
) -> Result<RateLimitOverride> {
    let row = sqlx::query_as::<_, OverrideRow>(
        "INSERT INTO rate_limit_override (cidr, limit_rps, expires_at, note, created_by) \
         VALUES (network($1::inet), $2, $3, $4, $5) \
         RETURNING id, cidr::text AS cidr, limit_rps, expires_at, note, created_by, created_at",
    )
    .bind(cidr)
    .bind(limit_rps)
    .bind(expires_at)
    .bind(note)
    .bind(created_by)
    .fetch_one(pool)
    .await?;
    Ok(row.into())
}

/// List overrides that are still in effect (`expires_at > now()`), newest
/// first.
///
/// # Errors
///
/// Returns [`StoreError::Database`] on driver failure.
pub async fn list_active(pool: &PgPool) -> Result<Vec<RateLimitOverride>> {
    let rows = sqlx::query_as::<_, OverrideRow>(&format!(
        "SELECT {COLS} FROM rate_limit_override \
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
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> Result<RateLimitOverride> {
    let row = sqlx::query_as::<_, OverrideRow>(&format!(
        "SELECT {COLS} FROM rate_limit_override WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(row.into())
}

/// Sparse patch applied by [`update`] — `Some(value)` updates the column,
/// `None` leaves it untouched.
#[derive(Debug, Default, Clone)]
pub struct RateLimitPatch {
    /// New expiry, when set.
    pub expires_at: Option<OffsetDateTime>,
    /// New requests-per-second ceiling, when set.
    pub limit_rps: Option<i32>,
    /// New operator note, when set.
    pub note: Option<String>,
}

/// Apply a sparse patch to one override by id, returning the updated row.
///
/// Uses `COALESCE` per column so a single `UPDATE ... RETURNING` covers every
/// patch shape — no dynamic SQL.
///
/// # Errors
///
/// Returns [`StoreError::NotFound`] if `id` is unknown, or
/// [`StoreError::CheckViolation`] if the patched `limit_rps` is non-positive.
pub async fn update(pool: &PgPool, id: Uuid, patch: RateLimitPatch) -> Result<RateLimitOverride> {
    let row = sqlx::query_as::<_, OverrideRow>(
        "UPDATE rate_limit_override SET \
            expires_at = COALESCE($2, expires_at), \
            limit_rps = COALESCE($3, limit_rps), \
            note = COALESCE($4, note) \
         WHERE id = $1 \
         RETURNING id, cidr::text AS cidr, limit_rps, expires_at, note, created_by, created_at",
    )
    .bind(id)
    .bind(patch.expires_at)
    .bind(patch.limit_rps)
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
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<RateLimitOverride> {
    let row = sqlx::query_as::<_, OverrideRow>(
        "DELETE FROM rate_limit_override WHERE id = $1 \
         RETURNING id, cidr::text AS cidr, limit_rps, expires_at, note, created_by, created_at",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map_or(Err(StoreError::NotFound), |r| Ok(r.into()))
}

#[derive(sqlx::FromRow)]
struct OverrideRow {
    id: Uuid,
    cidr: String,
    limit_rps: i32,
    expires_at: OffsetDateTime,
    note: Option<String>,
    created_by: String,
    created_at: OffsetDateTime,
}

impl From<OverrideRow> for RateLimitOverride {
    fn from(r: OverrideRow) -> Self {
        Self {
            id: r.id,
            cidr: r.cidr,
            limit_rps: r.limit_rps,
            expires_at: r.expires_at,
            note: r.note,
            created_by: r.created_by,
            created_at: r.created_at,
        }
    }
}

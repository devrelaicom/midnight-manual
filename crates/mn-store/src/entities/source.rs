//! `source` entity queries.

use mn_core::types::{Source, SourceKind};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::Result;

/// Insert a new source, returning its newly-minted id.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::UniqueViolation`] if `slug` already exists.
pub async fn insert(
    pool: &PgPool,
    slug: &str,
    display_name: &str,
    kind: SourceKind,
    origin_url: Option<&str>,
    retention_count: i32,
) -> Result<Uuid> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO source (slug, display_name, kind, origin_url, retention_count) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(slug)
    .bind(display_name)
    .bind(
        serde_json::to_value(kind)
            .expect("SourceKind serializes")
            .as_str()
            .unwrap(),
    )
    .bind(origin_url)
    .bind(retention_count)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Fetch one source by slug.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if slug does not exist.
pub async fn get_by_slug(pool: &PgPool, slug: &str) -> Result<Source> {
    let row = sqlx::query_as::<_, SourceRow>(
        "SELECT id, slug, display_name, kind, origin_url, retention_count, created_at, retired_at \
         FROM source WHERE slug = $1",
    )
    .bind(slug)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

/// List all non-retired sources, ordered by slug.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn list_active(pool: &PgPool) -> Result<Vec<Source>> {
    let rows = sqlx::query_as::<_, SourceRow>(
        "SELECT id, slug, display_name, kind, origin_url, retention_count, created_at, retired_at \
         FROM source WHERE retired_at IS NULL ORDER BY slug",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// Mark a source as retired (idempotent: setting an already-retired row is a no-op).
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if `slug` is unknown.
pub async fn retire(pool: &PgPool, slug: &str) -> Result<()> {
    let result =
        sqlx::query("UPDATE source SET retired_at = COALESCE(retired_at, now()) WHERE slug = $1")
            .bind(slug)
            .execute(pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(crate::error::StoreError::NotFound);
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct SourceRow {
    id: Uuid,
    slug: String,
    display_name: String,
    kind: String,
    origin_url: Option<String>,
    retention_count: i32,
    created_at: OffsetDateTime,
    retired_at: Option<OffsetDateTime>,
}

impl TryFrom<SourceRow> for Source {
    type Error = crate::error::StoreError;

    fn try_from(r: SourceRow) -> std::result::Result<Self, Self::Error> {
        let kind: SourceKind = serde_json::from_value(serde_json::Value::String(r.kind))
            .map_err(|e| crate::error::StoreError::Json(e.to_string()))?;
        Ok(Self {
            id: r.id,
            slug: r.slug,
            display_name: r.display_name,
            kind,
            origin_url: r.origin_url,
            retention_count: r.retention_count,
            created_at: r.created_at,
            retired_at: r.retired_at,
        })
    }
}

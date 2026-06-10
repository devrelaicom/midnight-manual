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

/// Filterable, keyset-paginated source listing. Page key is `slug` (unique,
/// matches the existing ORDER BY). Returns one page plus the total row count
/// for the same filter set (ignoring the cursor).
#[derive(Debug, Default)]
pub struct SourcePageQuery {
    /// Resume after this slug (exclusive). `None` = first page.
    pub after_slug: Option<String>,
    /// Page size (validated by the route: 1..=100).
    pub limit: i64,
    /// Only sources created strictly after this instant.
    pub created_after: Option<OffsetDateTime>,
    /// Only sources created strictly before this instant.
    pub created_before: Option<OffsetDateTime>,
    /// Only sources of this kind (wire string, e.g. "docs_site").
    pub kind: Option<String>,
    /// Include retired sources (default false = active only).
    pub include_retired: bool,
}

/// One page of sources plus pagination facts.
#[derive(Debug)]
pub struct SourcePage {
    /// The page rows, ordered by slug.
    pub sources: Vec<Source>,
    /// Total rows matching the filters (cursor ignored).
    pub total: i64,
    /// Slug to resume from when more rows exist.
    pub next_after_slug: Option<String>,
}

/// Run a [`SourcePageQuery`].
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn list_paged(pool: &PgPool, q: &SourcePageQuery) -> Result<SourcePage> {
    fn push_filters<'a>(b: &mut sqlx::QueryBuilder<'a, sqlx::Postgres>, q: &'a SourcePageQuery) {
        b.push(" WHERE 1=1");
        if !q.include_retired {
            b.push(" AND retired_at IS NULL");
        }
        if let Some(t) = q.created_after {
            b.push(" AND created_at > ").push_bind(t);
        }
        if let Some(t) = q.created_before {
            b.push(" AND created_at < ").push_bind(t);
        }
        if let Some(k) = &q.kind {
            b.push(" AND kind = ").push_bind(k.as_str());
        }
    }

    let mut count = sqlx::QueryBuilder::new("SELECT count(*) FROM source");
    push_filters(&mut count, q);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;

    let mut page = sqlx::QueryBuilder::new(
        "SELECT id, slug, display_name, kind, origin_url, retention_count, created_at, retired_at \
         FROM source",
    );
    push_filters(&mut page, q);
    if let Some(after) = &q.after_slug {
        page.push(" AND slug > ").push_bind(after.as_str());
    }
    page.push(" ORDER BY slug LIMIT ").push_bind(q.limit + 1);
    let rows: Vec<SourceRow> = page.build_query_as().fetch_all(pool).await?;

    let page_len = usize::try_from(q.limit).unwrap_or(usize::MAX);
    let has_more = rows.len() > page_len;
    let sources: Vec<Source> = rows
        .into_iter()
        .take(page_len)
        .map(TryInto::try_into)
        .collect::<Result<_>>()?;
    let next_after_slug = if has_more {
        sources.last().map(|s| s.slug.clone())
    } else {
        None
    };
    Ok(SourcePage {
        sources,
        total,
        next_after_slug,
    })
}

/// List every source row, including retired ones, ordered by slug.
///
/// Distinct from [`list_active`] which filters out `retired_at IS NOT NULL`.
/// Used by admin-tier endpoints that need full operator visibility.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn list_all(pool: &PgPool) -> Result<Vec<Source>> {
    let rows = sqlx::query_as::<_, SourceRow>(
        "SELECT id, slug, display_name, kind, origin_url, retention_count, created_at, retired_at \
         FROM source ORDER BY slug",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// Sources whose ACTIVE version is NOT on `target_model_id`, ordered by best
/// (lowest-rank) document attribution then slug. Foundation(1) → Partner(2) →
/// ThirdParty(3) → Community(4) → Unknown(5).
///
/// Only non-retired sources with an active `source_version` are considered.
/// Sources whose active version has no documents at all sort as Unknown (rank 5).
///
/// # Errors
///
/// Returns a store error on driver failure or a malformed `kind`.
pub async fn list_active_not_on_model(pool: &PgPool, target_model_id: Uuid) -> Result<Vec<Source>> {
    let rows = sqlx::query_as::<_, SourceRow>(
        "SELECT s.id, s.slug, s.display_name, s.kind, s.origin_url, s.retention_count, s.created_at, s.retired_at \
         FROM source s \
         JOIN source_version sv ON sv.source_id = s.id AND sv.is_active = true \
         LEFT JOIN document d ON d.source_version_id = sv.id \
         WHERE s.retired_at IS NULL AND sv.embedding_model_id <> $1 \
         GROUP BY s.id, s.slug, s.display_name, s.kind, s.origin_url, s.retention_count, s.created_at, s.retired_at \
         ORDER BY MIN(CASE d.provenance->>'attribution' \
                        WHEN 'foundation' THEN 1 WHEN 'partner' THEN 2 WHEN 'third_party' THEN 3 \
                        WHEN 'community' THEN 4 ELSE 5 END) ASC NULLS LAST, s.slug ASC",
    )
    .bind(target_model_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// Sparse patch applied by [`update`] — `Some(value)` updates the column,
/// `None` leaves it untouched.
#[derive(Debug, Default, Clone)]
pub struct SourcePatch {
    /// New display label, when set.
    pub display_name: Option<String>,
    /// New origin URL, when set. (Use `Some(None)` semantics by passing an
    /// empty string here is NOT supported — `None` here means "no change".)
    pub origin_url: Option<String>,
    /// New retention count, when set. The DB CHECK constraint clamps to
    /// `[1, 50]`; callers should validate before calling.
    pub retention_count: Option<i32>,
}

/// Apply a sparse patch to one source by slug, returning the updated row.
///
/// Uses `COALESCE` per-column so a single `UPDATE ... RETURNING *` covers all
/// patch shapes — no dynamic SQL.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if `slug` is unknown.
/// Returns [`crate::error::StoreError::CheckViolation`] when `retention_count`
/// falls outside `[1, 50]`.
pub async fn update(pool: &PgPool, slug: &str, patch: SourcePatch) -> Result<Source> {
    let row = sqlx::query_as::<_, SourceRow>(
        "UPDATE source SET \
            display_name = COALESCE($2, display_name), \
            origin_url = COALESCE($3, origin_url), \
            retention_count = COALESCE($4, retention_count) \
         WHERE slug = $1 \
         RETURNING id, slug, display_name, kind, origin_url, retention_count, created_at, retired_at",
    )
    .bind(slug)
    .bind(patch.display_name)
    .bind(patch.origin_url)
    .bind(patch.retention_count)
    .fetch_optional(pool)
    .await?;
    row.map_or(Err(crate::error::StoreError::NotFound), TryInto::try_into)
}

/// Hard-delete sources whose `retired_at` is older than `grace_seconds`.
///
/// Cascades through `source_version` → `chunk` / `document` / `node` /
/// `package` via the existing `ON DELETE CASCADE` foreign keys, so one
/// DELETE removes the whole subtree. Returns the slugs that were deleted
/// (sorted ascending for stable logging).
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure. A
/// `grace_seconds` value of 0 means "delete anything currently retired";
/// negative values are clamped to 0.
pub async fn sweep_retired(pool: &PgPool, grace_seconds: i64) -> Result<Vec<String>> {
    let grace = grace_seconds.max(0);
    let rows: Vec<(String,)> = sqlx::query_as(
        "DELETE FROM source \
         WHERE retired_at IS NOT NULL \
           AND retired_at < now() - ($1::bigint * interval '1 second') \
         RETURNING slug",
    )
    .bind(grace)
    .fetch_all(pool)
    .await?;
    let mut slugs: Vec<String> = rows.into_iter().map(|(s,)| s).collect();
    slugs.sort();
    Ok(slugs)
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

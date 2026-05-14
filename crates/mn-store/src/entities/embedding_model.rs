//! `embedding_model` entity queries.

use mn_core::types::EmbeddingModel;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;

/// Look up an embedding_model row by `name` + `revision`.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if no matching row exists.
pub async fn get_by_name_revision(
    pool: &PgPool,
    name: &str,
    revision: i32,
) -> Result<EmbeddingModel> {
    let row = sqlx::query_as::<_, EmbeddingModelRow>(
        "SELECT id, name, revision, dim, provider, created_at \
         FROM embedding_model WHERE name = $1 AND revision = $2",
    )
    .bind(name)
    .bind(revision)
    .fetch_one(pool)
    .await?;
    Ok(row.into())
}

/// Resolve the "active" embedding model — the row used by the most recently
/// activated source_version. In v1 there is at most one such model; this
/// function returns that row.
///
/// Falls back to the most recently-created `embedding_model` if no source
/// version is active yet (fresh DB after `0006_seed_embedding_model.sql`).
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if no `embedding_model` row
/// exists at all (migrations not run or seed row missing).
pub async fn get_active(pool: &PgPool) -> Result<EmbeddingModel> {
    let row = sqlx::query_as::<_, EmbeddingModelRow>(
        "SELECT em.id, em.name, em.revision, em.dim, em.provider, em.created_at \
         FROM embedding_model em \
         LEFT JOIN source_version sv \
           ON sv.embedding_model_id = em.id AND sv.is_active = true \
         ORDER BY sv.ingested_at DESC NULLS LAST, em.created_at DESC \
         LIMIT 1",
    )
    .fetch_one(pool)
    .await?;
    Ok(row.into())
}

/// Look up the embedding_model row by its primary key.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if the id does not exist.
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> Result<EmbeddingModel> {
    let row = sqlx::query_as::<_, EmbeddingModelRow>(
        "SELECT id, name, revision, dim, provider, created_at \
         FROM embedding_model WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(row.into())
}

/// Insert an embedding_model row, returning the newly-minted id. Idempotent
/// on (name, revision): an existing row returns its existing id.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::CheckViolation`] if `dim` or `revision` violate the
/// table's CHECK constraints (revision >= 1, 64 <= dim <= 4096).
pub async fn upsert(
    pool: &PgPool,
    name: &str,
    revision: i32,
    dim: i32,
    provider: &str,
) -> Result<Uuid> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO embedding_model (name, revision, dim, provider) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (name, revision) DO UPDATE SET name = EXCLUDED.name \
         RETURNING id",
    )
    .bind(name)
    .bind(revision)
    .bind(dim)
    .bind(provider)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

#[derive(sqlx::FromRow)]
struct EmbeddingModelRow {
    id: Uuid,
    name: String,
    revision: i32,
    dim: i32,
    provider: String,
    created_at: time::OffsetDateTime,
}

impl From<EmbeddingModelRow> for EmbeddingModel {
    fn from(r: EmbeddingModelRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            revision: r.revision,
            dim: r.dim,
            provider: r.provider,
            created_at: r.created_at,
        }
    }
}

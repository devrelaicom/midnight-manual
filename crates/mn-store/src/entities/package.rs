//! `package` entity queries.

use mn_core::types::{Package, PackageKind};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;

/// Insert a package row, returning the newly-minted id. Idempotent on
/// `(source_version_id, kind, name)` — repeated calls return the existing id.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::ForeignKeyViolation`] if
/// `source_version_id` is unknown.
pub async fn upsert(
    pool: &PgPool,
    source_version_id: Uuid,
    kind: PackageKind,
    name: &str,
    version: Option<&str>,
    manifest_path: Option<&str>,
) -> Result<Uuid> {
    let kind_str = match kind {
        PackageKind::Rust => "rust",
        PackageKind::Npm => "npm",
        PackageKind::Compact => "compact",
        PackageKind::Other => "other",
    };
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO package (source_version_id, kind, name, version, manifest_path) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (source_version_id, kind, name) DO UPDATE SET \
             version = EXCLUDED.version, \
             manifest_path = EXCLUDED.manifest_path \
         RETURNING id",
    )
    .bind(source_version_id)
    .bind(kind_str)
    .bind(name)
    .bind(version)
    .bind(manifest_path)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// List all packages for a source_version.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn list_by_source_version(
    pool: &PgPool,
    source_version_id: Uuid,
) -> Result<Vec<Package>> {
    let rows = sqlx::query_as::<_, PackageRow>(
        "SELECT id, source_version_id, kind, name, version, manifest_path, metadata \
         FROM package WHERE source_version_id = $1 ORDER BY kind, name",
    )
    .bind(source_version_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

#[derive(sqlx::FromRow)]
struct PackageRow {
    id: Uuid,
    source_version_id: Uuid,
    kind: String,
    name: String,
    version: Option<String>,
    manifest_path: Option<String>,
    metadata: serde_json::Value,
}

impl TryFrom<PackageRow> for Package {
    type Error = crate::error::StoreError;

    fn try_from(r: PackageRow) -> std::result::Result<Self, Self::Error> {
        let kind: PackageKind = serde_json::from_value(serde_json::Value::String(r.kind))
            .map_err(|e| crate::error::StoreError::Json(e.to_string()))?;
        Ok(Self {
            id: r.id,
            source_version_id: r.source_version_id,
            kind,
            name: r.name,
            version: r.version,
            manifest_path: r.manifest_path,
            metadata: r.metadata,
        })
    }
}

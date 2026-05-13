//! Postgres connection pool builder.
//!
//! Migrations are invoked via [`run_migrations`] explicitly; the server's
//! startup sequence (FR-009.d) gates migration on `MIDNIGHT_MANUAL_AUTO_MIGRATE`.

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;

use crate::error::{Result, StoreError};

/// The compiled-in migrations live under `crates/mn-store/migrations/`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Build a `PgPool` from a libpq-style connection URL with sensible defaults.
///
/// Defaults:
/// - max connections = 16
/// - acquire timeout = 5s
/// - statement cache = enabled (sqlx default)
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] if the URL is malformed or the pool fails
/// its initial probe.
pub async fn connect(database_url: &str) -> Result<PgPool> {
    let opts: PgConnectOptions = database_url
        .parse()
        .map_err(|e: sqlx::Error| StoreError::Database(e.to_string()))?;

    let pool = PgPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect_with(opts)
        .await
        .map_err(StoreError::from)?;

    Ok(pool)
}

/// Run pending migrations against `pool`.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Migration`] on a failed migration.
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))
}

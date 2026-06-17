//! Storage-layer error wrapping. sqlx errors are mapped to typed [`StoreError`]
//! variants so callers don't need to match on sqlx internals.

use thiserror::Error;

/// All the ways a storage call can fail.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The row asked for does not exist.
    #[error("row not found")]
    NotFound,
    /// A uniqueness constraint was violated (slug, revision, etc.).
    #[error("unique constraint violated: {0}")]
    UniqueViolation(String),
    /// A check constraint or trigger rejected the operation.
    #[error("constraint violated: {0}")]
    CheckViolation(String),
    /// Foreign-key constraint violated.
    #[error("foreign-key violation: {0}")]
    ForeignKeyViolation(String),
    /// Any other database-side error.
    #[error("database error: {0}")]
    Database(String),
    /// Serialization failure when round-tripping a JSONB column.
    #[error("json serialization error: {0}")]
    Json(String),
    /// Migration runner failed.
    #[error("migration error: {0}")]
    Migration(String),
}

impl From<sqlx::Error> for StoreError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => Self::NotFound,
            sqlx::Error::Database(ref db_err) => {
                let constraint = db_err.constraint().unwrap_or("");
                let msg = db_err.message().to_string();
                let code = db_err.code().map(|c| c.to_string()).unwrap_or_default();
                // PostgreSQL SQLSTATE codes
                match code.as_str() {
                    "23505" => Self::UniqueViolation(format!("{constraint}: {msg}")),
                    "23503" => Self::ForeignKeyViolation(format!("{constraint}: {msg}")),
                    "23514" | "P0001" => Self::CheckViolation(format!("{constraint}: {msg}")),
                    _ => Self::Database(format!("{code}: {msg}")),
                }
            }
            sqlx::Error::Migrate(e) => Self::Migration(e.to_string()),
            sqlx::Error::ColumnDecode { source, .. } | sqlx::Error::Decode(source) => {
                Self::Json(source.to_string())
            }
            other => Self::Database(other.to_string()),
        }
    }
}

/// Crate-local Result alias.
pub type Result<T> = std::result::Result<T, StoreError>;

//! The corpus's active CODE embedding model (voyage-code-3 family), resolved
//! at boot from config (`MIDNIGHT_MANUAL_CODE_MODEL`, default
//! "voyage-code-3@1") against the embedding_model registry. Unlike the
//! general corpus model this is config-pinned, not activity-derived: the code
//! column always encodes with exactly the configured model.

use mnm_store::entities::embedding_model;
use sqlx::PgPool;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// The code-embedding model the corpus's `code_embedding` column uses.
#[derive(Debug, Clone)]
pub struct CodeModel {
    /// Wire id, e.g. "voyage-code-3@1".
    pub wire: String,
    /// Bare model name (e.g. "voyage-code-3"), used to build the server-side
    /// code embedder so the model that COMPUTES `/v1/embeddings` code vectors is
    /// the same one whose wire id LABELS them.
    pub name: String,
    /// Primary key, used to gate code-vector ANN by `sv.code_embedding_model_id`.
    pub id: Uuid,
    /// Vector dimension, used to validate inbound code query vectors.
    pub dim: usize,
}

/// Shared handle stored in AppState. `None` until resolved (production
/// resolves at boot; tests that don't exercise code search leave it `None` —
/// code_mode searches then 503).
pub type Shared = Arc<RwLock<Option<CodeModel>>>;

/// Split a `name@revision` wire id into its parts.
///
/// # Errors
/// Returns an error when the wire id has no `@` or a non-integer revision.
fn split_wire(wire: &str) -> anyhow::Result<(&str, i32)> {
    let (name, rev) = wire
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("code model wire id `{wire}` is not name@revision"))?;
    let revision: i32 = rev
        .parse()
        .map_err(|_| anyhow::anyhow!("code model wire id `{wire}` has a non-integer revision"))?;
    Ok((name, revision))
}

/// Resolve `wire` ("name@revision") against the registry.
///
/// # Errors
/// Returns an error when the wire id does not parse or is not registered.
pub async fn resolve(pool: &PgPool, wire: &str) -> anyhow::Result<CodeModel> {
    let (name, revision) = split_wire(wire)?;
    let m = embedding_model::get_by_name_revision(pool, name, revision).await?;
    Ok(CodeModel {
        wire: format!("{}@{}", m.name, m.revision),
        name: m.name,
        id: m.id,
        dim: usize::try_from(m.dim)
            .map_err(|_| anyhow::anyhow!("code model dim {} out of range", m.dim))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_wire_parses_name_and_revision() {
        let (name, rev) = split_wire("voyage-code-3@1").unwrap();
        assert_eq!(name, "voyage-code-3");
        assert_eq!(rev, 1);
    }

    #[test]
    fn split_wire_rejects_missing_at_and_bad_revision() {
        assert!(split_wire("voyage-code-3").is_err());
        assert!(split_wire("voyage-code-3@one").is_err());
        assert!(split_wire("voyage-code-3@").is_err());
    }
}

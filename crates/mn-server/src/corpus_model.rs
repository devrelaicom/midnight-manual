//! The corpus's active embedding model, resolvable at boot and (Task 3.4)
//! after each ingest finalize. Held behind an RwLock in AppState so promotions
//! take effect without a restart.
use mn_store::entities::embedding_model;
use sqlx::PgPool;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// The embedding model the corpus is currently encoded with.
#[derive(Debug, Clone)]
pub struct CorpusModel {
    /// Wire id, e.g. "voyage-code-3@1".
    pub wire: String,
    /// Primary key, used to filter chunks by `sv.embedding_model_id`.
    pub id: Uuid,
    /// Vector dimension, used to validate inbound query vectors.
    pub dim: usize,
}

/// Shared, re-resolvable handle stored in AppState. `None` until resolved
/// (production resolves at boot; some tests leave it unresolved).
pub type Shared = Arc<RwLock<Option<CorpusModel>>>;

/// Resolve the active model from the DB (mirrors the prior boot logic).
///
/// # Errors
/// Returns an error if no `embedding_model` row can be resolved.
pub async fn resolve(pool: &PgPool) -> anyhow::Result<CorpusModel> {
    let m = embedding_model::get_active(pool).await?;
    Ok(CorpusModel {
        wire: format!("{}@{}", m.name, m.revision),
        id: m.id,
        // A negative dim can't pass the DB's `64 <= dim <= 4096` CHECK, so this
        // only fires on a corrupt row — surface it rather than coerce to a 0
        // sentinel that the search dim guard (Task 3.3) would misreport.
        dim: usize::try_from(m.dim)
            .map_err(|_| anyhow::anyhow!("embedding model dim {} out of range for usize", m.dim))?,
    })
}

/// Re-resolve + swap in place (called after ingest finalize in Task 3.4).
pub async fn refresh(pool: &PgPool, shared: &Shared) {
    match resolve(pool).await {
        Ok(cm) => {
            tracing::info!(corpus_model = %cm.wire, "re-resolved corpus model");
            *shared.write().expect("corpus_model lock poisoned") = Some(cm);
        }
        Err(e) => tracing::warn!(error = %e, "corpus model refresh failed"),
    }
}

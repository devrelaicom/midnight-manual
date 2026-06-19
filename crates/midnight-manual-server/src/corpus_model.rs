//! The corpus's active embedding model, resolvable at boot and (Task 3.4)
//! after each ingest finalize. Held behind an RwLock in AppState so the wire-id
//! LABEL search stamps (and the `/v1/models/active` response) reflect a promotion
//! without a restart.
//!
//! Scope note — the server-side proxy embedders (`AppState::voyage` /
//! `voyage_ctx`, built by [`crate::app::resolved_embedders`]) are NOT re-resolved
//! here: they are PINNED to the model resolved at boot. So after a runtime
//! promotion onto a different model, this handle and the wire-id label move to
//! the new model while the proxy still COMPUTES with the boot model — a restart
//! is required to re-align them. [`refresh`] fails loud (a `tracing::warn!`)
//! when the re-resolved `name`/`dim` differs from the prior value so that drift
//! window is visible in logs rather than silent.
use mnm_store::entities::embedding_model;
use sqlx::PgPool;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// The embedding model the corpus is currently encoded with.
#[derive(Debug, Clone)]
pub struct CorpusModel {
    /// Wire id, e.g. "voyage-code-3@1".
    pub wire: String,
    /// Bare model name (e.g. "voyage-context-3"), used to build the server-side
    /// embedder so the model that COMPUTES `/v1/embeddings` vectors is the same
    /// one whose wire id LABELS them.
    pub name: String,
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
        name: m.name,
        id: m.id,
        // A negative dim can't pass the DB's `64 <= dim <= 4096` CHECK, so this
        // only fires on a corrupt row — surface it rather than coerce to a 0
        // sentinel that the search dim guard (Task 3.3) would misreport.
        dim: usize::try_from(m.dim)
            .map_err(|_| anyhow::anyhow!("embedding model dim {} out of range for usize", m.dim))?,
    })
}

/// Re-resolve + swap in place (called after ingest finalize in Task 3.4).
///
/// Fails loud: if the re-resolved model's `name`/`dim` differs from the value
/// this handle held before the swap, the boot-time proxy embedders
/// (`AppState::voyage` / `voyage_ctx`) — which are PINNED at boot and not
/// re-resolved — now compute with a model that no longer matches the wire-id
/// LABEL this handle stamps. A `tracing::warn!` surfaces that drift window
/// (resolved by a restart) instead of letting it pass silently.
pub async fn refresh(pool: &PgPool, shared: &Shared) {
    match resolve(pool).await {
        Ok(cm) => {
            // Snapshot the prior identity (name/dim) before swapping, so we can
            // detect a model change against the boot-pinned proxy embedders.
            let prior = shared
                .read()
                .expect("corpus_model lock poisoned")
                .as_ref()
                .map(|p| (p.name.clone(), p.dim));
            if let Some((prior_name, prior_dim)) = prior {
                if prior_name != cm.name || prior_dim != cm.dim {
                    tracing::warn!(
                        prior_model = %prior_name,
                        prior_dim,
                        new_model = %cm.name,
                        new_dim = cm.dim,
                        "corpus model changed at runtime; the server-side /v1/embeddings proxy is \
                         pinned to the boot-time model and will keep computing with it while \
                         stamping the NEW wire id — RESTART the server to re-align the proxy"
                    );
                }
            }
            tracing::info!(corpus_model = %cm.wire, "re-resolved corpus model");
            *shared.write().expect("corpus_model lock poisoned") = Some(cm);
        }
        Err(e) => tracing::warn!(error = %e, "corpus model refresh failed"),
    }
}

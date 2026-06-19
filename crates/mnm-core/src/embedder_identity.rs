//! Deriving the embedding-model identity (name + dim + dtype) used to COMPUTE
//! an embedding from the SAME source that LABELS the resulting vectors.
//!
//! The source of truth is the corpus's active model from
//! `GET /v1/models/active`.
//!
//! ## Why this exists (the cross-element drift bug)
//!
//! RAG correctness requires that query vectors and stored vectors come from the
//! SAME embedding model. Historically every client embedded with a model
//! NAME/dim/dtype taken from LOCAL CONFIG (`[models].embedding` /
//! `code_embedding` / `voyage_output_dimension` / `voyage_output_dtype`) but
//! LABELED the resulting vectors with the corpus wire id fetched separately from
//! `GET /v1/models/active`. The server's only consistency guard compares that
//! label string + vector length — it never checks the vectors were actually
//! produced by the labeled model. Because Voyage's voyage-3 / voyage-code-3 /
//! voyage-context-3 all emit 1024-dim float vectors, pointing a client's config
//! at a different-but-same-dimension model produced vectors that mismatched the
//! corpus yet passed both guards → silently-wrong cosine similarities.
//!
//! The fix: DERIVE the model name + dim + dtype that drive embedder construction
//! from the SAME active-model response that supplies the wire-id label, so the
//! two cannot diverge. Local config survives only as a logged fallback for the
//! offline / active-fetch-unavailable path.

/// The three values an embedder constructor needs:
/// `VoyageEmbedder::new(api_key, name, dim, dtype)` /
/// `ContextualizedVoyageEmbedder::new(api_key, name, dim, dtype)`.
///
/// The API key is intentionally NOT here — a key is auth, not model identity,
/// and is resolved independently (`resolve_voyage_api_key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedderIdentity {
    /// Bare model name (e.g. `voyage-context-3`), NOT the `name@revision` wire id.
    pub name: String,
    /// Matryoshka output dimension sent as `output_dimension`.
    pub dim: u32,
    /// Output dtype sent as `output_dtype` (e.g. `"float"`).
    pub dtype: String,
}

/// What the active-model fetch reported for one model (general or code).
///
/// All three fields are present iff the fetch succeeded AND the model exists; a
/// failed/absent fetch is represented by passing `None` to [`derive()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveModelIdentity {
    /// Bare model name from the active-model response.
    pub name: String,
    /// Dimension from the active-model response.
    pub dim: u32,
    /// Dtype from the active-model response (new field; `"float"` today).
    pub dtype: String,
}

/// Config-supplied fallback identity, used ONLY when the active-model fetch is
/// unavailable (offline behavior preservation). These values are no longer the
/// authority for what the embedder computes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackIdentity<'a> {
    /// Config model name (`[models].embedding` or `[models].code_embedding`).
    pub name: &'a str,
    /// Config dimension (`[models].voyage_output_dimension`).
    pub dim: u32,
    /// Config dtype (`[models].voyage_output_dtype`).
    pub dtype: &'a str,
}

/// Derive the embedder identity, preferring the active-model fetch over config.
///
/// When `active` is `Some`, its `{name, dim, dtype}` win outright — this is the
/// authoritative path that keeps the embed model and the wire-id label in
/// lockstep. When `active` is `None` (the fetch was unavailable), `fallback`
/// (local config) is used instead and a `tracing::warn!` is emitted so the
/// degraded path is visible in logs; this preserves offline behavior but is no
/// longer trusted for correctness.
///
/// `which` is a short label (`"general"` / `"code"`) for the warning line.
///
/// Use [`derive_quiet`] when the caller has ALREADY logged the fetch failure
/// (so a single event doesn't warn once per identity) or when the derived
/// identity won't actually drive an embed (e.g. fts mode embeds nothing) — both
/// would otherwise emit a misleading "vectors are NOT guaranteed to match"
/// warning for a vector that is never produced.
#[must_use]
pub fn derive(
    which: &str,
    active: Option<&ActiveModelIdentity>,
    fallback: &FallbackIdentity<'_>,
) -> EmbedderIdentity {
    derive_inner(which, active, fallback, true)
}

/// Like [`derive()`] but suppresses the fallback `tracing::warn!`.
///
/// For callers that already logged the active-fetch failure (avoiding a
/// warn-per-identity for one event) or that won't use the identity for an
/// actual embed.
#[must_use]
pub fn derive_quiet(
    which: &str,
    active: Option<&ActiveModelIdentity>,
    fallback: &FallbackIdentity<'_>,
) -> EmbedderIdentity {
    derive_inner(which, active, fallback, false)
}

fn derive_inner(
    which: &str,
    active: Option<&ActiveModelIdentity>,
    fallback: &FallbackIdentity<'_>,
    warn_on_fallback: bool,
) -> EmbedderIdentity {
    active.map_or_else(
        || {
            if warn_on_fallback {
                tracing::warn!(
                    which,
                    fallback_model = fallback.name,
                    fallback_dim = fallback.dim,
                    fallback_dtype = fallback.dtype,
                    "active-model fetch unavailable; embedding with local config as a fallback \
                     (vectors are NOT guaranteed to match the corpus model)"
                );
            }
            EmbedderIdentity {
                name: fallback.name.to_owned(),
                dim: fallback.dim,
                dtype: fallback.dtype.to_owned(),
            }
        },
        |a| EmbedderIdentity {
            name: a.name.clone(),
            dim: a.dim,
            dtype: a.dtype.clone(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fallback() -> FallbackIdentity<'static> {
        FallbackIdentity {
            name: "config-model",
            dim: 256,
            dtype: "int8",
        }
    }

    /// The authoritative path: an active-model response drives the embedder
    /// identity verbatim, NOT the (divergent) config. This is the property the
    /// cross-element drift bug violated.
    #[test]
    fn derive_prefers_active_over_divergent_config() {
        let active = ActiveModelIdentity {
            name: "voyage-context-3".to_owned(),
            dim: 1024,
            dtype: "float".to_owned(),
        };
        let id = derive("general", Some(&active), &fallback());
        // Every field comes from the active response, none from the config fallback.
        assert_eq!(id.name, "voyage-context-3");
        assert_eq!(id.dim, 1024);
        assert_eq!(id.dtype, "float");
        assert_ne!(id.name, "config-model");
        assert_ne!(id.dim, 256);
        assert_ne!(id.dtype, "int8");
    }

    /// The offline path: with no active-model response, config is the fallback.
    #[test]
    fn derive_falls_back_to_config_when_active_absent() {
        let id = derive("code", None, &fallback());
        assert_eq!(id.name, "config-model");
        assert_eq!(id.dim, 256);
        assert_eq!(id.dtype, "int8");
    }
}

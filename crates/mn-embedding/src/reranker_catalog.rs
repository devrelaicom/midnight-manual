//! Reranker catalog: maps a config id to a loadable spec. Reranking is always
//! client-side. See the design doc §8 for the curated list + licences.

use std::path::{Path, PathBuf};

use fastembed::RerankerModel;

/// A resolved reranker the loader (Task 9.3) can instantiate.
#[derive(Debug)]
pub enum RerankerSpec {
    /// A fastembed-native reranker (downloaded/managed by fastembed).
    Native(RerankerModel),
    /// A user-defined ONNX reranker fetched from a Hugging Face repo via hf-hub;
    /// `model_file` is the ONNX file within the repo (tokenizer files sit beside it).
    UserOnnx {
        /// Hugging Face repo id (e.g. `Xenova/ms-marco-MiniLM-L-6-v2`).
        repo: &'static str,
        /// Path to the ONNX model file within the repo.
        model_file: &'static str,
    },
    /// A reranker loaded from a local ONNX model directory (`--reranker-path`).
    CustomPath(PathBuf),
    /// A VoyageAI API reranker; the string is the Voyage model name
    /// (e.g. `"rerank-2.5-lite"`). Requires `VOYAGE_API_KEY` at use time.
    Voyage(String),
}

/// Errors from [`resolve`].
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The id is not in the catalog.
    #[error("unknown reranker id `{0}` (see `mnm models pull --help` for the catalog)")]
    Unknown(String),
    /// `custom` was requested without a path.
    #[error("reranker `custom` requires a --reranker-path / models.reranker_path")]
    CustomPathMissing,
}

/// Resolve a reranker config `id` into a [`RerankerSpec`].
///
/// `custom_path` is only consulted for the `custom` id. Does NOT touch the
/// network or load any model — purely a table lookup. Voyage ids resolve
/// without a key here; the key is required only at use time (Task 9.3).
///
/// # Errors
///
/// Returns [`CatalogError::Unknown`] for an unrecognised id, or
/// [`CatalogError::CustomPathMissing`] when `id == "custom"` and `custom_path`
/// is `None`.
pub fn resolve(id: &str, custom_path: Option<&Path>) -> Result<RerankerSpec, CatalogError> {
    Ok(match id {
        // ── fastembed native ─────────────────────────────────────────────
        "bge-reranker-base" => RerankerSpec::Native(RerankerModel::BGERerankerBase),
        "bge-reranker-v2-m3" => RerankerSpec::Native(RerankerModel::BGERerankerV2M3),
        "jina-reranker-v1-turbo-en" => RerankerSpec::Native(RerankerModel::JINARerankerV1TurboEn),
        // NOTE: jina-reranker-v2-base-multilingual is intentionally excluded
        // (cc-by-nc-4.0 — non-commercial).
        // ── user-defined ONNX (Xenova mirrors for MiniLM; self-supply for mxbai) ──
        "ms-marco-minilm-l2" => RerankerSpec::UserOnnx {
            repo: "Xenova/ms-marco-MiniLM-L-2-v2",
            model_file: "onnx/model.onnx",
        },
        "ms-marco-minilm-l6" => RerankerSpec::UserOnnx {
            repo: "Xenova/ms-marco-MiniLM-L-6-v2",
            model_file: "onnx/model.onnx",
        },
        "ms-marco-minilm-l12" => RerankerSpec::UserOnnx {
            repo: "Xenova/ms-marco-MiniLM-L-12-v2",
            model_file: "onnx/model.onnx",
        },
        "mxbai-rerank-base-v1" => RerankerSpec::UserOnnx {
            repo: "mixedbread-ai/mxbai-rerank-base-v1",
            model_file: "onnx/model.onnx",
        },
        // experimental
        "mxbai-rerank-base-v2" => RerankerSpec::UserOnnx {
            repo: "mixedbread-ai/mxbai-rerank-base-v2",
            model_file: "onnx/model.onnx",
        },
        // ── custom local path ────────────────────────────────────────────
        "custom" => RerankerSpec::CustomPath(
            custom_path
                .ok_or(CatalogError::CustomPathMissing)?
                .to_path_buf(),
        ),
        // ── Voyage API (requires VOYAGE_API_KEY at use) ──────────────────
        "voyage-rerank-2.5" => RerankerSpec::Voyage("rerank-2.5".to_owned()),
        "voyage-rerank-2.5-lite" => RerankerSpec::Voyage("rerank-2.5-lite".to_owned()),
        "voyage-rerank-2" => RerankerSpec::Voyage("rerank-2".to_owned()),
        other => return Err(CatalogError::Unknown(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_ids_resolve() {
        assert!(matches!(resolve("bge-reranker-base", None).unwrap(), RerankerSpec::Native(_)));
        assert!(matches!(
            resolve("jina-reranker-v1-turbo-en", None).unwrap(),
            RerankerSpec::Native(_)
        ));
        assert!(matches!(resolve("bge-reranker-v2-m3", None).unwrap(), RerankerSpec::Native(_)));
    }
    #[test]
    fn onnx_ids_resolve_to_hf_repo() {
        assert!(matches!(
            resolve("ms-marco-minilm-l6", None).unwrap(),
            RerankerSpec::UserOnnx { .. }
        ));
    }
    #[test]
    fn voyage_ids_require_key_at_use_but_resolve_spec() {
        assert!(matches!(
            resolve("voyage-rerank-2.5-lite", None).unwrap(),
            RerankerSpec::Voyage(_)
        ));
    }
    #[test]
    fn custom_requires_path() {
        assert!(resolve("custom", None).is_err());
        assert!(matches!(
            resolve("custom", Some(std::path::Path::new("/tmp/m"))).unwrap(),
            RerankerSpec::CustomPath(_)
        ));
    }
    #[test]
    fn unknown_id_errors() {
        assert!(resolve("nope", None).is_err());
    }
}

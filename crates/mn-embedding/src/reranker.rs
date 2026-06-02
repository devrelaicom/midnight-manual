//! `bge-reranker-base` cross-encoder wrapper (D2).
//!
//! Lazy singleton behind a `OnceCell`. The reranker is used MCP-side only; the
//! cloud server never sees a reranker invocation. It is the only fastembed
//! model left in the corpus path — the embedder is now VoyageAI (remote).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fastembed::{
    OnnxSource, RerankInitOptions, RerankInitOptionsUserDefined, RerankerModel, TextRerank,
    TokenizerFiles, UserDefinedRerankingModel,
};
use tokio::sync::OnceCell;

use crate::error::{EmbeddingError, Result};
use crate::reranker_catalog::RerankerSpec;
use crate::voyage::VoyageReranker;

/// The tokenizer-side files a fastembed user-defined model needs, beside the
/// ONNX graph. Order matches [`TokenizerFiles`]'s fields below.
const TOKENIZER_FILES: [&str; 4] = [
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];

/// The ONNX graph file name expected inside a `--reranker-path` directory.
const CUSTOM_ONNX_FILE: &str = "model.onnx";

/// Canonical wire name for the v1 reranker.
pub const MODEL_NAME: &str = "bge-reranker-base";

/// Reranker handle. Cheap to clone; the heavy `TextRerank` lives behind an
/// `Arc` and is initialized once.
#[derive(Clone)]
pub struct Reranker {
    inner: Arc<TextRerank>,
}

impl std::fmt::Debug for Reranker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reranker")
            .field("model", &MODEL_NAME)
            .finish()
    }
}

/// One reranker result. Higher `score` means more relevant.
#[derive(Debug, Clone, PartialEq)]
pub struct RerankResult {
    /// The input document index this result corresponds to.
    pub index: usize,
    /// The cross-encoder relevance logit (not normalized).
    pub score: f32,
}

/// Canonical catalog id for a fastembed-native reranker, used as the error
/// label so failures name the model the way the user selected it (e.g.
/// `bge-reranker-base`) rather than the enum's Debug spelling.
const fn native_label(model: &RerankerModel) -> &'static str {
    match model {
        RerankerModel::BGERerankerBase => MODEL_NAME,
        RerankerModel::BGERerankerV2M3 => "bge-reranker-v2-m3",
        RerankerModel::JINARerankerV1TurboEn => "jina-reranker-v1-turbo-en",
        RerankerModel::JINARerankerV2BaseMultiligual => "jina-reranker-v2-base-multilingual",
    }
}

impl Reranker {
    /// Build the default `bge-reranker-base` reranker. First call downloads
    /// ~270 MB into `cache_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Init`] if fastembed fails to instantiate the
    /// model (network failure, corrupted cache, etc.).
    pub fn try_new(cache_dir: PathBuf) -> Result<Self> {
        Self::try_new_model(RerankerModel::BGERerankerBase, cache_dir)
    }

    /// Build a fastembed-native reranker `model`, downloading into `cache_dir`.
    ///
    /// Generalises [`Reranker::try_new`] across the native catalog (bge / jina /
    /// …); the singleton path still pins `bge-reranker-base` via [`try_new`].
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Init`] if fastembed fails to instantiate the
    /// model.
    ///
    /// [`try_new`]: Reranker::try_new
    pub fn try_new_model(model: RerankerModel, cache_dir: PathBuf) -> Result<Self> {
        let label = native_label(&model);
        let opts = RerankInitOptions::new(model).with_cache_dir(cache_dir);
        let model = TextRerank::try_new(opts).map_err(|e| EmbeddingError::Init {
            model: label.to_owned(),
            message: e.to_string(),
        })?;
        Ok(Self { inner: Arc::new(model) })
    }

    /// Build a *user-defined* ONNX reranker by downloading the model + tokenizer
    /// files from a Hugging Face `repo` (via hf-hub's blocking API) into
    /// `cache_dir`.
    ///
    /// `model_file` is the ONNX path within the repo (e.g. `onnx/model.onnx`);
    /// the four tokenizer files (`tokenizer.json`, `config.json`,
    /// `special_tokens_map.json`, `tokenizer_config.json`) are fetched from the
    /// repo root.
    ///
    /// This performs blocking network + disk I/O and must not be called on an
    /// async executor thread; [`LoadedReranker::load`] wraps it in
    /// `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Init`] (tagged with `repo`) on any download,
    /// file-read, or model-instantiation failure.
    pub fn try_new_user_defined(repo: &str, model_file: &str, cache_dir: PathBuf) -> Result<Self> {
        let init_err = |message: String| EmbeddingError::Init {
            model: repo.to_owned(),
            message,
        };

        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_progress(false)
            .with_cache_dir(cache_dir)
            .build()
            .map_err(|e| init_err(format!("hf-hub init failed: {e}")))?;
        let api_repo = api.model(repo.to_owned());

        let onnx_path = api_repo
            .get(model_file)
            .map_err(|e| init_err(format!("download `{model_file}` failed: {e}")))?;

        let tokenizer_files = download_tokenizer_files(&api_repo, &init_err)?;

        let model = UserDefinedRerankingModel::new(OnnxSource::File(onnx_path), tokenizer_files);
        let model =
            TextRerank::try_new_from_user_defined(model, RerankInitOptionsUserDefined::default())
                .map_err(|e| init_err(e.to_string()))?;
        Ok(Self { inner: Arc::new(model) })
    }

    /// Build a user-defined ONNX reranker from a local `dir`, reading
    /// `model.onnx` plus the four tokenizer files from it.
    ///
    /// Used by `--reranker-path` / `RerankerSpec::CustomPath`.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Init`] (tagged `custom:<dir>`) if any file is
    /// missing/unreadable or fastembed fails to instantiate the model.
    pub fn try_new_user_defined_path(dir: &Path) -> Result<Self> {
        let init_err = |message: String| EmbeddingError::Init {
            model: format!("custom:{}", dir.display()),
            message,
        };

        let onnx_path = dir.join(CUSTOM_ONNX_FILE);
        if !onnx_path.is_file() {
            return Err(init_err(format!("missing `{CUSTOM_ONNX_FILE}` in {}", dir.display())));
        }

        let tokenizer_files = read_tokenizer_files(dir, &init_err)?;

        let model = UserDefinedRerankingModel::new(OnnxSource::File(onnx_path), tokenizer_files);
        let model =
            TextRerank::try_new_from_user_defined(model, RerankInitOptionsUserDefined::default())
                .map_err(|e| init_err(e.to_string()))?;
        Ok(Self { inner: Arc::new(model) })
    }

    /// Rerank `documents` against `query`. Returns one [`RerankResult`] per
    /// input document in the input order (not sorted — callers usually sort
    /// by score descending and take the top K).
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Inference`] on tokenizer or ONNX runtime
    /// failure.
    pub fn rerank(
        &self,
        query: &str,
        documents: &[String],
        batch_size: Option<usize>,
    ) -> Result<Vec<RerankResult>> {
        let doc_refs: Vec<&str> = documents.iter().map(String::as_str).collect();
        let results = self
            .inner
            .rerank(query, doc_refs, false, batch_size)
            .map_err(|e| EmbeddingError::Inference {
                model: MODEL_NAME.to_owned(),
                message: e.to_string(),
            })?;
        Ok(results
            .into_iter()
            .map(|r| RerankResult { index: r.index, score: r.score })
            .collect())
    }

    /// Async-friendly variant of [`Reranker::rerank`] that offloads the
    /// CPU-bound cross-encoder inference to a blocking thread.
    ///
    /// # Errors
    ///
    /// Same as [`Reranker::rerank`].
    pub async fn rerank_blocking(
        &self,
        query: String,
        documents: Vec<String>,
        batch_size: Option<usize>,
    ) -> Result<Vec<RerankResult>> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.rerank(&query, &documents, batch_size))
            .await
            .map_err(|e| EmbeddingError::Inference {
                model: MODEL_NAME.to_owned(),
                message: format!("blocking task failed: {e}"),
            })?
    }
}

/// Assemble [`TokenizerFiles`] from four byte buffers in the canonical order:
/// `tokenizer.json`, `config.json`, `special_tokens_map.json`,
/// `tokenizer_config.json` (matching [`TOKENIZER_FILES`]).
fn tokenizer_files_from(bytes: [Vec<u8>; 4]) -> TokenizerFiles {
    let [tokenizer_file, config_file, special_tokens_map_file, tokenizer_config_file] = bytes;
    TokenizerFiles {
        tokenizer_file,
        config_file,
        special_tokens_map_file,
        tokenizer_config_file,
    }
}

/// Download the four tokenizer files from `api_repo` and read their bytes.
fn download_tokenizer_files(
    api_repo: &hf_hub::api::sync::ApiRepo,
    init_err: &impl Fn(String) -> EmbeddingError,
) -> Result<TokenizerFiles> {
    let mut bytes: Vec<Vec<u8>> = Vec::with_capacity(TOKENIZER_FILES.len());
    for name in TOKENIZER_FILES {
        let path = api_repo
            .get(name)
            .map_err(|e| init_err(format!("download `{name}` failed: {e}")))?;
        let buf = std::fs::read(&path)
            .map_err(|e| init_err(format!("read `{}` failed: {e}", path.display())))?;
        bytes.push(buf);
    }
    let bytes: [Vec<u8>; 4] = bytes
        .try_into()
        .expect("TOKENIZER_FILES has exactly four entries");
    Ok(tokenizer_files_from(bytes))
}

/// Read the four tokenizer files from a local `dir` into bytes.
fn read_tokenizer_files(
    dir: &Path,
    init_err: &impl Fn(String) -> EmbeddingError,
) -> Result<TokenizerFiles> {
    let mut bytes: Vec<Vec<u8>> = Vec::with_capacity(TOKENIZER_FILES.len());
    for name in TOKENIZER_FILES {
        let path = dir.join(name);
        let buf = std::fs::read(&path)
            .map_err(|e| init_err(format!("read `{}` failed: {e}", path.display())))?;
        bytes.push(buf);
    }
    let bytes: [Vec<u8>; 4] = bytes
        .try_into()
        .expect("TOKENIZER_FILES has exactly four entries");
    Ok(tokenizer_files_from(bytes))
}

/// A reranker resolved + loaded from a [`RerankerSpec`]: either a local
/// cross-encoder (fastembed) or the Voyage API client.
///
/// Construct via [`LoadedReranker::load`]; rerank via [`LoadedReranker::rerank`].
pub enum LoadedReranker {
    /// A local fastembed cross-encoder (native, user-defined ONNX, or custom).
    Local(Reranker),
    /// The VoyageAI rerank API client.
    Voyage(VoyageReranker),
}

impl std::fmt::Debug for LoadedReranker {
    /// Prints only the variant name. Deliberately does NOT delegate to the
    /// inner `VoyageReranker` (which holds the API key) so the key never lands
    /// in logs or panic output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let variant = match self {
            Self::Local(_) => "Local",
            Self::Voyage(_) => "Voyage",
        };
        f.debug_tuple("LoadedReranker").field(&variant).finish()
    }
}

impl LoadedReranker {
    /// Load the reranker described by `spec`. `voyage_key` is required for
    /// [`RerankerSpec::Voyage`] specs and ignored otherwise. `cache_dir` is the
    /// on-disk model cache for the fastembed-backed variants.
    ///
    /// The blocking model load (download + ONNX session build) runs on a
    /// `spawn_blocking` thread so the async executor is never blocked.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Init`] if the model can't be loaded, or a
    /// [`RerankerSpec::Voyage`] spec is given without `voyage_key`.
    pub async fn load(
        spec: RerankerSpec,
        cache_dir: PathBuf,
        voyage_key: Option<&str>,
    ) -> Result<Self> {
        match spec {
            RerankerSpec::Native(model) => {
                let reranker =
                    spawn_load(move || Reranker::try_new_model(model, cache_dir)).await?;
                Ok(Self::Local(reranker))
            }
            RerankerSpec::UserOnnx { repo, model_file } => {
                let reranker =
                    spawn_load(move || Reranker::try_new_user_defined(repo, model_file, cache_dir))
                        .await?;
                Ok(Self::Local(reranker))
            }
            RerankerSpec::CustomPath(dir) => {
                let reranker =
                    spawn_load(move || Reranker::try_new_user_defined_path(&dir)).await?;
                Ok(Self::Local(reranker))
            }
            RerankerSpec::Voyage(model) => {
                let key = voyage_key.ok_or_else(|| EmbeddingError::Init {
                    model: model.clone(),
                    message: "Voyage reranker requires VOYAGE_API_KEY".to_owned(),
                })?;
                Ok(Self::Voyage(VoyageReranker::new(key, &model)))
            }
        }
    }

    /// Rerank `documents` against `query`. Returns one [`RerankResult`] per
    /// returned document. (Voyage may return fewer when `top_k` is set; here
    /// `top_k` is `None`, so all documents are scored.)
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Inference`] on a rerank failure.
    pub async fn rerank(&self, query: String, documents: Vec<String>) -> Result<Vec<RerankResult>> {
        match self {
            Self::Local(reranker) => reranker.rerank_blocking(query, documents, None).await,
            Self::Voyage(client) => client
                .rerank(query, documents, None)
                .await
                .map(|out| out.results)
                .map_err(|e| EmbeddingError::Inference {
                    model: "voyage-rerank".to_owned(),
                    message: e.to_string(),
                }),
        }
    }
}

/// Run a blocking reranker-load closure on a `spawn_blocking` thread, mapping a
/// join failure to [`EmbeddingError::Init`].
async fn spawn_load<F>(load: F) -> Result<Reranker>
where
    F: FnOnce() -> Result<Reranker> + Send + 'static,
{
    tokio::task::spawn_blocking(load)
        .await
        .map_err(|e| EmbeddingError::Init {
            model: "reranker".to_owned(),
            message: format!("blocking load task failed: {e}"),
        })?
}

/// Process-wide lazy singleton. First call loads the model; concurrent callers
/// all wait on the same `OnceCell::get_or_try_init` future.
static GLOBAL: OnceCell<Reranker> = OnceCell::const_new();

/// Get the process-wide reranker, initializing on first call.
///
/// # Errors
///
/// See [`Reranker::try_new`].
pub async fn global(cache_dir: PathBuf) -> Result<Reranker> {
    GLOBAL
        .get_or_try_init(|| async move {
            tokio::task::spawn_blocking(move || Reranker::try_new(cache_dir))
                .await
                .map_err(|e| EmbeddingError::Init {
                    model: MODEL_NAME.to_owned(),
                    message: format!("blocking init task failed: {e}"),
                })?
        })
        .await
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerank_result_round_trips_into_top_k_select() {
        // Smoke test on the RerankResult shape: sorting by score descending
        // and taking the top-K is the canonical caller flow.
        let mut results = vec![
            RerankResult { index: 0, score: 0.1 },
            RerankResult { index: 1, score: 0.9 },
            RerankResult { index: 2, score: 0.5 },
        ];
        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        assert_eq!(results[0].index, 1);
        assert_eq!(results[1].index, 2);
        assert_eq!(results[2].index, 0);
    }
}

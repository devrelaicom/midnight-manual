# VoyageAI Embeddings, Token Limits & Configurable Reranker — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the local fastembed corpus embedder with VoyageAI `voyage-code-3` (client-side, BYOK-or-server), add tiered in-memory embedding token limits with admin overrides + a `/v1/embeddings` endpoint, a re-ingest-per-source model-migration command, and a configurable reranker catalog.

**Architecture:** Embedding stays client-side — `mnm` produces query/chunk vectors either by calling Voyage directly (BYOK) or via a new token-limited server `/v1/embeddings` endpoint, then POSTs vectors to the existing `/v1/search`. The corpus is pinned to one model via the existing immutable-`source_version` + `is_active` machinery; migration is re-ingestion per source. Token usage is tracked in-memory (rolling minute-ring + hour-buckets) with a periodic DB snapshot for restart durability, and overrides live in Postgres + a refreshed in-memory cache (mirroring `rate_limit_override`). Reranking remains client-side with a selectable catalog (fastembed native + custom ONNX + Voyage API).

**Tech Stack:** Rust (workspace: `mn-core`, `mn-store`, `mn-retrieval`, `mn-content`, `mn-embedding`, `mn-auth`, `mn-telemetry`, `mn-mcp`, `mn-cli`, `mn-server`); `axum`, `sqlx` (Postgres + pgvector, **runtime-checked queries — no offline prep needed**), `reqwest` (0.12, json+rustls — already a dep), `fastembed` 4.9.1, `hf-hub`, `tokenizers`, `clap` v4, `wiremock` (test HTTP mock), `testcontainers` (integration DB). Reference spec: `docs/superpowers/specs/2026-06-02-voyage-embeddings-token-limits-design.md`.

---

## Conventions (read once)

- **Quality gate after every task:** `just check` (= `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`). Steps below call narrower commands for speed; run `just check` before each commit.
- **Integration tests** need `--features integration` and boot an ephemeral pgvector via `testcontainers` (or `DATABASE_URL` if set): `cargo test -p mn-store --features integration`. The harness is `crates/mn-store/tests/common/mod.rs::boot()` returning `Harness { pool, .. }`.
- **sqlx is runtime-checked** (`sqlx::query`/`query_as::<_, Row>`), so new SQL needs **no** `cargo sqlx prepare`.
- **HTTP mocking** uses `wiremock` (already a workspace dep): `MockServer::start()`, `Mock::given(method("POST")).and(path("/v1/embeddings"))`.
- **Commit cadence:** one commit per task (or per cohesive step group). Conventional commits, e.g. `feat(mn-embedding): add VoyageEmbedder`. End messages with the `Co-Authored-By: Claude Code <noreply@anthropic.com>` trailer.
- **Env var prefix** is `MIDNIGHT_MANUAL_*`; the env-parse idiom is `env::var("…").ok().and_then(|s| s.parse().ok()).map_or(DEFAULT, |v| v.max(MIN))`.
- **Wire model id** format is `{name}@{revision}` (`mn-core::model_id::EmbeddingModelId`).

## File-structure map

**Created**
- `crates/mn-embedding/src/voyage.rs` — `VoyageEmbedder` (reqwest client; embeddings) + `VoyageReranker` (rerank); shared `VoyageError`.
- `crates/mn-embedding/src/reranker_catalog.rs` — reranker registry: native / user-defined-ONNX / custom-path resolution.
- `crates/mn-embedding/src/client.rs` — client-side embedding resolution helper (BYOK direct vs server `/v1/embeddings`), shared by `mn-cli` (`search`/`ingest`) **and** `mn-mcp`.
- `crates/mn-cli/src/commands/tokenlimits.rs` — `mnm tokenlimits {add,list,extend,remove}` (mirrors `ratelimits.rs`).
- `crates/mn-store/migrations/0008_voyage_embedding.sql` — register `voyage-code-3@1`, `chunk.embedding → vector(1024)`, HNSW recreate.
- `crates/mn-store/migrations/0009_token_limits.sql` — `token_limit_override` + `token_usage_snapshot` tables.
- `crates/mn-store/src/entities/token_limit_override.rs` — entity (mirrors `rate_limit_override.rs`).
- `crates/mn-server/src/tokenlimit.rs` — bucketed `TokenUsageLimiter` (minute-ring + hour-buckets + override cache).
- `crates/mn-server/src/routes/embeddings.rs` — `POST /v1/embeddings`.
- `crates/mn-server/src/routes/admin_tokenlimits.rs` — `/v1/admin/tokenlimits` CRUD.
- `crates/mn-server/src/jobs/token_usage_snapshot.rs` — periodic snapshot + reaper.
- `docs/README-deploy.md` additions + README "Embeddings & third-party processing".
- `tests/canary/embeddings_no_query_text.rs` (or extend existing canary) — privacy canary.

**Modified**
- `crates/mn-core/src/config.rs` — `ModelsConfig` (+ voyage key/model/dim/dtype, reranker id + path); helper for flag>env>config resolution.
- `crates/mn-embedding/src/lib.rs` — export `voyage`, `reranker_catalog`.
- `crates/mn-store/src/entities/source.rs` (+ `mod.rs`) — `list_active_not_on_model(...)` query (provenance-ordered).
- `crates/mn-server/src/config.rs` — voyage + token-limit env fields.
- `crates/mn-server/src/state.rs`/`app.rs` — `AppState`: `Arc<RwLock<CorpusModel>>`, `Arc<VoyageEmbedder>` (optional), `Arc<TokenUsageLimiter>`.
- `crates/mn-server/src/routes/search.rs` — dim from corpus model; `sv.embedding_model_id = $corpus_model_id` filter.
- `crates/mn-server/src/routes/admin_ingest.rs` — re-resolve corpus model after finalize.
- `crates/mn-server/src/routes/admin_sources.rs` (or `models`) — `GET /v1/admin/sources?not_model=`.
- `crates/mn-server/src/main.rs` / `app.rs` — wire new state, routes, snapshot job, override refresh.
- `crates/mn-cli/src/cli.rs` — add `Tokenlimits`; extend admin gate.
- `crates/mn-cli/src/commands/models.rs` — add `migrate`/`status`.
- `crates/mn-cli/src/commands/search.rs`, `crates/mn-cli/src/commands/ingest/run.rs` — use `embed.rs`.
- `crates/mn-mcp/src/tools.rs` — Voyage/server embedding; reranker selection.

---

## Phase 1 — `mn-core` config additions

Adds the configuration surface (no behavior change). Everything downstream reads these.

### Task 1.1: Extend `ModelsConfig` with Voyage + reranker fields

**Files:**
- Modify: `crates/mn-core/src/config.rs:49-70` (`ModelsConfig` + `Default`)
- Test: `crates/mn-core/src/config.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `config.rs`:

```rust
#[test]
fn models_config_defaults_to_voyage_code_3() {
    let m = ModelsConfig::default();
    assert_eq!(m.embedding, "voyage-code-3");
    assert_eq!(m.reranker, "bge-reranker-base");
    assert_eq!(m.voyage_output_dimension, 1024);
    assert_eq!(m.voyage_output_dtype, "float");
    assert!(m.voyage_api_key.is_none());
    assert!(m.reranker_path.is_none());
}

#[test]
fn models_config_roundtrips_through_toml() {
    let toml_src = r#"
embedding = "voyage-code-3"
reranker = "jina-reranker-v1-turbo-en"
voyage_output_dimension = 1024
voyage_output_dtype = "float"
"#;
    let m: ModelsConfig = toml::from_str(toml_src).unwrap();
    assert_eq!(m.reranker, "jina-reranker-v1-turbo-en");
    assert_eq!(m.voyage_output_dimension, 1024);
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p mn-core config::tests::models_config_defaults_to_voyage_code_3`
Expected: FAIL — fields `voyage_output_dimension` etc. don't exist (compile error).

- [ ] **Step 3: Implement the fields**

Replace `ModelsConfig` + its `Default` (`config.rs:49-70`) with:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsConfig {
    /// Corpus embedding model name (e.g. "voyage-code-3").
    pub embedding: String,
    /// Reranker catalog id (see mn-embedding reranker_catalog).
    pub reranker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<PathBuf>,
    /// Voyage API key (BYOK). Resolved with flag > env > config precedence;
    /// this is the config-file fallback only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voyage_api_key: Option<String>,
    /// Voyage output dimension (Matryoshka): 256/512/1024/2048.
    #[serde(default = "default_voyage_dim")]
    pub voyage_output_dimension: u32,
    /// Voyage output dtype: "float" | "int8" | "uint8" | "binary" | "ubinary".
    #[serde(default = "default_voyage_dtype")]
    pub voyage_output_dtype: String,
    /// Directory holding a custom reranker (model.onnx + tokenizer files) when
    /// `reranker == "custom"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reranker_path: Option<PathBuf>,
}

fn default_voyage_dim() -> u32 {
    1024
}
fn default_voyage_dtype() -> String {
    "float".to_owned()
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            embedding: "voyage-code-3".into(),
            reranker: "bge-reranker-base".into(),
            cache_dir: None,
            voyage_api_key: None,
            voyage_output_dimension: default_voyage_dim(),
            voyage_output_dtype: default_voyage_dtype(),
            reranker_path: None,
        }
    }
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p mn-core config::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-core/src/config.rs
git commit -m "feat(mn-core): add Voyage + reranker fields to ModelsConfig

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 1.2: Add a flag>env>config resolver for the Voyage key + reranker id

**Files:**
- Modify: `crates/mn-core/src/config.rs` (add `resolve_voyage_api_key` / `resolve_reranker`)
- Test: `crates/mn-core/src/config.rs` inline tests

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn resolve_voyage_key_prefers_flag_then_env_then_config() {
    let cfg = ModelsConfig { voyage_api_key: Some("from-config".into()), ..Default::default() };
    let mut env = std::collections::HashMap::new();
    env.insert("VOYAGE_API_KEY".to_string(), "from-env".to_string());
    let env = FakeEnv(env); // existing test helper implementing ConfigEnv

    assert_eq!(resolve_voyage_api_key(Some("from-flag"), &cfg, &env).as_deref(), Some("from-flag"));
    assert_eq!(resolve_voyage_api_key(None, &cfg, &env).as_deref(), Some("from-env"));

    let empty = FakeEnv(std::collections::HashMap::new());
    assert_eq!(resolve_voyage_api_key(None, &cfg, &empty).as_deref(), Some("from-config"));
}
```

If `FakeEnv` is not already present in `config.rs` tests, add:

```rust
#[cfg(test)]
struct FakeEnv(std::collections::HashMap<String, String>);
#[cfg(test)]
impl ConfigEnv for FakeEnv {
    fn var(&self, name: &str) -> Option<String> { self.0.get(name).cloned() }
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p mn-core config::tests::resolve_voyage_key_prefers_flag_then_env_then_config`
Expected: FAIL — `resolve_voyage_api_key` not defined.

- [ ] **Step 3: Implement the resolvers**

Add to `config.rs` (public fns):

```rust
/// Resolve the Voyage API key with precedence flag > `VOYAGE_API_KEY` env > config.
pub fn resolve_voyage_api_key(
    flag: Option<&str>,
    cfg: &ModelsConfig,
    env: &impl ConfigEnv,
) -> Option<String> {
    flag.map(str::to_owned)
        .or_else(|| env.var("VOYAGE_API_KEY"))
        .or_else(|| cfg.voyage_api_key.clone())
        .filter(|s| !s.is_empty())
}

/// Resolve reranker id with precedence flag > `MIDNIGHT_MANUAL_RERANKER` env > config.
pub fn resolve_reranker(flag: Option<&str>, cfg: &ModelsConfig, env: &impl ConfigEnv) -> String {
    flag.map(str::to_owned)
        .or_else(|| env.var("MIDNIGHT_MANUAL_RERANKER"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cfg.reranker.clone())
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p mn-core config::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-core/src/config.rs
git commit -m "feat(mn-core): flag>env>config resolvers for Voyage key + reranker

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Phase 2 — `VoyageEmbedder` (mn-embedding)

A `reqwest`-based client for `POST https://api.voyageai.com/v1/embeddings`. Used by both the CLI (BYOK) and the server endpoint.

### Task 2.1: `VoyageEmbedder` happy path (mock HTTP)

**Files:**
- Create: `crates/mn-embedding/src/voyage.rs`
- Modify: `crates/mn-embedding/src/lib.rs` (export), `crates/mn-embedding/Cargo.toml` (ensure `reqwest`, `serde`, `serde_json` deps; add `wiremock` dev-dep)
- Test: `crates/mn-embedding/tests/voyage.rs`

- [ ] **Step 1: Add deps**

In `crates/mn-embedding/Cargo.toml` ensure under `[dependencies]`: `reqwest = { workspace = true }`, `serde = { workspace = true, features = ["derive"] }`, `serde_json = { workspace = true }`, `thiserror = { workspace = true }`; under `[dev-dependencies]`: `wiremock = { workspace = true }`, `tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }`.

- [ ] **Step 2: Write the failing test**

`crates/mn-embedding/tests/voyage.rs`:

```rust
use mn_embedding::voyage::{VoyageEmbedder, InputType};
use wiremock::matchers::{method, path, header};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn embeds_and_reports_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [{ "object": "embedding", "embedding": [0.1, 0.2, 0.3], "index": 0 }],
            "model": "voyage-code-3",
            "usage": { "total_tokens": 7 }
        })))
        .mount(&server)
        .await;

    let emb = VoyageEmbedder::new("test-key", "voyage-code-3", 1024, "float")
        .with_base_url(&server.uri());
    let out = emb.embed(vec!["hello".into()], InputType::Query).await.unwrap();
    assert_eq!(out.vectors, vec![vec![0.1_f32, 0.2, 0.3]]);
    assert_eq!(out.total_tokens, 7);
    assert_eq!(out.model, "voyage-code-3");
}
```

- [ ] **Step 3: Run it, verify it fails**

Run: `cargo test -p mn-embedding --test voyage`
Expected: FAIL — `mn_embedding::voyage` module missing.

- [ ] **Step 4: Implement `voyage.rs`**

```rust
//! VoyageAI embeddings + reranking HTTP client (raw reqwest; no official Rust SDK).
use serde::{Deserialize, Serialize};

const DEFAULT_BASE_URL: &str = "https://api.voyageai.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputType {
    Query,
    Document,
}
impl InputType {
    fn as_str(self) -> &'static str {
        match self {
            InputType::Query => "query",
            InputType::Document => "document",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VoyageError {
    #[error("voyage http error: {0}")]
    Http(String),
    #[error("voyage returned status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("voyage response decode error: {0}")]
    Decode(String),
}

#[derive(Debug, Clone)]
pub struct EmbedOutput {
    pub vectors: Vec<Vec<f32>>,
    pub total_tokens: u64,
    pub model: String,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: Vec<String>,
    input_type: &'a str,
    output_dimension: u32,
    output_dtype: &'a str,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
    index: usize,
}
#[derive(Deserialize)]
struct Usage {
    total_tokens: u64,
}
#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
    model: String,
    usage: Usage,
}

#[derive(Clone)]
pub struct VoyageEmbedder {
    client: reqwest::Client,
    api_key: String,
    model: String,
    dim: u32,
    dtype: String,
    base_url: String,
}

impl VoyageEmbedder {
    #[must_use]
    pub fn new(api_key: &str, model: &str, dim: u32, dtype: &str) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("build reqwest client"),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            dim,
            dtype: dtype.to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base: &str) -> Self {
        self.base_url = base.trim_end_matches('/').to_owned();
        self
    }

    /// Embed a batch (≤1000 texts / ≤120K tokens per Voyage limits — caller batches).
    pub async fn embed(
        &self,
        input: Vec<String>,
        input_type: InputType,
    ) -> Result<EmbedOutput, VoyageError> {
        let body = EmbedRequest {
            model: &self.model,
            input,
            input_type: input_type.as_str(),
            output_dimension: self.dim,
            output_dtype: &self.dtype,
        };
        let resp = self
            .client
            .post(format!("{}/v1/embeddings", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| VoyageError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(VoyageError::Status { status: status.as_u16(), body });
        }
        let mut parsed: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| VoyageError::Decode(e.to_string()))?;
        parsed.data.sort_by_key(|d| d.index);
        Ok(EmbedOutput {
            vectors: parsed.data.into_iter().map(|d| d.embedding).collect(),
            total_tokens: parsed.usage.total_tokens,
            model: parsed.model,
        })
    }
}
```

In `crates/mn-embedding/src/lib.rs` add: `pub mod voyage;`.

- [ ] **Step 5: Run test, verify pass**

Run: `cargo test -p mn-embedding --test voyage`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-embedding/src/voyage.rs crates/mn-embedding/src/lib.rs crates/mn-embedding/Cargo.toml crates/mn-embedding/tests/voyage.rs
git commit -m "feat(mn-embedding): add VoyageEmbedder HTTP client

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 2.2: Error + status mapping

**Files:**
- Test: `crates/mn-embedding/tests/voyage.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn maps_non_2xx_to_status_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .mount(&server)
        .await;
    let emb = VoyageEmbedder::new("k", "voyage-code-3", 1024, "float").with_base_url(&server.uri());
    let err = emb.embed(vec!["x".into()], InputType::Document).await.unwrap_err();
    match err {
        mn_embedding::voyage::VoyageError::Status { status, .. } => assert_eq!(status, 429),
        other => panic!("expected Status, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p mn-embedding --test voyage maps_non_2xx`
Expected: PASS (already implemented in 2.1 — this locks the behavior).

- [ ] **Step 3: Commit**

```bash
git add crates/mn-embedding/tests/voyage.rs
git commit -m "test(mn-embedding): lock Voyage non-2xx status mapping

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Phase 3 — Schema migration to `voyage-code-3` / 1024-dim + search filtering

Re-types the embedding column, registers + activates the new model, and teaches search to read the dimension from the resolved corpus model and to filter by it (so a partially-migrated corpus is safe).

> After this phase the corpus is "empty" of voyage chunks until re-ingest (Phase 7) — that is expected and intended (the design's accepted coverage gap on a dimension change).

### Task 3.1: Migration `0008_voyage_embedding.sql`

**Files:**
- Create: `crates/mn-store/migrations/0008_voyage_embedding.sql`
- Test: `crates/mn-store/tests/voyage_migration.rs`

- [ ] **Step 1: Write the failing integration test**

`crates/mn-store/tests/voyage_migration.rs`:

```rust
#![cfg(feature = "integration")]
mod common;

#[tokio::test]
async fn migration_registers_voyage_and_sets_1024_dim() {
    let h = common::boot().await; // runs all migrations incl. 0008
    // voyage-code-3@1 is registered with dim 1024
    let m = mn_store::entities::embedding_model::get_by_name_revision(&h.pool, "voyage-code-3", 1)
        .await
        .expect("voyage-code-3@1 registered");
    assert_eq!(m.dim, 1024);
    assert_eq!(m.provider, "voyageai");
    // chunk.embedding column is vector(1024)
    let dim: i32 = sqlx::query_scalar(
        "SELECT atttypmod FROM pg_attribute \
         WHERE attrelid = 'chunk'::regclass AND attname = 'embedding'",
    )
    .fetch_one(&h.pool)
    .await
    .expect("query embedding typmod");
    assert_eq!(dim, 1024, "pgvector stores dimension in atttypmod");
    // get_active resolves to voyage-code-3 (most-recently-created model, no active sv yet)
    let active = mn_store::entities::embedding_model::get_active(&h.pool).await.unwrap();
    assert_eq!(active.name, "voyage-code-3");
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p mn-store --features integration --test voyage_migration`
Expected: FAIL — voyage-code-3 not registered / dim 768.

- [ ] **Step 3: Write the migration**

`crates/mn-store/migrations/0008_voyage_embedding.sql`:

```sql
-- 0008 — switch the corpus embedding model to VoyageAI voyage-code-3 (1024-dim).
--
-- A vector(1024) column cannot hold the prior 768-dim vectors, so this clears
-- existing embeddings (greenfield/test data only). Chunk rows are preserved.
-- The HNSW index is bound to the column dimension and must be recreated.

-- 1. Register + (implicitly) make voyage-code-3 the newest model. get_active()
--    returns the most-recently-created model when no source_version is active.
INSERT INTO embedding_model (name, revision, dim, provider)
VALUES ('voyage-code-3', 1, 1024, 'voyageai')
ON CONFLICT (name, revision) DO NOTHING;

-- 2. Drop the HNSW index (bound to vector(768)).
DROP INDEX IF EXISTS idx_chunk_embedding;

-- 3. Clear old-dim vectors and re-type the column to vector(1024).
UPDATE chunk SET embedding = NULL WHERE embedding IS NOT NULL;
ALTER TABLE chunk ALTER COLUMN embedding TYPE vector(1024);

-- 4. Recreate the HNSW index on the new column (skips NULLs automatically).
CREATE INDEX idx_chunk_embedding ON chunk USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);
```

- [ ] **Step 4: Run test, verify pass**

Run: `cargo test -p mn-store --features integration --test voyage_migration`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-store/migrations/0008_voyage_embedding.sql crates/mn-store/tests/voyage_migration.rs
git commit -m "feat(mn-store): migration to voyage-code-3 / vector(1024)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 3.2: `CorpusModel` state + boot resolution (mutable, re-resolvable)

Replaces the immutable `cfg.corpus_model: Option<String>` with a re-resolvable `Arc<RwLock<CorpusModel>>` in `AppState`, carrying the wire id **and** the model UUID + dim (so search filters by id and validates the right dim).

**Files:**
- Create: `crates/mn-server/src/corpus_model.rs`
- Modify: `crates/mn-server/src/state.rs` (or `app.rs` where `AppState` is defined), `crates/mn-server/src/main.rs:43-49`, `crates/mn-server/src/lib.rs`/`mod` to add the module
- Test: `crates/mn-server/tests/corpus_model_resolve.rs`

- [ ] **Step 1: Write the failing integration test**

`crates/mn-server/tests/corpus_model_resolve.rs`:

```rust
#![cfg(feature = "integration")]
// Uses the mn-server test harness that exposes a pool (mirror existing
// crates/mn-server/tests/*.rs harness usage).
mod common;

#[tokio::test]
async fn resolves_voyage_corpus_model_from_db() {
    let h = common::boot().await;
    let cm = mn_server::corpus_model::resolve(&h.pool).await.expect("resolve");
    assert_eq!(cm.wire, "voyage-code-3@1");
    assert_eq!(cm.dim, 1024);
}
```

(If `crates/mn-server/tests/common` doesn't exist, mirror `crates/mn-store/tests/common/mod.rs::boot()`; the server tests already boot a pool — reuse that helper.)

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p mn-server --features integration --test corpus_model_resolve`
Expected: FAIL — `mn_server::corpus_model` missing.

- [ ] **Step 3: Implement `corpus_model.rs`**

```rust
//! The corpus's active embedding model, resolvable at boot and after each
//! ingest finalize. Held behind an RwLock in AppState so promotions take
//! effect without a restart.
use sqlx::PgPool;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct CorpusModel {
    pub wire: String, // "voyage-code-3@1"
    pub id: Uuid,
    pub dim: usize,
}

/// Resolve the active model from the DB (mirrors the prior boot logic).
pub async fn resolve(pool: &PgPool) -> anyhow::Result<CorpusModel> {
    let m = mn_store::entities::embedding_model::get_active(pool).await?;
    Ok(CorpusModel {
        wire: format!("{}@{}", m.name, m.revision),
        id: m.id,
        dim: usize::try_from(m.dim).unwrap_or(0),
    })
}

/// Shared handle stored in AppState.
pub type Shared = Arc<RwLock<CorpusModel>>;

/// Re-resolve + swap in place (called after ingest finalize).
pub async fn refresh(pool: &PgPool, shared: &Shared) {
    match resolve(pool).await {
        Ok(cm) => {
            tracing::info!(corpus_model = %cm.wire, "re-resolved corpus model");
            *shared.write().expect("corpus_model lock poisoned") = cm;
        }
        Err(e) => tracing::warn!(error = %e, "corpus model refresh failed"),
    }
}
```

Add `pub mod corpus_model;` to the server crate root (`main.rs`/`lib.rs`). Add a field to `AppState`: `pub corpus_model: corpus_model::Shared,`.

- [ ] **Step 4: Wire boot resolution in `main.rs`**

Replace the block at `main.rs:43-49` (which set `cfg.corpus_model`) with:

```rust
let corpus_model = std::sync::Arc::new(std::sync::RwLock::new(
    crate::corpus_model::resolve(&pool)
        .await
        .context("resolve active embedding model (did migration 0008 run?)")?,
));
tracing::info!(
    corpus_model = %corpus_model.read().unwrap().wire,
    "resolved active embedding model"
);
```

Pass `corpus_model.clone()` into `AppState` construction (in `app.rs`). Remove the now-dead `cfg.corpus_model` field usage (or keep the field but stop reading it — prefer removing to avoid drift; update `search.rs` in Task 3.3).

- [ ] **Step 5: Run test + build, verify pass**

Run: `cargo test -p mn-server --features integration --test corpus_model_resolve` then `cargo build -p mn-server`
Expected: PASS + clean build.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-server/src/corpus_model.rs crates/mn-server/src/state.rs crates/mn-server/src/app.rs crates/mn-server/src/main.rs crates/mn-server/tests/corpus_model_resolve.rs
git commit -m "feat(mn-server): re-resolvable CorpusModel state (wire+id+dim)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 3.3: Search reads dim from corpus model + filters by model id

**Files:**
- Modify: `crates/mn-server/src/routes/search.rs` (409 check ~274-293, dim check ~296-308, `vector_search` ~712-723, `fts_search` ~751-761)
- Test: `crates/mn-server/tests/search_model_filter.rs`

- [ ] **Step 1: Write the failing integration test**

`crates/mn-server/tests/search_model_filter.rs` (sketch — mirror existing search integration test setup):

```rust
#![cfg(feature = "integration")]
mod common;
// Seed: a source_version active on bge-base-en-v1.5@1 with one ready chunk
// (768 NULL after migration won't matter — insert a chunk on a NON-corpus model
// with a 1024 vector but a different embedding_model_id), then assert it is
// excluded from results because embedding_model_id != corpus model id.
#[tokio::test]
async fn excludes_chunks_not_on_corpus_model() {
    // ... seed via helpers; POST /v1/search with client_embedding_model = "voyage-code-3@1"
    //     and a 1024-dim query vector; assert the off-model chunk is absent.
}
```

(Keep this test minimal but real; reuse whatever seeding helper the existing `crates/mn-server/tests/search*.rs` uses. If none seeds chunks directly, add a small SQL insert helper in the test.)

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p mn-server --features integration --test search_model_filter`
Expected: FAIL — off-model chunk currently returned (no model filter yet).

- [ ] **Step 3: Update the 409 + dim checks**

In `search.rs`, replace the `cfg.corpus_model` read with a snapshot of the RwLock and use its fields. Near line 274:

```rust
let cm = state.corpus_model.read().expect("corpus_model lock poisoned").clone();
let corpus_model_id_wire = cm.wire.clone();
if req.client_embedding_model != corpus_model_id_wire {
    return error::into_response(
        CoreError::builder(ErrorCode::EmbeddingModelMismatch)
            .message(format!(
                "client_embedding_model `{}` does not match corpus model `{corpus_model_id_wire}`",
                req.client_embedding_model,
            ))
            .remediation("re-run `mnm models pull` to fetch the corpus model")
            .context("corpus_model", corpus_model_id_wire.clone())
            .context("client_model", req.client_embedding_model.clone())
            .build(),
        rid,
    );
}
```

Replace the dim check (line ~296-308) `768` literal with `cm.dim`:

```rust
for (i, q) in queries.iter().enumerate() {
    if q.vector.len() != cm.dim {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(format!(
                    "queries[{i}].vector has {} dimensions; expected {}",
                    q.vector.len(), cm.dim,
                ))
                .remediation("re-embed with the corpus model (mnm models pull)")
                .build(),
            rid,
        );
    }
}
```

Thread `cm.id` (a `Uuid`) into `vector_search` / `fts_search` calls (add a `corpus_model_id: Uuid` parameter to both fns and to their call sites).

- [ ] **Step 4: Add the model filter to both SQL builders**

In `vector_search` (after the existing `WHERE … sv.is_active = true` push, line ~719):

```rust
qb.push(
    " WHERE chunk.embedding IS NOT NULL AND chunk.status = 'ready' AND sv.is_active = true \
     AND sv.embedding_model_id = ",
);
qb.push_bind(corpus_model_id);
```

In `fts_search` (line ~759, the `… AND sv.is_active = true` push):

```rust
qb.push(") AND chunk.status = 'ready' AND sv.is_active = true AND sv.embedding_model_id = ");
qb.push_bind(corpus_model_id);
```

- [ ] **Step 5: Run the test + the existing search tests, verify pass**

Run: `cargo test -p mn-server --features integration --test search_model_filter` then `cargo test -p mn-server --features integration` (full server integration suite).
Expected: PASS (no regressions in existing search tests).

- [ ] **Step 6: Commit**

```bash
git add crates/mn-server/src/routes/search.rs crates/mn-server/tests/search_model_filter.rs
git commit -m "feat(mn-server): search dim from corpus model + filter by model id

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 3.4: Re-resolve corpus model after ingest finalize

**Files:**
- Modify: `crates/mn-server/src/routes/admin_ingest.rs` (the `finalize_run` handler, ~466-533)
- Test: `crates/mn-server/tests/finalize_reresolves_model.rs`

- [ ] **Step 1: Write the failing integration test**

```rust
#![cfg(feature = "integration")]
mod common;
// Boot, register a SECOND model, ingest+finalize a source_version on it, then
// assert AppState's corpus_model.wire flips to the new model. (Drive through the
// HTTP finalize handler or call the handler's helper; mirror existing ingest tests.)
#[tokio::test]
async fn finalize_updates_corpus_model() { /* ... */ }
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p mn-server --features integration --test finalize_reresolves_model`
Expected: FAIL — corpus_model not updated post-finalize.

- [ ] **Step 3: Hook the refresh**

In `finalize_run`, after a successful `source_version::finalize(...)`:

```rust
Ok((promoted, demoted)) => {
    crate::corpus_model::refresh(&state.pool, &state.corpus_model).await;
    Json(FinalizeResult { source_version_id: run_id, revision: promoted, is_active: true, demoted_revision: demoted }).into_response()
}
```

- [ ] **Step 4: Run test, verify pass**

Run: `cargo test -p mn-server --features integration --test finalize_reresolves_model`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/routes/admin_ingest.rs crates/mn-server/tests/finalize_reresolves_model.rs
git commit -m "feat(mn-server): re-resolve corpus model after ingest finalize

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Phase 4 — Token-limit subsystem + `POST /v1/embeddings`

In-memory bucketed token accounting (rolling minute-ring + hour-buckets), DB-backed overrides + restart snapshot, and the embedding endpoint that calls Voyage server-side.

### Task 4.1: Migration `0009_token_limits.sql`

**Files:**
- Create: `crates/mn-store/migrations/0009_token_limits.sql`
- Test: `crates/mn-store/tests/token_limits_schema.rs`

- [ ] **Step 1: Write the failing integration test**

```rust
#![cfg(feature = "integration")]
mod common;
#[tokio::test]
async fn token_limit_tables_exist() {
    let h = common::boot().await;
    sqlx::query("INSERT INTO token_limit_override (subject_kind, subject, hourly, daily, expires_at, created_by) \
                 VALUES ('user', 'u1', 100, 1000, now() + interval '1 hour', 'admin')")
        .execute(&h.pool).await.expect("insert override");
    sqlx::query("INSERT INTO token_usage_snapshot (subject_kind, subject, hour_epoch, tokens) \
                 VALUES ('ip', '203.0.113.1', 480000, 42)")
        .execute(&h.pool).await.expect("insert snapshot");
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p mn-store --features integration --test token_limits_schema`
Expected: FAIL — tables don't exist.

- [ ] **Step 3: Write the migration**

`crates/mn-store/migrations/0009_token_limits.sql`:

```sql
-- 0009 — embedding token-limit overrides + restart-durability snapshot.

CREATE TABLE token_limit_override (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_kind text NOT NULL CHECK (subject_kind IN ('cidr','user')),
    subject      text NOT NULL,   -- CIDR text (network-normalised) or user id
    hourly       bigint NOT NULL CHECK (hourly >= 0),
    daily        bigint NOT NULL CHECK (daily  >= 0),
    expires_at   timestamptz NOT NULL,
    note         text,
    created_by   text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX idx_token_limit_override_active ON token_limit_override (expires_at);

-- In-memory usage is the source of truth on the hot path; this is only a
-- periodic snapshot so a restart can reload ~last 24h of hourly buckets.
CREATE TABLE token_usage_snapshot (
    subject_kind text   NOT NULL CHECK (subject_kind IN ('ip','user')),
    subject      text   NOT NULL,
    hour_epoch   bigint NOT NULL,            -- floor(unix_secs / 3600)
    tokens       bigint NOT NULL CHECK (tokens >= 0),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (subject_kind, subject, hour_epoch)
);
CREATE INDEX idx_token_usage_snapshot_hour ON token_usage_snapshot (hour_epoch);
```

- [ ] **Step 4: Run test, verify pass** → `cargo test -p mn-store --features integration --test token_limits_schema`
- [ ] **Step 5: Commit**

```bash
git add crates/mn-store/migrations/0009_token_limits.sql crates/mn-store/tests/token_limits_schema.rs
git commit -m "feat(mn-store): token_limit_override + token_usage_snapshot tables

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 4.2: `token_limit_override` entity + types

**Files:**
- Create: `crates/mn-store/src/entities/token_limit_override.rs` (+ `mod.rs` export)
- Modify: `crates/mn-core/src/types.rs` (add `TokenLimitOverride`)
- Test: `crates/mn-store/tests/token_limit_override.rs`

Mirror `rate_limit_override.rs` exactly (same `insert/list_active/get_by_id/update/delete` shape). Differences: `subject_kind`+`subject` instead of `cidr`; `hourly`+`daily` instead of `limit_rps`; for `cidr` subjects normalise via `network($2::inet)::text`.

- [ ] **Step 1: Add the type to `mn-core/src/types.rs`**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenLimitOverride {
    pub id: Uuid,
    pub subject_kind: String, // "cidr" | "user"
    pub subject: String,
    pub hourly: i64,
    pub daily: i64,
    pub expires_at: OffsetDateTime,
    pub note: Option<String>,
    pub created_by: String,
    pub created_at: OffsetDateTime,
}
```

- [ ] **Step 2: Write the failing integration test**

```rust
#![cfg(feature = "integration")]
mod common;
use mn_store::entities::token_limit_override as tlo;
use time::{Duration, OffsetDateTime};

#[tokio::test]
async fn insert_list_update_delete_roundtrip() {
    let h = common::boot().await;
    let exp = OffsetDateTime::now_utc() + Duration::hours(2);
    let row = tlo::insert(&h.pool, "user", "alice", 4000, 40000, exp, Some("vip"), "admin").await.unwrap();
    assert_eq!(row.hourly, 4000);
    let active = tlo::list_active(&h.pool).await.unwrap();
    assert!(active.iter().any(|r| r.id == row.id));
    let cidr = tlo::insert(&h.pool, "cidr", "203.0.113.0/24", 9, 90, exp, None, "admin").await.unwrap();
    assert_eq!(cidr.subject, "203.0.113.0/24"); // network-normalised
    let _ = tlo::delete(&h.pool, row.id).await.unwrap();
}
```

- [ ] **Step 3: Run it, verify it fails** → `cargo test -p mn-store --features integration --test token_limit_override` → FAIL (module missing).

- [ ] **Step 4: Implement the entity** (mirror `rate_limit_override.rs`; key SQL below)

```rust
//! Per-subject (CIDR or user) embedding token-limit overrides.
use crate::error::Result;
use mn_core::types::TokenLimitOverride;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

const COLS: &str = "id, subject_kind, subject, hourly, daily, expires_at, note, created_by, created_at";

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid, subject_kind: String, subject: String, hourly: i64, daily: i64,
    expires_at: OffsetDateTime, note: Option<String>, created_by: String, created_at: OffsetDateTime,
}
impl From<Row> for TokenLimitOverride {
    fn from(r: Row) -> Self {
        Self { id: r.id, subject_kind: r.subject_kind, subject: r.subject, hourly: r.hourly,
                daily: r.daily, expires_at: r.expires_at, note: r.note, created_by: r.created_by, created_at: r.created_at }
    }
}

pub async fn insert(pool: &PgPool, subject_kind: &str, subject: &str, hourly: i64, daily: i64,
    expires_at: OffsetDateTime, note: Option<&str>, created_by: &str) -> Result<TokenLimitOverride> {
    // For CIDR subjects, normalise the network so longest-prefix matching is stable.
    let normalised_sql = if subject_kind == "cidr" { "network($2::inet)::text" } else { "$2" };
    let sql = format!(
        "INSERT INTO token_limit_override (subject_kind, subject, hourly, daily, expires_at, note, created_by) \
         VALUES ($1, {normalised_sql}, $3, $4, $5, $6, $7) RETURNING {COLS}");
    let row: Row = sqlx::query_as(&sql)
        .bind(subject_kind).bind(subject).bind(hourly).bind(daily)
        .bind(expires_at).bind(note).bind(created_by)
        .fetch_one(pool).await?;
    Ok(row.into())
}

pub async fn list_active(pool: &PgPool) -> Result<Vec<TokenLimitOverride>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM token_limit_override WHERE expires_at > now() ORDER BY created_at DESC"))
        .fetch_all(pool).await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn get_by_id(pool: &PgPool, id: Uuid) -> Result<TokenLimitOverride> {
    let row: Row = sqlx::query_as(&format!("SELECT {COLS} FROM token_limit_override WHERE id = $1"))
        .bind(id).fetch_one(pool).await?;
    Ok(row.into())
}

#[derive(Default)]
pub struct Patch { pub expires_at: Option<OffsetDateTime>, pub hourly: Option<i64>, pub daily: Option<i64>, pub note: Option<String> }

pub async fn update(pool: &PgPool, id: Uuid, patch: Patch) -> Result<TokenLimitOverride> {
    let row: Row = sqlx::query_as(&format!(
        "UPDATE token_limit_override SET expires_at = COALESCE($2, expires_at), \
         hourly = COALESCE($3, hourly), daily = COALESCE($4, daily), note = COALESCE($5, note) \
         WHERE id = $1 RETURNING {COLS}"))
        .bind(id).bind(patch.expires_at).bind(patch.hourly).bind(patch.daily).bind(patch.note)
        .fetch_one(pool).await?;
    Ok(row.into())
}

pub async fn delete(pool: &PgPool, id: Uuid) -> Result<TokenLimitOverride> {
    let row: Row = sqlx::query_as(&format!("DELETE FROM token_limit_override WHERE id = $1 RETURNING {COLS}"))
        .bind(id).fetch_one(pool).await?;
    Ok(row.into())
}
```

Add `pub mod token_limit_override;` to `crates/mn-store/src/entities/mod.rs`.

- [ ] **Step 5: Run test, verify pass** → `cargo test -p mn-store --features integration --test token_limit_override`
- [ ] **Step 6: Commit**

```bash
git add crates/mn-store/src/entities/token_limit_override.rs crates/mn-store/src/entities/mod.rs crates/mn-core/src/types.rs crates/mn-store/tests/token_limit_override.rs
git commit -m "feat(mn-store): token_limit_override entity

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 4.3: Server config env fields (Voyage + token limits)

**Files:**
- Modify: `crates/mn-server/src/config.rs` (struct + `from_env`)
- Test: `crates/mn-server/src/config.rs` inline tests (mirror existing rate-limit env tests)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn token_limit_defaults() {
    // with no env set, defaults match the spec
    let c = ServerConfig::from_env_for_test(&[]); // mirror existing test ctor if present
    assert_eq!(c.token_limit_anon_hourly, 2_000);
    assert_eq!(c.token_limit_anon_daily, 20_000);
    assert_eq!(c.token_limit_uplift_hourly, 4_000);
    assert_eq!(c.token_limit_admin_daily, 100_000_000);
    assert_eq!(c.token_snapshot_secs, 300);
}
```

(If `ServerConfig` is built only via `from_env()` reading the process env, follow the existing test style in `config.rs` — set env vars with a guard, or add a small internal constructor. Match whatever pattern the rate-limit tests use.)

- [ ] **Step 2: Run it, verify it fails** → FAIL (fields missing).

- [ ] **Step 3: Add fields + parsing**

Add to `ServerConfig`:

```rust
pub voyage_api_key: Option<String>,
pub voyage_model: String,            // "voyage-code-3"
pub voyage_output_dimension: u32,    // 1024
pub voyage_output_dtype: String,     // "float"
pub token_limit_anon_hourly: u64,
pub token_limit_anon_daily: u64,
pub token_limit_uplift_hourly: u64,
pub token_limit_uplift_daily: u64,
pub token_limit_admin_hourly: u64,
pub token_limit_admin_daily: u64,
pub token_snapshot_secs: u64,
```

In `from_env()` (mirror the existing `map_or(DEFAULT, |v| v.max(MIN))` idiom):

```rust
let voyage_api_key = env::var("VOYAGE_API_KEY").ok().filter(|s| !s.is_empty());
let voyage_model = env::var("MIDNIGHT_MANUAL_VOYAGE_MODEL").unwrap_or_else(|_| "voyage-code-3".into());
let voyage_output_dimension = env::var("MIDNIGHT_MANUAL_VOYAGE_DIM")
    .ok().and_then(|s| s.parse().ok()).map_or(1024, |v: u32| v);
let voyage_output_dtype = env::var("MIDNIGHT_MANUAL_VOYAGE_DTYPE").unwrap_or_else(|_| "float".into());

let tl = |name: &str, default: u64| env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default);
let token_limit_anon_hourly   = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_ANON_HOURLY",   2_000);
let token_limit_anon_daily    = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_ANON_DAILY",    20_000);
let token_limit_uplift_hourly = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_UPLIFT_HOURLY", 4_000);
let token_limit_uplift_daily  = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_UPLIFT_DAILY",  40_000);
let token_limit_admin_hourly  = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_ADMIN_HOURLY",  500_000);
let token_limit_admin_daily   = tl("MIDNIGHT_MANUAL_TOKEN_LIMIT_ADMIN_DAILY",   100_000_000);
let token_snapshot_secs       = env::var("MIDNIGHT_MANUAL_TOKEN_SNAPSHOT_SECS").ok().and_then(|s| s.parse().ok()).map_or(300, |v: u64| v.max(1));
```

- [ ] **Step 4: Run tests, verify pass** → `cargo test -p mn-server config`
- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/config.rs
git commit -m "feat(mn-server): Voyage + token-limit config env fields

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 4.4: `TokenUsageLimiter` — rolling buckets (pure logic, no DB)

**Files:**
- Create: `crates/mn-server/src/tokenlimit.rs` (+ `mod`/`lib` export)
- Test: `crates/mn-server/src/tokenlimit.rs` inline `#[cfg(test)]`

All methods take an explicit `now_secs: i64` so tests are deterministic.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn lim() -> Limits { Limits { hourly: 100, daily: 1000 } }
    fn subj() -> TokenSubject { TokenSubject::User("u1".into()) }

    #[test]
    fn charges_and_reports_remaining() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        l.charge(&subj(), 30, now);
        let info = l.snapshot_for(&subj(), lim(), now);
        assert_eq!(info.hour.remaining, 70);
        assert_eq!(info.day.remaining, 970);
    }

    #[test]
    fn rejects_when_estimate_exceeds_hour() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let now = 1_000_000_000;
        l.charge(&subj(), 90, now);
        let rej = l.check(&subj(), lim(), 20, now);
        assert!(matches!(rej, Err(Reject { window: Window::Hour, .. })));
    }

    #[test]
    fn hourly_usage_ages_out_after_60_minutes() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let t0 = 1_000_000_000;
        l.charge(&subj(), 90, t0);
        // 61 minutes later the minute buckets have aged out of the hour window
        let later = t0 + 61 * 60;
        assert!(l.check(&subj(), lim(), 90, later).is_ok());
        // but daily (hour buckets, 24h) still counts it
        let info = l.snapshot_for(&subj(), lim(), later);
        assert_eq!(info.day.remaining, 1000 - 90);
    }

    #[test]
    fn daily_usage_ages_out_after_24_hours() {
        let l = TokenUsageLimiter::new(d(), d(), d());
        let t0 = 1_000_000_000;
        l.charge(&subj(), 500, t0);
        let later = t0 + 25 * 3600;
        let info = l.snapshot_for(&subj(), lim(), later);
        assert_eq!(info.day.remaining, 1000);
    }

    fn d() -> Limits { Limits { hourly: 100, daily: 1000 } }
}
```

- [ ] **Step 2: Run them, verify they fail** → `cargo test -p mn-server tokenlimit` → FAIL (module missing).

- [ ] **Step 3: Implement `tokenlimit.rs`**

```rust
//! In-memory, rolling-window embedding token accounting.
//! Hourly = rolling 60 min via per-minute buckets; daily = rolling 24h via
//! per-hour buckets. Both checked in-memory (no DB on the hot path). A separate
//! snapshot job persists hour buckets for restart durability (see jobs/).
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, RwLock};

#[derive(Debug, Clone, Copy)]
pub struct Limits { pub hourly: u64, pub daily: u64 }

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TokenSubject { Ip(String), User(String) }
impl TokenSubject {
    fn kind(&self) -> &'static str { match self { Self::Ip(_) => "ip", Self::User(_) => "user" } }
    fn value(&self) -> &str { match self { Self::Ip(v) | Self::User(v) => v } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window { Hour, Day }

#[derive(Debug, Clone, Copy)]
pub struct WindowInfo { pub limit: u64, pub remaining: u64, pub reset_at_secs: i64 }
#[derive(Debug, Clone, Copy)]
pub struct RateInfo { pub hour: WindowInfo, pub day: WindowInfo }
#[derive(Debug, Clone, Copy)]
pub struct Reject { pub window: Window, pub limit: u64, pub reset_at_secs: i64 }

#[derive(Default)]
struct SubjectUsage {
    minutes: BTreeMap<i64, u64>, // unix-minute -> tokens
    hours: BTreeMap<i64, u64>,   // unix-hour   -> tokens
    last_seen_secs: i64,
}
impl SubjectUsage {
    fn prune(&mut self, now: i64) {
        let min_floor = now / 60 - 59;     // keep last 60 minutes
        let hr_floor = now / 3600 - 23;    // keep last 24 hours
        self.minutes.retain(|&m, _| m >= min_floor);
        self.hours.retain(|&hh, _| hh >= hr_floor);
    }
    fn hour_used(&self, now: i64) -> u64 {
        let floor = now / 60 - 59;
        self.minutes.range(floor..).map(|(_, v)| *v).sum()
    }
    fn day_used(&self, now: i64) -> u64 {
        let floor = now / 3600 - 23;
        self.hours.range(floor..).map(|(_, v)| *v).sum()
    }
    fn hour_reset(&self, now: i64) -> i64 {
        let floor = now / 60 - 59;
        self.minutes.range(floor..).next().map_or(now, |(&m, _)| (m + 60) * 60)
    }
    fn day_reset(&self, now: i64) -> i64 {
        let floor = now / 3600 - 23;
        self.hours.range(floor..).next().map_or(now, |(&hh, _)| (hh + 24) * 3600)
    }
}

pub struct TokenUsageLimiter {
    usage: Mutex<HashMap<TokenSubject, SubjectUsage>>,
    overrides: RwLock<Vec<crate::tokenlimit_override::Parsed>>, // populated in Task 4.5
    anon: Limits, uplift: Limits, admin: Limits,
}

impl TokenUsageLimiter {
    #[must_use]
    pub fn new(anon: Limits, uplift: Limits, admin: Limits) -> Self {
        Self { usage: Mutex::new(HashMap::new()), overrides: RwLock::new(Vec::new()), anon, uplift, admin }
    }

    /// Returns Ok if `estimate` more tokens fit within both windows; else Reject.
    pub fn check(&self, subject: &TokenSubject, limits: Limits, estimate: u64, now: i64) -> Result<(), Reject> {
        let mut map = self.usage.lock().expect("usage lock");
        let u = map.entry(subject.clone()).or_default();
        u.prune(now);
        if u.hour_used(now) + estimate > limits.hourly {
            return Err(Reject { window: Window::Hour, limit: limits.hourly, reset_at_secs: u.hour_reset(now) });
        }
        if u.day_used(now) + estimate > limits.daily {
            return Err(Reject { window: Window::Day, limit: limits.daily, reset_at_secs: u.day_reset(now) });
        }
        Ok(())
    }

    pub fn charge(&self, subject: &TokenSubject, tokens: u64, now: i64) {
        let mut map = self.usage.lock().expect("usage lock");
        let u = map.entry(subject.clone()).or_default();
        u.prune(now);
        *u.minutes.entry(now / 60).or_default() += tokens;
        *u.hours.entry(now / 3600).or_default() += tokens;
        u.last_seen_secs = now;
    }

    #[must_use]
    pub fn snapshot_for(&self, subject: &TokenSubject, limits: Limits, now: i64) -> RateInfo {
        let mut map = self.usage.lock().expect("usage lock");
        let u = map.entry(subject.clone()).or_default();
        u.prune(now);
        let hu = u.hour_used(now);
        let du = u.day_used(now);
        RateInfo {
            hour: WindowInfo { limit: limits.hourly, remaining: limits.hourly.saturating_sub(hu), reset_at_secs: u.hour_reset(now) },
            day:  WindowInfo { limit: limits.daily,  remaining: limits.daily.saturating_sub(du),  reset_at_secs: u.day_reset(now) },
        }
    }
}
```

Add `pub mod tokenlimit;` (and the `tokenlimit_override` module from Task 4.5) to the server crate root.

- [ ] **Step 4: Run tests, verify pass** → `cargo test -p mn-server tokenlimit`
- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/tokenlimit.rs crates/mn-server/src/main.rs
git commit -m "feat(mn-server): bucketed in-memory TokenUsageLimiter

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 4.5: Subject/tier resolution + override cache

**Files:**
- Create: `crates/mn-server/src/tokenlimit_override.rs` (parse/match — mirror `ratelimit.rs` `ParsedOverride`/`match_override`)
- Modify: `crates/mn-server/src/tokenlimit.rs` (add `resolve` + `refresh_overrides_now`)
- Test: `crates/mn-server/src/tokenlimit.rs` inline tests

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn resolve_picks_tier_limits_and_user_override() {
    let l = TokenUsageLimiter::new(Limits{hourly:2000,daily:20000}, Limits{hourly:4000,daily:40000}, Limits{hourly:500_000,daily:100_000_000});
    // anonymous IP → anon limits
    let (s, _t, lim) = l.resolve("203.0.113.9", None);
    assert!(matches!(s, TokenSubject::Ip(_)));
    assert_eq!(lim.hourly, 2000);
    // a user override beats the tier default (after manual insert into the cache)
    l.set_overrides_for_test(vec![crate::tokenlimit_override::Parsed::user("alice", 9999, 99999)]);
    let auth = test_admin_ctx("alice");
    let (_s, _t, lim) = l.resolve("203.0.113.9", Some(&auth));
    assert_eq!(lim.hourly, 9999);
}
```

- [ ] **Step 2: Run it, verify it fails** → FAIL (`resolve` not defined).

- [ ] **Step 3: Implement override parsing + resolve**

`tokenlimit_override.rs` (mirror `ratelimit.rs::ParsedOverride` + `match_override` for the CIDR case; add an exact `user` case):

```rust
use std::net::IpAddr;

#[derive(Debug, Clone)]
pub enum Parsed {
    Cidr { net: IpAddr, prefix: u8, raw: String, hourly: u64, daily: u64, created_at: time::OffsetDateTime },
    User { id: String, hourly: u64, daily: u64 },
}
impl Parsed {
    #[must_use] pub fn user(id: &str, hourly: u64, daily: u64) -> Self { Self::User { id: id.into(), hourly, daily } }
}

#[must_use]
pub fn match_cidr(overrides: &[Parsed], ip: IpAddr) -> Option<(u64, u64)> {
    overrides.iter().filter_map(|o| match o {
        Parsed::Cidr { net, prefix, hourly, daily, created_at, .. } if ip_in(*net, *prefix, ip) => Some((*prefix, *created_at, *hourly, *daily)),
        _ => None,
    })
    .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)))
    .map(|(_, _, h, d)| (h, d))
}
#[must_use]
pub fn match_user<'a>(overrides: &'a [Parsed], id: &str) -> Option<(u64, u64)> {
    overrides.iter().find_map(|o| match o { Parsed::User { id: oid, hourly, daily } if oid == id => Some((*hourly, *daily)), _ => None })
}
// ip_in: copy the helper from ratelimit.rs (network prefix containment).
```

Add to `TokenUsageLimiter` in `tokenlimit.rs`:

```rust
pub enum TokenTier { Anonymous, ReadUplift, Admin }

impl TokenUsageLimiter {
    /// Resolve subject + tier + effective limits (override > tier default).
    /// `AuthContext` is the same type the rate-limit middleware extracts — import
    /// it from wherever `middleware/rate_limit.rs` imports it.
    pub fn resolve(&self, client_ip: &str, auth: Option<&AuthContext>) -> (TokenSubject, TokenTier, Limits) {
        let ov = self.overrides.read().expect("ov lock");
        if let Some(ctx) = auth {
            let (subject, tier, base) = match ctx.tier {
                mn_auth::Tier::Admin => (TokenSubject::User(ctx.sub.clone()), TokenTier::Admin, self.admin),
                mn_auth::Tier::ReadUplift => (TokenSubject::User(ctx.sub.clone()), TokenTier::ReadUplift, self.uplift),
            };
            if let Some((h, d)) = crate::tokenlimit_override::match_user(&ov, &ctx.sub) {
                return (subject, tier, Limits { hourly: h, daily: d });
            }
            return (subject, tier, base);
        }
        if let Ok(ip) = client_ip.parse() {
            if let Some((h, d)) = crate::tokenlimit_override::match_cidr(&ov, ip) {
                return (TokenSubject::Ip(client_ip.to_owned()), TokenTier::Anonymous, Limits { hourly: h, daily: d });
            }
        }
        (TokenSubject::Ip(client_ip.to_owned()), TokenTier::Anonymous, self.anon)
    }

    pub async fn refresh_overrides_now(&self, pool: &sqlx::PgPool) -> Result<usize, mn_store::error::StoreError> {
        let rows = mn_store::entities::token_limit_override::list_active(pool).await?;
        let parsed: Vec<_> = rows.into_iter().filter_map(crate::tokenlimit_override::parse_row).collect();
        let n = parsed.len();
        *self.overrides.write().expect("ov lock") = parsed;
        Ok(n)
    }

    #[cfg(test)]
    pub fn set_overrides_for_test(&self, v: Vec<crate::tokenlimit_override::Parsed>) { *self.overrides.write().unwrap() = v; }
}
```

(Use the project's real `AuthContext` import path — see `middleware/rate_limit.rs` imports; the placeholder `mn_server_auth_ctx` above is illustrative. Add a `parse_row(TokenLimitOverride) -> Option<Parsed>` to `tokenlimit_override.rs` mirroring `ratelimit::parse_override`.)

- [ ] **Step 4: Run tests, verify pass** → `cargo test -p mn-server tokenlimit`
- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/tokenlimit.rs crates/mn-server/src/tokenlimit_override.rs crates/mn-server/src/main.rs
git commit -m "feat(mn-server): token-limit subject/tier resolution + override cache

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 4.6: `POST /v1/embeddings` handler

**Files:**
- Create: `crates/mn-server/src/routes/embeddings.rs`
- Modify: `crates/mn-server/src/app.rs` (merge router), `AppState` (add `Arc<TokenUsageLimiter>` + optional `Arc<VoyageEmbedder>`)
- Test: `crates/mn-server/tests/embeddings_endpoint.rs`

- [ ] **Step 1: Write the failing integration test (mock Voyage)**

```rust
#![cfg(feature = "integration")]
mod common;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn embeds_via_voyage_and_charges_tokens() {
    let voyage = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data":[{"embedding": vec![0.0_f32; 1024], "index":0}], "model":"voyage-code-3", "usage":{"total_tokens": 5}})))
        .mount(&voyage).await;
    // boot the app with VoyageEmbedder pointed at `voyage.uri()` and anon limit 2000/hr
    let app = common::boot_app_with_voyage(&voyage.uri()).await;
    let resp = app.post("/v1/embeddings").json(&serde_json::json!({"input":["hi"], "input_type":"query"})).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await;
    assert_eq!(body["usage"]["total_tokens"], 5);
    assert_eq!(body["model"], "voyage-code-3@1");
    assert_eq!(body["rate"]["hour"]["remaining"], 2000 - 5);
}
```

(Use the project's existing server-test client style — e.g. `axum_test::TestServer` or a `reqwest` client against a bound `tokio` task; mirror existing `crates/mn-server/tests/*.rs`.)

- [ ] **Step 2: Run it, verify it fails** → FAIL (route missing).

- [ ] **Step 3: Implement the handler**

```rust
//! POST /v1/embeddings — server-side Voyage embedding with tiered token limits.
use axum::{extract::State, response::{IntoResponse, Response}, Extension, Json, Router, routing::post};
use mn_core::error::{CoreError, ErrorCode};
use mn_embedding::voyage::InputType;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Deserialize)]
struct EmbeddingsRequest {
    #[serde(default)] input: Vec<String>,
    #[serde(default = "default_input_type")] input_type: String, // "query" | "document"
    #[serde(default)] model: Option<String>,
}
fn default_input_type() -> String { "query".into() }

#[derive(Serialize)]
struct WindowOut { limit: u64, remaining: u64, reset_at: String }
#[derive(Serialize)]
struct RateOut { hour: WindowOut, day: WindowOut }
#[derive(Serialize)]
struct EmbeddingsResponse { model: String, embeddings: Vec<Vec<f32>>, usage: UsageOut, rate: RateOut }
#[derive(Serialize)]
struct UsageOut { total_tokens: u64 }

pub fn router() -> Router<crate::app::AppState> {
    Router::new().route("/v1/embeddings", post(embeddings))
}

async fn embeddings(
    State(state): State<crate::app::AppState>,
    Extension(req_id): Extension<crate::RequestId>,
    headers: axum::http::HeaderMap,         // must precede the body-consuming Json extractor
    auth: Option<Extension<AuthContext>>,   // same AuthContext type as the rate-limit middleware
    Json(req): Json<EmbeddingsRequest>,
) -> Response {
    let rid = req_id.as_str();
    let Some(voyage) = state.voyage.clone() else {
        return crate::error::service_unavailable("server embedding is not configured (no VOYAGE_API_KEY)", rid);
    };
    if req.input.is_empty() {
        return crate::error::into_response(CoreError::builder(ErrorCode::InvalidRequest).message("input must be non-empty").build(), rid);
    }
    // Voyage per-request caps → 413.
    if req.input.len() > 1000 {
        return crate::error::payload_too_large("input exceeds 1000 texts; batch client-side", rid);
    }
    let cm = state.corpus_model.read().expect("corpus_model lock").clone();
    if let Some(m) = &req.model {
        if m != &cm.wire {
            return crate::error::into_response(
                CoreError::builder(ErrorCode::EmbeddingModelMismatch)
                    .message(format!("model `{m}` does not match corpus model `{}`", cm.wire))
                    .build(),
                rid,
            );
        }
    }

    let client_ip = crate::middleware::rate_limit::client_ip(&headers, &state.cfg.rate_limit_client_ip_header);
    let auth_ctx = auth.as_ref().map(|Extension(c)| c.clone());
    let (subject, _tier, limits) = state.token_limiter.resolve(&client_ip, auth_ctx.as_ref());
    let now = OffsetDateTime::now_utc().unix_timestamp();

    // Best-effort pre-count; falls back to gate-on-remaining (estimate 0).
    let estimate = crate::tokenlimit::count_tokens_best_effort(&req.input, &state.cache_dir).unwrap_or(0) as u64;
    if let Err(rej) = state.token_limiter.check(&subject, limits, estimate, now) {
        return token_limit_429(rej, rid);
    }

    let it = if req.input_type == "document" { InputType::Document } else { InputType::Query };
    let out = match voyage.embed(req.input.clone(), it).await {
        Ok(o) => o,
        Err(e) => return crate::error::bad_gateway(&format!("voyage embedding failed: {e}"), rid),
    };
    state.token_limiter.charge(&subject, out.total_tokens, now);
    let info = state.token_limiter.snapshot_for(&subject, limits, now);

    let body = EmbeddingsResponse {
        model: cm.wire.clone(),
        embeddings: out.vectors,
        usage: UsageOut { total_tokens: out.total_tokens },
        rate: RateOut {
            hour: WindowOut { limit: info.hour.limit, remaining: info.hour.remaining, reset_at: iso(info.hour.reset_at_secs) },
            day:  WindowOut { limit: info.day.limit,  remaining: info.day.remaining,  reset_at: iso(info.day.reset_at_secs) },
        },
    };
    Json(body).into_response()
}
```

Implement helpers: `iso(secs)` (format unix → RFC3339), `token_limit_429(rej, rid)` (build a 429 `CoreError` with `window`/`limit`/`reset_at` context + `Retry-After`/`x-tokenlimit-*` headers, mirroring `middleware/rate_limit.rs` header-setting), `crate::error::bad_gateway` (502) + `crate::error::payload_too_large` (413) (mirror the existing `crate::error::service_unavailable` constructor), and `tokenlimit::count_tokens_best_effort(inputs, cache_dir) -> Option<usize>` (load `tokenizer.json` for `voyage-code-3` via `tokenizers::Tokenizer::from_file` if present in cache, else `None`). Make `client_ip(headers, header_name)` `pub(crate)` in `middleware/rate_limit.rs` so the handler can reuse it.

- [ ] **Step 4: Wire the router** in `app.rs`: `.merge(crate::routes::embeddings::router())`. Add `voyage: Option<Arc<VoyageEmbedder>>`, `token_limiter: Arc<TokenUsageLimiter>`, `cache_dir: PathBuf` to `AppState` (constructed in `main.rs` from config: build `VoyageEmbedder` when `cfg.voyage_api_key` is set).

- [ ] **Step 5: Run test, verify pass** → `cargo test -p mn-server --features integration --test embeddings_endpoint`
- [ ] **Step 6: Commit**

```bash
git add crates/mn-server/src/routes/embeddings.rs crates/mn-server/src/app.rs crates/mn-server/src/main.rs crates/mn-server/tests/embeddings_endpoint.rs
git commit -m "feat(mn-server): POST /v1/embeddings with tiered token limits

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 4.7: 429 path + Voyage-not-configured + 413 tests

**Files:** Test: `crates/mn-server/tests/embeddings_endpoint.rs`

- [ ] **Step 1: Write tests** for: (a) anon over hourly cap → 429 with `error == "token_limit_exceeded"`, `window == "hour"`, `Retry-After` header present; (b) no `VOYAGE_API_KEY` configured → 503; (c) >1000 inputs → 413. (Set the anon limit low via env for the over-cap test, or charge first.)
- [ ] **Step 2: Run, verify fail** → adjust handler until green. Run: `cargo test -p mn-server --features integration --test embeddings_endpoint`
- [ ] **Step 3: Commit**

```bash
git add crates/mn-server/tests/embeddings_endpoint.rs crates/mn-server/src/routes/embeddings.rs
git commit -m "test(mn-server): /v1/embeddings 429/503/413 paths

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 4.8: `/v1/admin/tokenlimits` CRUD + override refresh task + snapshot job + boot wiring

**Files:**
- Create: `crates/mn-server/src/routes/admin_tokenlimits.rs`, `crates/mn-server/src/jobs/token_usage_snapshot.rs`
- Modify: `crates/mn-server/src/app.rs` (merge), `crates/mn-server/src/main.rs` (spawn refresh + snapshot, initial snapshot load)
- Test: `crates/mn-server/tests/admin_tokenlimits.rs`

- [ ] **Step 1: Write the failing integration test** for the admin CRUD (mirror `crates/mn-server/tests` rate-limit admin test): POST creates, GET lists, PATCH extends, DELETE removes; non-admin → 403; after POST, `refresh_overrides_now` makes the override take effect in `resolve`.

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement `admin_tokenlimits.rs`** mirroring `admin_ratelimits.rs` (`router()` with `post(create).get(list)` on `/v1/admin/tokenlimits` and `patch(update).delete(remove)` on `/v1/admin/tokenlimits/:id`; reuse `admin_reject` + `sub_of`). Request body: `{ subject_kind, subject, hourly, daily, expires_at, note? }`; call `token_limit_override::insert/list_active/update/delete`; after a mutating call, `state.token_limiter.refresh_overrides_now(&state.pool).await`.

- [ ] **Step 4: Implement the snapshot job** `jobs/token_usage_snapshot.rs` mirroring the `jobs/source_retention.rs` spawn pattern:

```rust
pub fn spawn(pool: sqlx::PgPool, limiter: std::sync::Arc<crate::tokenlimit::TokenUsageLimiter>, secs: u64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(secs.max(1)));
        tick.tick().await;
        loop {
            tick.tick().await;
            let now = time::OffsetDateTime::now_utc().unix_timestamp();
            if let Err(e) = limiter.snapshot_to_db(&pool, now).await { tracing::warn!(error=%e, "token usage snapshot failed"); }
        }
    })
}
```

Add `snapshot_to_db(pool, now)` to `TokenUsageLimiter`: upsert each subject's in-window hour buckets into `token_usage_snapshot` (`INSERT … ON CONFLICT (subject_kind, subject, hour_epoch) DO UPDATE SET tokens = EXCLUDED.tokens, updated_at = now()`), evict subjects with `last_seen_secs < now - 86400`, and `DELETE FROM token_usage_snapshot WHERE hour_epoch < now/3600 - 25`. Add `load_from_db(pool, now)` that seeds each subject's `hours` map from the snapshot at boot.

- [ ] **Step 5: Wire in `main.rs`** — build `Arc<TokenUsageLimiter>` from `cfg` token limits; `limiter.load_from_db(&pool, now).await`; `limiter.refresh_overrides_now(&pool).await`; spawn the override-refresh task (mirror the rate-limit one, `cfg.rate_limit_override_refresh_secs`) and the snapshot job (`cfg.token_snapshot_secs`). Merge `admin_tokenlimits::router()` in `app.rs`.

- [ ] **Step 6: Run tests + build** → `cargo test -p mn-server --features integration --test admin_tokenlimits` then `cargo build -p mn-server`.
- [ ] **Step 7: Commit**

```bash
git add crates/mn-server/src/routes/admin_tokenlimits.rs crates/mn-server/src/jobs/token_usage_snapshot.rs crates/mn-server/src/tokenlimit.rs crates/mn-server/src/app.rs crates/mn-server/src/main.rs crates/mn-server/tests/admin_tokenlimits.rs
git commit -m "feat(mn-server): /v1/admin/tokenlimits + override refresh + usage snapshot job

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Phase 5 — `mnm tokenlimits` CLI

Mirror `crates/mn-cli/src/commands/ratelimits.rs` exactly; only the args + endpoint differ.

### Task 5.1: `tokenlimits` command module

**Files:**
- Create: `crates/mn-cli/src/commands/tokenlimits.rs`
- Modify: `crates/mn-cli/src/commands/mod.rs`, `crates/mn-cli/src/cli.rs` (add `Tokenlimits` variant + dispatch + `ADMIN_SUBCOMMANDS`)
- Test: `crates/mn-cli/tests/tokenlimits_integration.rs`

- [ ] **Step 1: Write the failing test (wiremock, mirror `ratelimits_integration.rs`)**

```rust
use mn_cli::commands::tokenlimits::add_request;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};
use std::sync::{Arc, Mutex};

fn http_client() -> reqwest::Client {
    reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().unwrap()
}

#[tokio::test]
async fn add_posts_subject_and_limits_with_bearer() {
    let server = MockServer::start().await;
    let cap = Arc::new(Mutex::new(None::<(String, serde_json::Value)>));
    let c = Arc::clone(&cap);
    Mock::given(method("POST")).and(path("/v1/admin/tokenlimits"))
        .respond_with(move |req: &Request| {
            let auth = req.headers.get("authorization").map(|h| h.to_str().unwrap().to_owned()).unwrap_or_default();
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            *c.lock().unwrap() = Some((auth, body));
            ResponseTemplate::new(201).set_body_json(serde_json::json!({"id":"00000000-0000-0000-0000-000000000000"}))
        }).mount(&server).await;

    add_request(&http_client(), &server.uri(), "user", "alice", 4000, 40000, "2030-01-01T00:00:00Z", None, "admin-tok").await.unwrap();
    let (auth, body) = cap.lock().unwrap().clone().unwrap();
    assert_eq!(auth, "Bearer admin-tok");
    assert_eq!(body["subject_kind"], "user");
    assert_eq!(body["hourly"], 4000);
    assert_eq!(body["daily"], 40000);
}
```

- [ ] **Step 2: Run, verify fail** → `cargo test -p mn-cli --test tokenlimits_integration` → FAIL (module missing).

- [ ] **Step 3: Implement `tokenlimits.rs`** mirroring `ratelimits.rs`. Subcommand enum + args:

```rust
#[derive(Debug, clap::Subcommand)]
pub enum TokenlimitsCmd { Add(AddArgs), List, Extend(ExtendArgs), Remove(RemoveArgs) }

#[derive(Debug, clap::Args)]
pub struct AddArgs {
    /// Exactly one of --cidr / --user.
    #[arg(long, conflicts_with = "user")] pub cidr: Option<String>,
    #[arg(long, conflicts_with = "cidr")] pub user: Option<String>,
    #[arg(long)] pub hourly: i64,
    #[arg(long)] pub daily: i64,
    #[arg(long)] pub ttl: String,
    #[arg(long)] pub note: Option<String>,
}
#[derive(Debug, clap::Args)] pub struct ExtendArgs { pub id: String, #[arg(long)] pub ttl: String }
#[derive(Debug, clap::Args)] pub struct RemoveArgs { pub id: String, #[arg(long)] pub yes: bool }
```

`run(args, server, json)` resolves `subject_kind`/`subject` from `--cidr`/`--user` (error if neither/both), parses `ttl` via the same `expiry_from_ttl` helper used by `ratelimits.rs` (extract it to `crate::shared` if not already shared, otherwise duplicate the small parser), gets the admin token via `require_admin_token()`, and dispatches to:

```rust
pub async fn add_request(client: &reqwest::Client, server_url: &str, subject_kind: &str, subject: &str,
    hourly: i64, daily: i64, expires_at: &str, note: Option<&str>, bearer: &str) -> anyhow::Result<serde_json::Value> {
    let mut body = serde_json::Map::new();
    body.insert("subject_kind".into(), serde_json::json!(subject_kind));
    body.insert("subject".into(), serde_json::json!(subject));
    body.insert("hourly".into(), serde_json::json!(hourly));
    body.insert("daily".into(), serde_json::json!(daily));
    body.insert("expires_at".into(), serde_json::json!(expires_at));
    if let Some(n) = note { body.insert("note".into(), serde_json::json!(n)); }
    let resp = client.post(format!("{server_url}/v1/admin/tokenlimits")).bearer_auth(bearer).json(&body).send().await?;
    decode_response(resp, "add token limit").await
}
```

Add `list_request` (GET), `extend_request` (PATCH `/:id` with `{expires_at}`), `remove_request` (DELETE `/:id`) — mirror `ratelimits.rs` 1:1.

- [ ] **Step 4: Wire into `cli.rs`** — add `Tokenlimits(commands::tokenlimits::Args)` to the `Command` enum, add `"tokenlimits"` to `ADMIN_SUBCOMMANDS`, and a dispatch arm: `Command::Tokenlimits(args) => commands::tokenlimits::run(args, cli.server.as_deref(), cli.json).await`. Add `pub mod tokenlimits;` to `commands/mod.rs`.

- [ ] **Step 5: Run tests + build** → `cargo test -p mn-cli --test tokenlimits_integration` then `cargo build -p mn-cli`.
- [ ] **Step 6: Commit**

```bash
git add crates/mn-cli/src/commands/tokenlimits.rs crates/mn-cli/src/commands/mod.rs crates/mn-cli/src/cli.rs crates/mn-cli/tests/tokenlimits_integration.rs
git commit -m "feat(mn-cli): mnm tokenlimits admin command

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Phase 6 — Client embedding swap (BYOK or server endpoint)

Replace local fastembed embedding on the corpus path with Voyage. The shared helper lives in **`mn-embedding`** (so both `mn-cli` and `mn-mcp` use it).

> **File-map correction:** the helper is `crates/mn-embedding/src/client.rs` (not `mn-cli/src/embed.rs`) so MCP can share it.

### Task 6.1: `mn_embedding::client::embed` resolution helper

**Files:**
- Create: `crates/mn-embedding/src/client.rs` (+ `lib.rs` export)
- Test: `crates/mn-embedding/tests/client_embed.rs`

- [ ] **Step 1: Write the failing tests (wiremock for both modes)**

```rust
use mn_embedding::client::{embed, EmbedSource};
use mn_embedding::voyage::{VoyageEmbedder, InputType};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn byok_uses_voyage_directly() {
    let voyage = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data":[{"embedding": vec![1.0_f32;4], "index":0}], "model":"voyage-code-3", "usage":{"total_tokens":3}})))
        .mount(&voyage).await;
    let v = VoyageEmbedder::new("k","voyage-code-3",1024,"float").with_base_url(&voyage.uri());
    let out = embed(vec!["q".into()], InputType::Query, EmbedSource::Byok(&v)).await.unwrap();
    assert_eq!(out.vectors.len(), 1);
    assert_eq!(out.total_tokens, 3);
}

#[tokio::test]
async fn server_mode_calls_v1_embeddings() {
    let srv = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model":"voyage-code-3@1", "embeddings":[vec![0.5_f32;4]], "usage":{"total_tokens":2},
            "rate":{"hour":{"limit":2000,"remaining":1998,"reset_at":"2030-01-01T00:00:00Z"},
                    "day":{"limit":20000,"remaining":19998,"reset_at":"2030-01-01T00:00:00Z"}}})))
        .mount(&srv).await;
    let out = embed(vec!["q".into()], InputType::Query, EmbedSource::Server { base_url: &srv.uri(), bearer: None }).await.unwrap();
    assert_eq!(out.vectors, vec![vec![0.5_f32;4]]);
    assert_eq!(out.total_tokens, 2);
}
```

- [ ] **Step 2: Run, verify fail** → `cargo test -p mn-embedding --test client_embed` → FAIL.

- [ ] **Step 3: Implement `client.rs`**

```rust
//! Client-side embedding resolution: BYOK (Voyage direct) or our server endpoint.
use crate::voyage::{InputType, VoyageEmbedder, VoyageError};
use serde::Deserialize;

pub enum EmbedSource<'a> {
    Byok(&'a VoyageEmbedder),
    Server { base_url: &'a str, bearer: Option<&'a str> },
}

pub struct Embedded { pub vectors: Vec<Vec<f32>>, pub total_tokens: u64 }

#[derive(Deserialize)]
struct ServerResp { embeddings: Vec<Vec<f32>>, usage: ServerUsage }
#[derive(Deserialize)]
struct ServerUsage { total_tokens: u64 }

pub async fn embed(texts: Vec<String>, input_type: InputType, src: EmbedSource<'_>) -> Result<Embedded, VoyageError> {
    match src {
        EmbedSource::Byok(v) => {
            let out = v.embed(texts, input_type).await?;
            Ok(Embedded { vectors: out.vectors, total_tokens: out.total_tokens })
        }
        EmbedSource::Server { base_url, bearer } => {
            let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()
                .map_err(|e| VoyageError::Http(e.to_string()))?;
            let it = match input_type { InputType::Query => "query", InputType::Document => "document" };
            let mut rb = client.post(format!("{}/v1/embeddings", base_url.trim_end_matches('/')))
                .json(&serde_json::json!({ "input": texts, "input_type": it }));
            if let Some(b) = bearer { rb = rb.bearer_auth(b); }
            let resp = rb.send().await.map_err(|e| VoyageError::Http(e.to_string()))?;
            let status = resp.status();
            if !status.is_success() {
                return Err(VoyageError::Status { status: status.as_u16(), body: resp.text().await.unwrap_or_default() });
            }
            let parsed: ServerResp = resp.json().await.map_err(|e| VoyageError::Decode(e.to_string()))?;
            Ok(Embedded { vectors: parsed.embeddings, total_tokens: parsed.usage.total_tokens })
        }
    }
}
```

Add `pub mod client;` to `lib.rs`.

- [ ] **Step 4: Run, verify pass** → `cargo test -p mn-embedding --test client_embed`
- [ ] **Step 5: Commit**

```bash
git add crates/mn-embedding/src/client.rs crates/mn-embedding/src/lib.rs crates/mn-embedding/tests/client_embed.rs
git commit -m "feat(mn-embedding): client embed resolution (BYOK or server)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 6.2: CLI `search` uses Voyage embedding + corpus-model wire id

**Files:**
- Modify: `crates/mn-cli/src/commands/search.rs` (replace `embedder::global` block ~129-149), `crates/mn-cli/src/cli.rs` (add global `--voyage-api-key`)
- Test: `crates/mn-cli/tests/search_embed_paths.rs`

- [ ] **Step 1: Add the `--voyage-api-key` global flag** to the top-level `Cli` struct in `cli.rs` (alongside `--server`/`--json`): `#[arg(long, global = true)] pub voyage_api_key: Option<String>`.

- [ ] **Step 2: Write the failing test** — with a mock server that serves `GET /v1/models/active` (returns `{"model":"voyage-code-3@1"}`) and `POST /v1/embeddings`, assert the CLI search path produces a `SearchRequest` whose `client_embedding_model == "voyage-code-3@1"` and `queries[0].vector` matches the mock. (Drive `search_via_http` indirectly or factor the request-building into a testable fn.)

- [ ] **Step 3: Replace the embedding block** in `search.rs`:

```rust
let env = mn_core::config::StdEnv;
let (cfg, _) = mn_core::config::Config::discover(None, &env).unwrap_or_default();
let voyage_key = mn_core::config::resolve_voyage_api_key(voyage_api_key_flag, &cfg.models, &env);

let input_type = mn_embedding::voyage::InputType::Query;
let embedded = if let Some(key) = voyage_key.as_deref() {
    let v = mn_embedding::voyage::VoyageEmbedder::new(key, &cfg.models.embedding, cfg.models.voyage_output_dimension, &cfg.models.voyage_output_dtype);
    mn_embedding::client::embed(texts.clone(), input_type, mn_embedding::client::EmbedSource::Byok(&v)).await
} else {
    mn_embedding::client::embed(texts.clone(), input_type, mn_embedding::client::EmbedSource::Server { base_url: server_url, bearer: bearer.as_deref() }).await
}.context("embed queries")?;

// client_embedding_model must equal the corpus model; resolve from the server.
let active = crate::commands::models::fetch_active(server_url).await.context("resolve active model")?;
let queries: Vec<QueryPair> = texts.into_iter().zip(embedded.vectors)
    .map(|(text, vector)| QueryPair { text, vector }).collect();
let request = SearchRequest {
    queries,
    client_embedding_model: active.model, // e.g. "voyage-code-3@1"
    limit: args.limit,
    filters: SearchFilters::default(),
};
```

Delete the now-unused `mn_embedding::embedder::global` import in this file. (Keep `args.embedding_model` only if you want a manual override; otherwise remove it.)

- [ ] **Step 4: Run tests + build** → `cargo test -p mn-cli` then `cargo build -p mn-cli`.
- [ ] **Step 5: Commit**

```bash
git add crates/mn-cli/src/commands/search.rs crates/mn-cli/src/cli.rs crates/mn-cli/tests/search_embed_paths.rs
git commit -m "feat(mn-cli): search embeds via Voyage (BYOK or server)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 6.3: CLI `ingest` embeds chunks via Voyage (`document`)

**Files:**
- Modify: `crates/mn-cli/src/commands/ingest/run.rs` (the `embed_batch` path ~699-713 and the embedder load ~389-397)
- Test: extend `crates/mn-cli` ingest tests with a wiremock Voyage server

- [ ] **Step 1: Write/adjust the failing test** — assert that with `VOYAGE_API_KEY` set (or `--voyage-api-key`), the ingest run embeds each chunk batch via Voyage (`input_type=document`) and uploads chunks carrying 1024-dim vectors. (Use the existing ingest test scaffold; point Voyage at a `MockServer`.)

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Replace `embed_batch`** to call `mn_embedding::client::embed(texts, InputType::Document, source)` where `source` is `Byok(&VoyageEmbedder)` when a key is resolved, else `Server { base_url, bearer }`. Batch to ≤1000 texts / ≤120K tokens before each call. Remove the local `embedder::global`/`embed_blocking` usage and the `--enable-server-embedding` flag (the design removes it). Resolve the key once at the start of the run via `resolve_voyage_api_key`.

- [ ] **Step 4: Run tests + build** → `cargo test -p mn-cli` ; `cargo build -p mn-cli`.
- [ ] **Step 5: Commit**

```bash
git add crates/mn-cli/src/commands/ingest/run.rs
git commit -m "feat(mn-cli): ingest embeds chunks via Voyage (document input type)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 6.4: MCP `search` embeds via Voyage

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs` (embedding call ~499-506; `client_embedding_model` build)
- Test: `crates/mn-mcp` tests (mock Voyage + a stub search server, or unit-test the embedding selection)

- [ ] **Step 1: Write the failing test** — given a config/env Voyage key, the MCP search tool embeds via `mn_embedding::client::embed(.. InputType::Query ..)` rather than the local fastembed embedder, and labels the request with the corpus wire id (from `/v1/models/active`).

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Replace the embedder block** in `tools.rs` mirroring Task 6.2 (resolve key from MCP config/env; BYOK vs `Server`). Keep `LOADED_MARKERS.mark_embedder()` only if still meaningful (BYOK has no local model load — drop the marker on the Voyage path or repurpose it). Remove the local `embedder::global`/`embed_blocking` on the corpus path.

- [ ] **Step 4: Run tests + build** → `cargo test -p mn-mcp` ; `cargo build -p mn-mcp`.
- [ ] **Step 5: Commit**

```bash
git add crates/mn-mcp/src/tools.rs
git commit -m "feat(mn-mcp): search embeds via Voyage (BYOK or server)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 6.5: Retire the dead fastembed embedder + fix `mnm models pull`

After 6.2–6.4 nothing on the corpus path uses the local fastembed `Embedder`, so `clippy -D warnings` will flag dead code, and `mnm models pull` no longer needs to download an embedder.

**Files:**
- Modify/Delete: `crates/mn-embedding/src/embedder.rs`, `crates/mn-embedding/src/lib.rs` (exports), `crates/mn-cli/src/commands/models.rs` (`run_pull`)
- Test: adjust any test that referenced `embedder::global` / `BGE_BASE_DIM` / `EMBEDDER_MODEL_NAME`

- [ ] **Step 1: Find references** — `grep -rn "embedder::global\|EMBEDDER_MODEL_NAME\|BGE_BASE_DIM\|mn_embedding::embedder" crates/`. Expected after Phase 6: only `models.rs` `run_pull` and the embedder's own tests.
- [ ] **Step 2: Remove the module** — delete `crates/mn-embedding/src/embedder.rs` and its `pub mod embedder;` + `pub use embedder::{…}` lines in `lib.rs`. (The reranker still uses fastembed, so keep the `fastembed` dependency.)
- [ ] **Step 3: Fix `run_pull`** — change it to pull only the reranker (the configured one via `reranker_catalog`), dropping the embedder download. Update its help text/output (no more "downloads ~100 MB embedder").
- [ ] **Step 4: Verify** — Run: `cargo build --workspace` then `cargo clippy --workspace --all-targets -- -D warnings`.
Expected: clean (no dead-code/unused-import warnings).
- [ ] **Step 5: Commit**

```bash
git add crates/mn-embedding/src/embedder.rs crates/mn-embedding/src/lib.rs crates/mn-cli/src/commands/models.rs
git commit -m "refactor(mn-embedding): retire fastembed embedder (corpus is Voyage now)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Phase 7 — Initial corpus cutover (operational)

No code; this populates the voyage-code-3 corpus and validates the quality win. Do this against a **dev/staging** server first.

- [ ] **Step 1: Enable zero-retention on the server's Voyage account** — in the Voyage dashboard, add a payment method, become org admin, accept the ToS, and toggle the training opt-out (zero-day retention). Record the date in `docs/README-deploy.md`. (Spec §9.)
- [ ] **Step 2: Set the server secret** — `flyctl secrets set VOYAGE_API_KEY=<key>` (or the dev `.env`). Confirm the server logs `resolved active embedding model corpus_model=voyage-code-3@1` on boot and `/readyz` is 200.
- [ ] **Step 3: Re-ingest the corpus with Voyage** — on the ingest host export `VOYAGE_API_KEY`, then run `scripts/ingest-midnight.sh --server <dev-url>`. Each source ingests + finalizes on `voyage-code-3`. Confirm the per-source progress lines show chunk counts.
- [ ] **Step 4: Verify search quality** — run several real queries (`mnm --server <dev-url> search "compile a compact contract"` etc.) and compare against the prior bge results. This is the acceptance check for the whole effort. Record before/after notes.
- [ ] **Step 5 (no commit; operational).** If quality is good, promote the secret to production and re-ingest prod.

---

## Phase 8 — Model migration tooling (`mnm models migrate` + `status`)

For future model changes (e.g. `voyage-code-4`): re-ingest per source onto the target model, provenance-ordered, with visibility into incomplete migrations.

### Task 8.1: `source::list_active_not_on_model` (provenance-ordered)

**Files:**
- Modify: `crates/mn-store/src/entities/source.rs` (+ a small return struct in `mn-core/types.rs` if needed)
- Test: `crates/mn-store/tests/sources_not_on_model.rs`

- [ ] **Step 1: Write the failing integration test**

```rust
#![cfg(feature = "integration")]
mod common;
#[tokio::test]
async fn lists_sources_whose_active_version_is_not_on_target_ordered_by_provenance() {
    let h = common::boot().await;
    // seed: source A (active sv on bge model id, docs attribution "partner"),
    //       source B (active sv on bge, docs attribution "foundation").
    // target = voyage-code-3 model id.
    let target = mn_store::entities::embedding_model::get_by_name_revision(&h.pool, "voyage-code-3", 1).await.unwrap();
    let rows = mn_store::entities::source::list_active_not_on_model(&h.pool, target.id).await.unwrap();
    // Foundation source first.
    assert_eq!(rows[0].slug, "source-b");
    assert_eq!(rows[1].slug, "source-a");
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement the query** in `source.rs`:

```rust
/// Sources whose ACTIVE version is not on `target_model_id`, ordered by best
/// (lowest-rank) document attribution then slug. Foundation(1) → … → Unknown(5).
pub async fn list_active_not_on_model(pool: &PgPool, target_model_id: Uuid) -> Result<Vec<Source>> {
    let rows: Vec<SourceRow> = sqlx::query_as(
        "SELECT s.id, s.slug, s.display_name, s.kind, s.origin_url, s.retention_count, s.created_at, s.retired_at \
         FROM source s \
         JOIN source_version sv ON sv.source_id = s.id AND sv.is_active = true \
         LEFT JOIN document d ON d.source_version_id = sv.id \
         WHERE s.retired_at IS NULL AND sv.embedding_model_id <> $1 \
         GROUP BY s.id, s.slug, s.display_name, s.kind, s.origin_url, s.retention_count, s.created_at, s.retired_at \
         ORDER BY MIN(CASE d.provenance->>'attribution' \
                        WHEN 'foundation' THEN 1 WHEN 'partner' THEN 2 WHEN 'third_party' THEN 3 \
                        WHEN 'community' THEN 4 ELSE 5 END) ASC NULLS LAST, s.slug ASC")
        .bind(target_model_id)
        .fetch_all(pool).await?;
    Ok(rows.into_iter().map(Into::into).collect())
}
```

- [ ] **Step 4: Run, verify pass** → `cargo test -p mn-store --features integration --test sources_not_on_model`
- [ ] **Step 5: Commit**

```bash
git add crates/mn-store/src/entities/source.rs crates/mn-store/tests/sources_not_on_model.rs
git commit -m "feat(mn-store): list_active_not_on_model (provenance-ordered)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 8.2: `GET /v1/admin/sources?not_model=` + `mnm models status`

**Files:**
- Modify: `crates/mn-server/src/routes/admin_sources.rs` (add the query handler), `crates/mn-cli/src/commands/models.rs` (add `Status`)
- Test: `crates/mn-server/tests/admin_sources_not_model.rs`, `crates/mn-cli/tests/models_status.rs`

- [ ] **Step 1: Write the failing server test** — `GET /v1/admin/sources?not_model=voyage-code-3@1` (admin bearer) returns the provenance-ordered slugs; non-admin → 403.

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement the handler** — parse `not_model` query param to `EmbeddingModelId`, look up the model id via `embedding_model::get_by_name_revision`, call `source::list_active_not_on_model`, return `{"sources":[{"slug","origin_url"}...]}`. Gate with `admin_reject`. Add the route to `admin_sources::router()` (or a small dedicated route). Reference §5.3 of the spec.

- [ ] **Step 4: Implement `mnm models status`** — add `Status(StatusArgs)` to `ModelsCmd`; `run_status` GETs `/v1/admin/sources?not_model=<active>` (active resolved via `fetch_active`) and prints the sources still on the old model (or "all sources on <model>" when empty). Admin token via `require_admin_token()`.

- [ ] **Step 5: Run tests + build** → `cargo test -p mn-server --features integration --test admin_sources_not_model` ; `cargo test -p mn-cli --test models_status` ; `cargo build -p mn-cli -p mn-server`.
- [ ] **Step 6: Commit**

```bash
git add crates/mn-server/src/routes/admin_sources.rs crates/mn-cli/src/commands/models.rs crates/mn-server/tests/admin_sources_not_model.rs crates/mn-cli/tests/models_status.rs
git commit -m "feat: GET /v1/admin/sources?not_model + mnm models status

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 8.3: `mnm models migrate --to` driver

Re-ingests each not-on-target source onto the target model (reusing the ingest pipeline from Phase 6.3), provenance-ordered, with a session token budget and max-docs, aborting (not finalizing) the in-flight source on a limit/429.

**Files:**
- Modify: `crates/mn-cli/src/commands/models.rs` (add `Migrate`)
- Test: `crates/mn-cli/tests/models_migrate.rs`

- [ ] **Step 1: Add the subcommand + args**

```rust
#[derive(Debug, clap::Args)]
pub struct MigrateArgs {
    /// Target model wire id, e.g. "voyage-code-4@1". Defaults to the active model.
    #[arg(long)] pub to: Option<String>,
    /// Comma-separated source names to restrict the run.
    #[arg(long, value_delimiter = ',')] pub source: Vec<String>,
    /// Stop after this many documents (evaluated at source boundaries).
    #[arg(long)] pub max_docs: Option<u64>,
    /// Client-side session token budget (sums Voyage usage across server + BYOK).
    #[arg(long)] pub token_budget: Option<u64>,
    /// Manifest directory (defaults to manifests/midnight).
    #[arg(long, default_value = "manifests/midnight")] pub manifests_dir: std::path::PathBuf,
}
```

- [ ] **Step 2: Write the failing test** — with a mock server serving `GET /v1/admin/sources?not_model=...` (two sources) + `/v1/models/active`, and a `--token-budget` smaller than one source needs, assert the driver migrates 0–1 sources and stops cleanly without finalizing the over-budget source. (Stub the ingest at the boundary; or factor the "process one source" step into a testable unit and assert the budget-stop logic.)

- [ ] **Step 3: Implement `run_migrate`** — resolve target (flag or `fetch_active`), resolve target model id, GET `/v1/admin/sources?not_model=<target>` (provenance-ordered), filter by `--source`. Maintain `spent_tokens` + `done_docs`. For each source: locate `manifests_dir/<slug>.yaml`, clone `origin_url`, run the ingest pipeline on the target model (reusing Phase 6.3's embed path, which returns `total_tokens` per batch — accumulate into `spent_tokens`). Before starting a source, if `--max-docs`/`--token-budget` would be exceeded, stop. If a limit/`429` trips **mid-source**, **do not call finalize** (leave the run `building` for the retention sweep), log it, and stop. Print a summary (migrated N sources / M docs / `spent_tokens`; remaining sources). Budget never bypasses the server rate limit (a server-path `429` also stops the run).

- [ ] **Step 4: Run tests + build** → `cargo test -p mn-cli --test models_migrate` ; `cargo build -p mn-cli`.
- [ ] **Step 5: Commit**

```bash
git add crates/mn-cli/src/commands/models.rs crates/mn-cli/tests/models_migrate.rs
git commit -m "feat(mn-cli): mnm models migrate (re-ingest per source, budgeted)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Phase 9 — Configurable reranker catalog

Reranking stays client-side. Add a selectable catalog (fastembed native + auto-fetched ONNX + custom path + Voyage API).

### Task 9.1: `VoyageReranker` (voyage.rs)

**Files:**
- Modify: `crates/mn-embedding/src/voyage.rs`
- Test: `crates/mn-embedding/tests/voyage_rerank.rs`

- [ ] **Step 1: Write the failing test (mock `/v1/rerank`)**

```rust
use mn_embedding::voyage::VoyageReranker;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn reranks_and_returns_sorted_indices() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/rerank"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object":"list",
            "data":[{"relevance_score":0.2,"index":0},{"relevance_score":0.9,"index":1}],
            "model":"rerank-2.5-lite","usage":{"total_tokens":8}})))
        .mount(&server).await;
    let r = VoyageReranker::new("k","rerank-2.5-lite").with_base_url(&server.uri());
    let out = r.rerank("q".into(), vec!["a".into(),"b".into()], None).await.unwrap();
    // returns RerankResult{index,score}; index 1 has the higher score
    assert_eq!(out.results.iter().max_by(|a,b| a.score.total_cmp(&b.score)).unwrap().index, 1);
    assert_eq!(out.total_tokens, 8);
}
```

- [ ] **Step 2: Run, verify fail** → `cargo test -p mn-embedding --test voyage_rerank`.

- [ ] **Step 3: Implement `VoyageReranker`** in `voyage.rs` (returns `RerankOutput { results: Vec<crate::reranker::RerankResult>, total_tokens: u64 }`):

```rust
#[derive(Clone)]
pub struct VoyageReranker { client: reqwest::Client, api_key: String, model: String, base_url: String }
pub struct RerankOutput { pub results: Vec<crate::reranker::RerankResult>, pub total_tokens: u64 }

impl VoyageReranker {
    #[must_use] pub fn new(api_key: &str, model: &str) -> Self {
        Self { client: reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().expect("client"),
               api_key: api_key.into(), model: model.into(), base_url: super::DEFAULT_BASE_URL.into() }
    }
    #[must_use] pub fn with_base_url(mut self, b: &str) -> Self { self.base_url = b.trim_end_matches('/').into(); self }

    pub async fn rerank(&self, query: String, documents: Vec<String>, top_k: Option<usize>) -> Result<RerankOutput, VoyageError> {
        #[derive(serde::Serialize)] struct Req<'a> { model: &'a str, query: String, documents: Vec<String>, #[serde(skip_serializing_if="Option::is_none")] top_k: Option<usize> }
        #[derive(serde::Deserialize)] struct Data { relevance_score: f32, index: usize }
        #[derive(serde::Deserialize)] struct Resp { data: Vec<Data>, usage: super::Usage }
        let resp = self.client.post(format!("{}/v1/rerank", self.base_url)).bearer_auth(&self.api_key)
            .json(&Req { model: &self.model, query, documents, top_k }).send().await
            .map_err(|e| VoyageError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() { return Err(VoyageError::Status { status: status.as_u16(), body: resp.text().await.unwrap_or_default() }); }
        let parsed: Resp = resp.json().await.map_err(|e| VoyageError::Decode(e.to_string()))?;
        Ok(RerankOutput {
            results: parsed.data.into_iter().map(|d| crate::reranker::RerankResult { index: d.index, score: d.relevance_score }).collect(),
            total_tokens: parsed.usage.total_tokens,
        })
    }
}
```

(Make the `Usage` struct + `DEFAULT_BASE_URL` `pub(crate)` so the reranker can reuse them.)

- [ ] **Step 4: Run, verify pass; commit**

```bash
git add crates/mn-embedding/src/voyage.rs crates/mn-embedding/tests/voyage_rerank.rs
git commit -m "feat(mn-embedding): VoyageReranker client

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 9.2: Reranker catalog resolver (id → spec)

**Files:**
- Create: `crates/mn-embedding/src/reranker_catalog.rs` (+ `lib.rs` export)
- Test: `crates/mn-embedding/src/reranker_catalog.rs` inline tests

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn native_ids_resolve() {
        assert!(matches!(resolve("bge-reranker-base", None).unwrap(), RerankerSpec::Native(_)));
        assert!(matches!(resolve("jina-reranker-v1-turbo-en", None).unwrap(), RerankerSpec::Native(_)));
        assert!(matches!(resolve("bge-reranker-v2-m3", None).unwrap(), RerankerSpec::Native(_)));
    }
    #[test] fn onnx_ids_resolve_to_hf_repo() {
        assert!(matches!(resolve("ms-marco-minilm-l6", None).unwrap(), RerankerSpec::UserOnnx { .. }));
    }
    #[test] fn voyage_ids_require_key_at_use_but_resolve_spec() {
        assert!(matches!(resolve("voyage-rerank-2.5-lite", None).unwrap(), RerankerSpec::Voyage(_)));
    }
    #[test] fn custom_requires_path() {
        assert!(resolve("custom", None).is_err());
        assert!(matches!(resolve("custom", Some(std::path::Path::new("/tmp/m"))).unwrap(), RerankerSpec::CustomPath(_)));
    }
    #[test] fn unknown_id_errors() { assert!(resolve("nope", None).is_err()); }
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement the resolver + catalog table**

```rust
//! Reranker catalog: maps a config id to a loadable spec. Reranking is always
//! client-side. See the design doc §8 for the curated list + licences.
use fastembed::RerankerModel;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum RerankerSpec {
    Native(RerankerModel),
    /// ONNX mirror fetched via hf-hub; `files` are the model + 4 tokenizer files.
    UserOnnx { repo: &'static str, model_file: &'static str },
    CustomPath(PathBuf),
    Voyage(String), // voyage model name, e.g. "rerank-2.5-lite"
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("unknown reranker id `{0}` (see `mnm models pull --help` for the catalog)")] Unknown(String),
    #[error("reranker `custom` requires a --reranker-path / models.reranker_path")] CustomPathMissing,
}

pub fn resolve(id: &str, custom_path: Option<&Path>) -> Result<RerankerSpec, CatalogError> {
    Ok(match id {
        // fastembed native
        "bge-reranker-base" => RerankerSpec::Native(RerankerModel::BGERerankerBase),
        "bge-reranker-v2-m3" => RerankerSpec::Native(RerankerModel::BGERerankerV2M3),
        "jina-reranker-v1-turbo-en" => RerankerSpec::Native(RerankerModel::JINARerankerV1TurboEn),
        // user-defined ONNX (Xenova mirrors for MiniLM; self-supply for mxbai-base-v1)
        "ms-marco-minilm-l2"  => RerankerSpec::UserOnnx { repo: "Xenova/ms-marco-MiniLM-L-2-v2",  model_file: "onnx/model.onnx" },
        "ms-marco-minilm-l6"  => RerankerSpec::UserOnnx { repo: "Xenova/ms-marco-MiniLM-L-6-v2",  model_file: "onnx/model.onnx" },
        "ms-marco-minilm-l12" => RerankerSpec::UserOnnx { repo: "Xenova/ms-marco-MiniLM-L-12-v2", model_file: "onnx/model.onnx" },
        "mxbai-rerank-base-v1" => RerankerSpec::UserOnnx { repo: "mixedbread-ai/mxbai-rerank-base-v1", model_file: "onnx/model.onnx" },
        "mxbai-rerank-base-v2" => RerankerSpec::UserOnnx { repo: "mixedbread-ai/mxbai-rerank-base-v2", model_file: "onnx/model.onnx" }, // experimental
        // custom
        "custom" => RerankerSpec::CustomPath(custom_path.ok_or(CatalogError::CustomPathMissing)?.to_path_buf()),
        // Voyage API (requires VOYAGE_API_KEY at use)
        "voyage-rerank-2.5"      => RerankerSpec::Voyage("rerank-2.5".into()),
        "voyage-rerank-2.5-lite" => RerankerSpec::Voyage("rerank-2.5-lite".into()),
        "voyage-rerank-2"        => RerankerSpec::Voyage("rerank-2".into()),
        other => return Err(CatalogError::Unknown(other.to_owned())),
    })
}
```

(Note in a code comment that `jina-reranker-v2-base-multilingual` is intentionally excluded — cc-by-nc-4.0.) Add `pub mod reranker_catalog;` to `lib.rs`.

- [ ] **Step 4: Run, verify pass; commit**

```bash
git add crates/mn-embedding/src/reranker_catalog.rs crates/mn-embedding/src/lib.rs
git commit -m "feat(mn-embedding): reranker catalog resolver (native/onnx/custom/voyage)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 9.3: Load a reranker from a spec (`LoadedReranker`)

**Files:**
- Modify: `crates/mn-embedding/src/reranker.rs` (add `load_from_spec` + `LoadedReranker`)
- Test: `crates/mn-embedding/tests/reranker_load.rs` (native happy path; Voyage via wiremock; custom-path error when files missing)

- [ ] **Step 1: Write the failing tests** — (a) `LoadedReranker::load(RerankerSpec::Native(BGERerankerBase), …)` loads and reranks 2 docs (gated behind a `#[ignore]`/network attribute if CI lacks model download; mirror how existing reranker tests handle the download); (b) Voyage spec + key → uses `VoyageReranker` (wiremock); (c) `custom` with a non-existent dir → error.

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement**

```rust
use crate::reranker_catalog::RerankerSpec;
use crate::voyage::VoyageReranker;

pub enum LoadedReranker { Local(Reranker), Voyage(VoyageReranker) }

impl LoadedReranker {
    /// `voyage_key` is required for Voyage specs; ignored otherwise.
    pub async fn load(spec: RerankerSpec, cache_dir: std::path::PathBuf, voyage_key: Option<&str>) -> Result<Self> {
        match spec {
            RerankerSpec::Native(model) => Ok(Self::Local(Reranker::try_new_model(model, cache_dir)?)),
            RerankerSpec::UserOnnx { repo, model_file } => {
                // hf-hub fetch model_file + tokenizer.json/config.json/special_tokens_map.json/tokenizer_config.json,
                // then fastembed `TextRerank::try_new_from_user_defined(UserDefinedRerankingModel::new(OnnxSource::File(p), TokenizerFiles{..}), RerankInitOptionsUserDefined{ max_length: 512, execution_providers: vec![] })`.
                Ok(Self::Local(Reranker::try_new_user_defined(repo, model_file, cache_dir)?))
            }
            RerankerSpec::CustomPath(dir) => Ok(Self::Local(Reranker::try_new_user_defined_path(&dir)?)),
            RerankerSpec::Voyage(model) => {
                let key = voyage_key.ok_or(EmbeddingError::Init { model: model.clone(), message: "Voyage reranker requires VOYAGE_API_KEY".into() })?;
                Ok(Self::Voyage(VoyageReranker::new(key, &model)))
            }
        }
    }

    pub async fn rerank(&self, query: String, documents: Vec<String>) -> Result<Vec<RerankResult>> {
        match self {
            Self::Local(r) => r.rerank_blocking(query, documents, None).await,
            Self::Voyage(v) => v.rerank(query, documents, None).await
                .map(|o| o.results)
                .map_err(|e| EmbeddingError::Inference { model: "voyage-rerank".into(), message: e.to_string() }),
        }
    }
}
```

Add the supporting `Reranker::try_new_model(model, cache_dir)`, `try_new_user_defined(repo, model_file, cache_dir)` (hf-hub download + `try_new_from_user_defined`), and `try_new_user_defined_path(dir)` (read `model.onnx` + the 4 tokenizer files from `dir`) constructors in `reranker.rs`. The current `Reranker::try_new(cache_dir)` becomes `try_new_model(RerankerModel::BGERerankerBase, cache_dir)`.

- [ ] **Step 4: Run, verify pass; commit**

```bash
git add crates/mn-embedding/src/reranker.rs crates/mn-embedding/tests/reranker_load.rs
git commit -m "feat(mn-embedding): LoadedReranker (native/onnx/custom/voyage)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 9.4: MCP + CLI reranker selection

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs` (use `reranker_catalog::resolve` + `LoadedReranker` instead of `reranker::global`), `crates/mn-cli/src/commands/search.rs` + `cli.rs` (add `--rerank` + `--reranker`)
- Test: `crates/mn-mcp` reranker-selection test; `crates/mn-cli` `--rerank` flag test

- [ ] **Step 1: Write the failing tests** — (a) MCP: when config `reranker = "voyage-rerank-2.5-lite"` and a key is set, the search tool reranks via `VoyageReranker` (wiremock), not local; (b) CLI: `mnm search --rerank` triggers a rerank pass (and without it, no rerank — current behavior).

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement** — In `tools.rs`, replace `reranker::global(...)` with: resolve the reranker id (`mn_core::config::resolve_reranker(flag=None, &cfg.models, &env)`) → `reranker_catalog::resolve(id, cfg.models.reranker_path.as_deref())` → `LoadedReranker::load(spec, cache_dir, voyage_key)`; rerank with the pivot query. Cache the loaded reranker in a `OnceCell` keyed appropriately (or load per-call behind the existing lazy pattern). In `cli.rs` add `#[arg(long)] pub rerank: bool` and `#[arg(long)] pub reranker: Option<String>` to search args; when `--rerank`, after `search_via_http` returns candidates, run a `LoadedReranker` pass (mirroring the MCP `rerank_postprocess`) before printing. MCP keeps rerank-on-by-default.

- [ ] **Step 4: Run tests + build** → `cargo test -p mn-mcp -p mn-cli` ; `cargo build -p mn-mcp -p mn-cli`.
- [ ] **Step 5: Commit**

```bash
git add crates/mn-mcp/src/tools.rs crates/mn-cli/src/commands/search.rs crates/mn-cli/src/cli.rs
git commit -m "feat: configurable reranker selection (MCP + CLI --rerank)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Phase 10 — Privacy posture + docs + canary

### Task 10.1: Docs — third-party processing + zero-retention

**Files:**
- Modify: `README.md` (extend "Telemetry & Privacy"), `docs/README-deploy.md`
- Test: none (docs) — verify links/markdown render.

- [ ] **Step 1:** Add a README "Embeddings & third-party processing" subsection: corpus = public repos; non-BYOK query text reaches Voyage under the server's account (training disabled); BYOK sends text to the user's own Voyage account; token counts/subject keys are the only things logged. State the `VOYAGE_API_KEY`/`--voyage-api-key` BYOK path and the reranker catalog options + the `jina-v2-multilingual` NC-licence caveat.
- [ ] **Step 2:** Add to `docs/README-deploy.md`: the operational requirement to enable Voyage zero-retention on the server account, and the new env vars (`VOYAGE_API_KEY`, `MIDNIGHT_MANUAL_TOKEN_LIMIT_*`, `MIDNIGHT_MANUAL_VOYAGE_*`, `MIDNIGHT_MANUAL_TOKEN_SNAPSHOT_SECS`).
- [ ] **Step 3: Commit**

```bash
git add README.md docs/README-deploy.md
git commit -m "docs: embeddings third-party processing + Voyage deploy config

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 10.2: Privacy canary — no query text on the embeddings path

**Files:**
- Create: `tests/canary/embeddings_no_query_text.rs` (or extend the existing canary suite under `tests/canary/`)
- Test: the canary itself

- [ ] **Step 1: Write the canary test** — boot the server with a `tracing` subscriber capturing logs into a buffer (mirror the existing canary's capture approach), mock Voyage, POST `/v1/embeddings` with a sentinel input string like `"CANARY_SECRET_QUERY_TOKEN"`, and assert the sentinel **never** appears in captured logs nor in any emitted telemetry event. Also assert a 429 response body contains no input text.

- [ ] **Step 2: Run, verify it passes** (the handler/accounting must not log input text). If it fails, remove any `tracing` field that includes input text. Run: `just test-canary` (or `cargo test -p mn-server --features integration --test embeddings_no_query_text`).

- [ ] **Step 3: Wire into the canary CI gate** — add the test to `.github/workflows/canary.yml`'s invocation if it runs a specific set (mirror how existing canary tests are listed).

- [ ] **Step 4: Commit**

```bash
git add tests/canary/embeddings_no_query_text.rs .github/workflows/canary.yml
git commit -m "test(canary): embeddings path emits no query text

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Acceptance / definition of done

- `just check` and `cargo test --workspace --features integration` pass; `just test-canary` passes.
- With `VOYAGE_API_KEY` set, `mnm search` and `mnm ingest run` work end-to-end on `voyage-code-3@1`; without it (and a server key configured), the same flows work via `/v1/embeddings`.
- `/v1/embeddings` returns `{ model, embeddings, usage.total_tokens, rate.{hour,day} }`; over-cap → 429 (`token_limit_exceeded`, `Retry-After`); no key → 503; >1000 inputs → 413.
- `mnm tokenlimits {add,list,extend,remove}` (CIDR or user) changes effective limits within ~30s.
- `mnm models migrate --to` re-ingests not-on-target sources provenance-ordered, honouring `--max-docs`/`--token-budget` (abort-not-promote on limit/429); `mnm models status` lists sources still on the old model.
- Reranker is selectable (native/onnx/custom/voyage) via config/`--reranker`; CLI `--rerank` works; MCP reranks by default.
- Search excludes chunks whose `sv.embedding_model_id` ≠ corpus model; the FR-063 retention sweep still prunes superseded versions.
- README + deploy docs updated; privacy canary green.

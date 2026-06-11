# VoyageAI Reranking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move all reranking to VoyageAI rerank-2.5 / rerank-2.5-lite — inline server-side rerank in `POST /v1/search` (charged to token budgets, half-rate for lite), local BYOK rerank in CLI/MCP, instruction-following with derived defaults — and delete the entire local fastembed/ONNX reranker subsystem.

**Architecture:** Spec at `docs/superpowers/specs/2026-06-11-voyage-reranking-design.md`. New `mn_core::rerank` module holds the shared vocabulary (model enum, billed-token math, instruction derivation, query composition). The server gains a rerank stage between scoring and confidence-filtering that degrades gracefully (never fails search). Clients resolve placement (`local`/`server`/`off`, default auto on `VOYAGE_API_KEY`) and structurally guarantee one rerank pass by sending `rerank: "none"` whenever they rerank locally.

**Tech Stack:** Rust workspace (see CLAUDE.md). `VoyageReranker` HTTP client already exists in `mn-embedding/src/voyage.rs`. Server tests use `wiremock`. Verify with `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace` (per the verify-against-full-ci-surface rule).

**Conventions for every task:**
- TDD: write the failing test, run it, implement, run again, commit.
- Run unit tests with `cargo test -p <crate> <filter>`. Integration tests (`crates/mn-server/tests/`) need Docker/`DATABASE_URL` and are verified in CI, not the sandbox — for those, get them compiling (`cargo test -p mn-server --no-run --features integration` if that's how sibling tests gate, check the existing `[[test]]`/feature setup in `crates/mn-server/Cargo.toml`) and rely on CI.
- BYOK-path client tests must run with `VOYAGE_API_KEY=` cleared (the sandbox exports it globally): `VOYAGE_API_KEY= cargo test -p mn-cli ...`.
- Doc comments on all new public items (workspace denies missing docs in several crates — mirror sibling files).

---

### Task 1: `mn_core::rerank` — shared vocabulary module

**Files:**
- Create: `crates/mn-core/src/rerank.rs`
- Modify: `crates/mn-core/src/lib.rs` (add `pub mod rerank;` alongside the existing module list)

- [ ] **Step 1: Write the failing tests** (inside `crates/mn-core/src/rerank.rs`, module skeleton + tests first)

```rust
//! Shared reranking vocabulary (spec: docs/superpowers/specs/2026-06-11-voyage-reranking-design.md).
//!
//! Used identically by the server's inline rerank stage and by clients
//! reranking locally (BYOK), so the same search reranks the same way
//! regardless of placement.

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerank_param_wire_values_round_trip() {
        for (variant, wire) in [
            (RerankParam::Rerank25, "\"rerank-2.5\""),
            (RerankParam::Rerank25Lite, "\"rerank-2.5-lite\""),
            (RerankParam::None, "\"none\""),
        ] {
            assert_eq!(serde_json::to_string(&variant).unwrap(), wire);
            let back: RerankParam = serde_json::from_str(wire).unwrap();
            assert_eq!(back, variant);
        }
        // Default (omitted on the wire) is the full model.
        assert_eq!(RerankParam::default(), RerankParam::Rerank25);
    }

    #[test]
    fn model_name_is_none_only_for_none() {
        assert_eq!(RerankParam::Rerank25.model_name(), Some("rerank-2.5"));
        assert_eq!(RerankParam::Rerank25Lite.model_name(), Some("rerank-2.5-lite"));
        assert_eq!(RerankParam::None.model_name(), None);
    }

    #[test]
    fn lite_bills_half_rounded_up() {
        // D5: lite charges ceil(total/2); the full model charges face value.
        assert_eq!(RerankParam::Rerank25.billed_tokens(1001), 1001);
        assert_eq!(RerankParam::Rerank25Lite.billed_tokens(1000), 500);
        assert_eq!(RerankParam::Rerank25Lite.billed_tokens(1001), 501);
        assert_eq!(RerankParam::Rerank25Lite.billed_tokens(0), 0);
        assert_eq!(RerankParam::Rerank25Lite.billed_tokens(1), 1);
        // None never reaches billing, but must not panic.
        assert_eq!(RerankParam::None.billed_tokens(10), 10);
    }

    #[test]
    fn instruction_cap_is_400_chars() {
        assert!(validate_instruction(&"x".repeat(400)).is_ok());
        let err = validate_instruction(&"x".repeat(401)).unwrap_err();
        assert!(err.contains("400"), "error should name the cap: {err}");
        // Cap counts chars, not bytes (a 200-char multibyte string passes).
        assert!(validate_instruction(&"é".repeat(400)).is_ok());
    }

    #[test]
    fn default_instruction_rule_table() {
        // No condition -> bare query (None).
        assert_eq!(default_instruction(false, None), None);
        // code_mode exclusive -> code-focused instruction.
        let code = default_instruction(true, None).unwrap();
        assert!(code.contains("code examples"));
        // Version filter -> version preference, naming language + version.
        let ver = default_instruction(false, Some(("compact", "0.31"))).unwrap();
        assert!(ver.contains("compact") && ver.contains("0.31"));
        // Both -> both sentences concatenated (non-contradictory by construction).
        let both = default_instruction(true, Some(("compact", "0.31"))).unwrap();
        assert!(both.contains("code examples") && both.contains("0.31"));
    }

    #[test]
    fn compose_appends_instruction_to_query() {
        assert_eq!(compose_rerank_query("how do circuits work", None), "how do circuits work");
        assert_eq!(compose_rerank_query("q", Some("   ")), "q");
        let composed = compose_rerank_query("q", Some("Prioritize code."));
        assert_eq!(composed, "q\nInstructions: Prioritize code.");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mn-core rerank`
Expected: compile FAIL — `RerankParam` etc. not defined.

- [ ] **Step 3: Implement the module** (above the `tests` module)

```rust
/// Hard cap on agent-supplied rerank instructions, in characters. The
/// instruction is multiplied by the candidate-pool size in Voyage's token
/// formula (`query_tokens × num_documents`), so length is a direct cost lever.
pub const MAX_INSTRUCTION_CHARS: usize = 400;

/// The `rerank` request parameter: which Voyage model to rerank with, or none.
///
/// Omitting the parameter defaults to the full model (`rerank-2.5`). Clients
/// reranking locally always send `none` (one rerank pass, structurally).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RerankParam {
    /// VoyageAI `rerank-2.5` (the default; full quality).
    #[default]
    #[serde(rename = "rerank-2.5")]
    Rerank25,
    /// VoyageAI `rerank-2.5-lite` (lower latency; billed at half tokens, D5).
    #[serde(rename = "rerank-2.5-lite")]
    Rerank25Lite,
    /// No server-side reranking (RRF order).
    #[serde(rename = "none")]
    None,
}

impl RerankParam {
    /// The Voyage model name to call, or `None` when reranking is off.
    #[must_use]
    pub const fn model_name(self) -> Option<&'static str> {
        match self {
            Self::Rerank25 => Some("rerank-2.5"),
            Self::Rerank25Lite => Some("rerank-2.5-lite"),
            Self::None => None,
        }
    }

    /// Billed-equivalent tokens for a Voyage-reported `total_tokens` (D5):
    /// `rerank-2.5-lite` is charged at `ceil(total / 2)` — mirroring Voyage's
    /// half-rate pricing for lite — everything else at face value.
    #[must_use]
    pub const fn billed_tokens(self, total_tokens: u64) -> u64 {
        match self {
            Self::Rerank25Lite => total_tokens.div_ceil(2),
            _ => total_tokens,
        }
    }
}

/// Validate an agent-supplied instruction against [`MAX_INSTRUCTION_CHARS`].
///
/// # Errors
///
/// Returns a human-readable message naming the cap when the instruction is too
/// long (callers reject with 400 / InvalidInput — never truncate silently).
pub fn validate_instruction(instruction: &str) -> Result<(), String> {
    let n = instruction.chars().count();
    if n > MAX_INSTRUCTION_CHARS {
        return Err(format!(
            "rerank_instructions is {n} characters; the cap is {MAX_INSTRUCTION_CHARS}. \
             Shorter instructions also cost fewer tokens (the instruction is \
             multiplied by the candidate-pool size)."
        ));
    }
    Ok(())
}

/// Derive the default rerank instruction from request shape (spec §3).
///
/// `code_exclusive` is `code_mode == exclusive`; `version` is the first
/// `language_target` filter's `(name, version_satisfies)` when both are
/// present. Deliberately minimal: every default token is multiplied by ~50
/// docs per search. Agent-supplied instructions replace this wholesale (D4).
#[must_use]
pub fn default_instruction(code_exclusive: bool, version: Option<(&str, &str)>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if code_exclusive {
        parts.push(
            "Prioritize chunks containing code examples, function signatures, and API usage \
             over prose."
                .to_owned(),
        );
    }
    if let Some((name, ver)) = version {
        parts.push(format!(
            "Prefer content applying to {name} version {ver}; deprioritize other versions."
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Compose the query text sent to Voyage `/v1/rerank`: the instruction (when
/// present and non-blank) is appended to the query on a labelled second line —
/// Voyage's documented convention is natural-language instructions appended or
/// prepended to the query string (instructions are NOT an API parameter).
#[must_use]
pub fn compose_rerank_query(query: &str, instruction: Option<&str>) -> String {
    match instruction.map(str::trim) {
        Some(i) if !i.is_empty() => format!("{query}\nInstructions: {i}"),
        _ => query.to_owned(),
    }
}
```

Add `pub mod rerank;` to `crates/mn-core/src/lib.rs` next to `pub mod scoring;`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p mn-core rerank`
Expected: all 6 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-core/src/rerank.rs crates/mn-core/src/lib.rs
git commit -m "feat(mn-core): rerank vocabulary — RerankParam, billed-token math, instruction derivation"
```

---

### Task 2: `mn_core::config` — `[rerank]` client config + placement resolution (additive)

The old `[models].reranker` / `resolve_reranker` are removed later (Task 8), after CLI/MCP migrate.

**Files:**
- Modify: `crates/mn-core/src/config.rs`

- [ ] **Step 1: Write the failing tests** (append to the existing `tests` module in `config.rs`, which already has a `FakeEnv` helper — see the `resolve_reranker_prefers_flag_then_env_then_config` test around line 458 for the pattern)

```rust
#[test]
fn rerank_config_parses_from_toml() {
    let toml = r#"
[rerank]
location = "server"
model = "rerank-2.5-lite"
"#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert_eq!(cfg.rerank.location.as_deref(), Some("server"));
    assert_eq!(cfg.rerank.model.as_deref(), Some("rerank-2.5-lite"));
    // Absent section -> defaults (both None).
    let cfg: Config = toml::from_str("").unwrap();
    assert!(cfg.rerank.location.is_none() && cfg.rerank.model.is_none());
}

#[test]
fn resolve_rerank_placement_precedence_and_auto() {
    let cfg = RerankConfig { location: Some("off".into()), model: None };
    let env = FakeEnv::default().set("MIDNIGHT_MANUAL_RERANK", "server");
    // flag > env > config.
    assert_eq!(resolve_rerank_placement(Some("local"), &cfg, &env, false), RerankPlacement::Local);
    assert_eq!(resolve_rerank_placement(None, &cfg, &env, true), RerankPlacement::Server);
    let no_env = FakeEnv::default();
    assert_eq!(resolve_rerank_placement(None, &cfg, &no_env, true), RerankPlacement::Off);
    // Auto everywhere -> key detection: key => local, no key => server.
    let empty = RerankConfig::default();
    assert_eq!(resolve_rerank_placement(None, &empty, &no_env, true), RerankPlacement::Local);
    assert_eq!(resolve_rerank_placement(None, &empty, &no_env, false), RerankPlacement::Server);
    // Explicit "auto" at any level falls through to key detection.
    assert_eq!(resolve_rerank_placement(Some("auto"), &empty, &no_env, false), RerankPlacement::Server);
    // Unknown value falls through to the next level (lenient, like other resolvers).
    assert_eq!(resolve_rerank_placement(Some("bogus"), &empty, &no_env, true), RerankPlacement::Local);
}

#[test]
fn resolve_rerank_model_precedence_and_default() {
    use crate::rerank::RerankParam;
    let cfg = RerankConfig { location: None, model: Some("rerank-2.5-lite".into()) };
    let env = FakeEnv::default().set("MIDNIGHT_MANUAL_RERANK_MODEL", "rerank-2.5");
    assert_eq!(resolve_rerank_model(Some("rerank-2.5-lite"), &cfg, &env), RerankParam::Rerank25Lite);
    assert_eq!(resolve_rerank_model(None, &cfg, &env), RerankParam::Rerank25);
    let no_env = FakeEnv::default();
    assert_eq!(resolve_rerank_model(None, &cfg, &no_env), RerankParam::Rerank25Lite);
    // Nothing anywhere -> rerank-2.5; unknown strings fall through.
    assert_eq!(resolve_rerank_model(None, &RerankConfig::default(), &no_env), RerankParam::Rerank25);
    assert_eq!(resolve_rerank_model(Some("bogus"), &RerankConfig::default(), &no_env), RerankParam::Rerank25);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mn-core config`
Expected: compile FAIL — `RerankConfig` / `RerankPlacement` not defined.

- [ ] **Step 3: Implement.** Add next to `ModelsConfig` (match its serde style — the `Config` struct has `#[serde(default)]` per section; add a `pub rerank: RerankConfig` field to `Config` with `#[serde(default)]`):

```rust
/// `[rerank]` — client-side rerank placement and model selection (spec §4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct RerankConfig {
    /// Where reranking runs: `"auto"` (default) | `"local"` | `"server"` | `"off"`.
    pub location: Option<String>,
    /// Voyage rerank model: `"rerank-2.5"` (default) | `"rerank-2.5-lite"`.
    pub model: Option<String>,
}

/// Where a client runs reranking after placement resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankPlacement {
    /// Call Voyage directly with the user's own key; tell the server `none`.
    Local,
    /// Ask the server to rerank inline in `/v1/search`.
    Server,
    /// No reranking anywhere; tell the server `none`.
    Off,
}

/// Resolve rerank placement: flag > `MIDNIGHT_MANUAL_RERANK` env > config
/// `[rerank].location` > auto. Auto (and any unrecognized value) resolves by
/// key detection: a Voyage key present means local BYOK, absent means server
/// (D6). Mirrors the embedding-path defaulting.
#[must_use]
pub fn resolve_rerank_placement(
    flag: Option<&str>,
    cfg: &RerankConfig,
    env: &impl ConfigEnv,
    has_voyage_key: bool,
) -> RerankPlacement {
    let explicit = |s: &str| match s {
        "local" => Some(RerankPlacement::Local),
        "server" => Some(RerankPlacement::Server),
        "off" => Some(RerankPlacement::Off),
        _ => None, // "auto" or unknown: fall through
    };
    flag.and_then(explicit)
        .or_else(|| env.var("MIDNIGHT_MANUAL_RERANK").as_deref().and_then(explicit))
        .or_else(|| cfg.location.as_deref().and_then(explicit))
        .unwrap_or(if has_voyage_key {
            RerankPlacement::Local
        } else {
            RerankPlacement::Server
        })
}

/// Resolve the rerank model: flag > `MIDNIGHT_MANUAL_RERANK_MODEL` env >
/// config `[rerank].model` > `rerank-2.5`. Returns a model variant only —
/// never [`crate::rerank::RerankParam::None`] (placement handles "off").
#[must_use]
pub fn resolve_rerank_model(
    flag: Option<&str>,
    cfg: &RerankConfig,
    env: &impl ConfigEnv,
) -> crate::rerank::RerankParam {
    use crate::rerank::RerankParam;
    let parse = |s: &str| match s {
        "rerank-2.5" => Some(RerankParam::Rerank25),
        "rerank-2.5-lite" => Some(RerankParam::Rerank25Lite),
        _ => None,
    };
    flag.and_then(parse)
        .or_else(|| env.var("MIDNIGHT_MANUAL_RERANK_MODEL").as_deref().and_then(parse))
        .or_else(|| cfg.model.as_deref().and_then(parse))
        .unwrap_or(RerankParam::Rerank25)
}
```

Note: if `FakeEnv::set` filters empty strings differently, mirror how `resolve_reranker` treats empties (`.filter(|s| !s.is_empty())` on env reads) — check the existing resolver at config.rs:254 and copy its empty-string handling into both resolvers.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p mn-core config`
Expected: PASS (new tests + all pre-existing config tests).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-core/src/config.rs
git commit -m "feat(mn-core): [rerank] config section + placement/model resolution"
```

---

### Task 3: Server config — kill switch + Voyage base-url override

**Files:**
- Modify: `crates/mn-server/src/config.rs` (struct `ServerConfig` ~line 13; follow the exact doc-comment + env-parsing pattern of `rate_limit_enabled` / `voyage_api_key` — read the `from_env`/builder section before editing)

- [ ] **Step 1: Add two fields to `ServerConfig`** (doc style mirrors siblings):

```rust
/// `MIDNIGHT_MANUAL_SERVER_RERANK` — master switch for inline server-side
/// reranking in `POST /v1/search` (spec §1 ops kill switch). `"off"`
/// disables (searches degrade to RRF order with
/// `search_metadata.rerank.reason = "disabled"`); anything else (or unset)
/// enables. Default `true`.
pub server_rerank_enabled: bool,
/// `MIDNIGHT_MANUAL_VOYAGE_BASE_URL` — override the VoyageAI API base URL
/// (tests point this at a wiremock; unset in production).
pub voyage_base_url: Option<String>,
```

Wire both in the same place the other `MIDNIGHT_MANUAL_*` vars are read (the config constructor / `from_env`): `server_rerank_enabled` is `env != Some("off")` — use the same bool-parsing helper the file already uses for `rate_limit_enabled` if one exists, otherwise:

```rust
server_rerank_enabled: std::env::var("MIDNIGHT_MANUAL_SERVER_RERANK")
    .map(|v| v != "off")
    .unwrap_or(true),
voyage_base_url: std::env::var("MIDNIGHT_MANUAL_VOYAGE_BASE_URL").ok().filter(|s| !s.is_empty()),
```

(If `ServerConfig` is built by a test-visible `Default`/builder used in `tests/common`, set `server_rerank_enabled: true`, `voyage_base_url: None` there too.)

- [ ] **Step 2: Build**

Run: `cargo build -p mn-server`
Expected: compiles (struct-literal sites in tests may need the new fields — fix them).

- [ ] **Step 3: Commit**

```bash
git add crates/mn-server/src/config.rs crates/mn-server/tests
git commit -m "feat(mn-server): config for rerank kill switch + Voyage base-url override"
```

---

### Task 4: Server — inline rerank stage in `POST /v1/search`

**Files:**
- Modify: `crates/mn-server/src/routes/search.rs`
- Create: `crates/mn-server/tests/search_rerank.rs` (integration; CI-verified)

- [ ] **Step 1: Write failing unit tests** (append to `search.rs`'s `#[cfg(test)] mod tests` if one exists, else create one at the bottom of the file):

```rust
#[cfg(test)]
mod rerank_tests {
    use super::*;

    #[test]
    fn rerank_pool_is_max_of_limit_and_floor() {
        assert_eq!(rerank_pool_size(10), 50);
        assert_eq!(rerank_pool_size(50), 50);
        assert_eq!(rerank_pool_size(80), 80);
    }

    #[test]
    fn rerank_token_estimate_multiplies_query_by_docs() {
        // 8-char query ≈ 2 tokens; two 4-char docs ≈ 1 token each.
        // (2 × 2) + (1 + 1) = 6.
        let docs = vec!["aaaa".to_owned(), "bbbb".to_owned()];
        assert_eq!(rerank_token_estimate("qqqqqqqq", &docs), 6);
        // Empty docs -> 0 (no Voyage call would be made anyway).
        assert_eq!(rerank_token_estimate("qqqqqqqq", &[]), 0);
    }

    #[test]
    fn rerank_metadata_serializes_per_spec() {
        let applied = RerankMetadata {
            applied: true,
            model: Some("rerank-2.5"),
            reason: None,
        };
        let v = serde_json::to_value(&applied).unwrap();
        assert_eq!(v, serde_json::json!({"applied": true, "model": "rerank-2.5"}));

        let degraded = RerankMetadata {
            applied: false,
            model: Some("rerank-2.5-lite"),
            reason: Some("token_budget_exhausted"),
        };
        let v = serde_json::to_value(&degraded).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "applied": false, "model": "rerank-2.5-lite",
                "reason": "token_budget_exhausted"
            })
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mn-server rerank`
Expected: compile FAIL.

- [ ] **Step 3: Implement the request/response surface.** In `SearchRequest` (after `code_vector`, search.rs:89):

```rust
/// Server-side rerank model, or `"none"` to skip (spec §1). Omitted ⇒
/// `rerank-2.5`. Clients that rerank locally always send `"none"`.
#[serde(default)]
pub rerank: Option<mn_core::rerank::RerankParam>,
/// Optional natural-language rerank instruction (≤400 chars; replaces the
/// derived default wholesale, D4). Ignored when rerank is `"none"`.
#[serde(default)]
pub rerank_instructions: Option<String>,
```

New types + helpers (near `SearchMetadata`):

```rust
/// Outcome of the rerank stage, reported on every response (spec §1).
#[derive(Debug, Serialize)]
pub struct RerankMetadata {
    /// Whether a Voyage rerank was applied to this result set.
    pub applied: bool,
    /// The model attempted/applied; absent when rerank was `"none"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'static str>,
    /// Why rerank was not applied: `not_requested` | `token_budget_exhausted`
    /// | `provider_error` | `disabled`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

/// Server-side rerank candidate-pool floor (mirrors the clients' RERANK_FETCH).
const RERANK_POOL: u32 = 50;

/// Pool size: at least [`RERANK_POOL`], never below the caller's `limit`.
const fn rerank_pool_size(limit: u32) -> u32 {
    if limit > RERANK_POOL { limit } else { RERANK_POOL }
}

/// Pre-gate estimate of a rerank's token cost, in Voyage's formula
/// `(query_tokens × num_documents) + sum(document_tokens)`, using the same
/// ~4-bytes/token heuristic as the embeddings route. The reservation is
/// settled against Voyage's reported count, so slack here only affects
/// in-flight gating, never the final balance.
fn rerank_token_estimate(query: &str, docs: &[String]) -> u64 {
    if docs.is_empty() {
        return 0;
    }
    let est = |s: &str| (s.len() as u64).div_ceil(4).max(1);
    est(query) * docs.len() as u64 + docs.iter().map(|d| est(d)).sum::<u64>()
}
```

Add `pub rerank: RerankMetadata` to `SearchMetadata` (with a doc comment), and add `rerank_score` to the score surface:

- `ScoreBreakdown`: add field
  ```rust
  /// Voyage relevance score in [0, 1]; present only when the server reranked.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub rerank_score: Option<f64>,
  ```
- `ScoredCandidate`: add `rerank_score: Option<f64>` (init `None` at the construction site in the scoring loop) and pass it through in `into_result()`.

- [ ] **Step 4: Implement the rerank stage.** Handler signature gains header/auth extractors (body extractor stays last):

```rust
async fn search(
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
    rl: Option<Extension<RateLimitContext>>,
    headers: axum::http::HeaderMap,
    auth: Option<Extension<crate::middleware::bearer::AuthContext>>,
    Json(req): Json<SearchRequest>,
) -> Response {
```

Early validation (with the other 400 guards, before any retrieval work):

```rust
// Instruction cap (spec §1): reject, never truncate silently.
if let Some(instr) = req.rerank_instructions.as_deref() {
    if let Err(msg) = mn_core::rerank::validate_instruction(instr) {
        return error::into_response(
            CoreError::builder(ErrorCode::InvalidRequest)
                .message(msg)
                .remediation("shorten rerank_instructions to 400 characters or fewer")
                .build(),
            rid,
        );
    }
}
```

Insert the stage between the scoring loop and the `min_confidence` filter (currently search.rs:776) — pool selection happens on relevance (RRF score) order, dedup runs **before** the Voyage call so tokens aren't spent on overlapping chunks, and `min_confidence`/`sort_by`/truncate then operate on recomputed confidences:

```rust
// ---- Rerank stage (spec §1). Degrades, never fails search. ----
let rerank_param = req.rerank.unwrap_or_default();
let rerank_meta = if let Some(model) = rerank_param.model_name() {
    // Pool by relevance, dedup overlaps first (don't pay Voyage for dupes),
    // then keep the top max(limit, 50).
    sort_candidates(&mut scored, SortBy::Score);
    let dedup_stats_early;
    (scored, dedup_stats_early) = if dedup_enabled() {
        mn_retrieval::dedup::trim_overlaps(scored)
    } else {
        (scored, mn_retrieval::dedup::DedupStats::default())
    };
    scored.truncate(rerank_pool_size(limit) as usize);
    rerank_stage(&state, &req, &queries, &headers, auth.as_ref().map(|Extension(c)| c), model, rerank_param, &mut scored, rid).await
        // dedup already ran for this path; remember its stats
        .with_dedup(dedup_stats_early)
} else {
    RerankOutcome::not_requested()
};
```

…where `rerank_stage` is a new private async fn. To keep the handler readable AND keep the no-rerank path byte-identical, implement it like this (complete function):

```rust
/// Everything the rerank stage produced: the metadata for the response plus
/// (for the rerank path) the dedup stats already accounted.
struct RerankOutcome {
    meta: RerankMetadata,
    /// `Some` when the rerank path ran dedup early (so the main flow must skip
    /// its own dedup pass); `None` on the not-requested path.
    dedup: Option<mn_retrieval::dedup::DedupStats>,
}

impl RerankOutcome {
    fn not_requested() -> Self {
        Self {
            meta: RerankMetadata { applied: false, model: None, reason: Some("not_requested") },
            dedup: None,
        }
    }
    fn with_dedup(mut self, stats: mn_retrieval::dedup::DedupStats) -> Self {
        self.dedup = Some(stats);
        self
    }
}

/// Run the Voyage rerank over the pooled candidates, charging billed-equivalent
/// tokens to the caller's windows + the global cap. Mutates `scored` in place
/// (relevance, confidence, factors, rerank_score). Every failure path degrades
/// to RRF order with a `reason` — a flaky upstream or an empty budget never
/// fails the search (spec D3).
#[allow(clippy::too_many_arguments)]
async fn rerank_stage(
    state: &AppState,
    req: &SearchRequest,
    queries: &[QueryPair],
    headers: &axum::http::HeaderMap,
    auth: Option<&crate::middleware::bearer::AuthContext>,
    model: &'static str,
    param: mn_core::rerank::RerankParam,
    scored: &mut [ScoredCandidate],
    rid: &str,
) -> RerankOutcome {
    let degraded = |reason: &'static str| RerankOutcome {
        meta: RerankMetadata { applied: false, model: Some(model), reason: Some(reason) },
        dedup: None,
    };

    // Kill switch / no platform key -> disabled.
    if !state.cfg.server_rerank_enabled {
        return degraded("disabled");
    }
    let Some(key) = state.cfg.voyage_api_key.as_deref() else {
        return degraded("disabled");
    };
    if scored.is_empty() {
        // Nothing to rerank; report applied (a no-op rerank is not a failure).
        return RerankOutcome {
            meta: RerankMetadata { applied: true, model: Some(model), reason: None },
            dedup: None,
        };
    }

    // Compose the rerank query: first query text + (agent instruction, else
    // derived default per spec §3).
    let pivot = queries.first().map(|q| q.text.as_str()).unwrap_or_default();
    let derived;
    let instruction: Option<&str> = match req.rerank_instructions.as_deref() {
        Some(i) => Some(i),
        None => {
            let code_exclusive = matches!(req.code_mode, Some(CodeMode::Exclusive));
            let version = req.filters.language_target.any_of.first().and_then(|lt| {
                lt.version_satisfies.as_deref().map(|v| (lt.name.as_str(), v))
            });
            derived = mn_core::rerank::default_instruction(code_exclusive, version);
            derived.as_deref()
        }
    };
    let composed = mn_core::rerank::compose_rerank_query(pivot, instruction);
    let docs: Vec<String> = scored.iter().map(|c| c.content.clone()).collect();

    // Gate-then-charge against the shared Voyage token budget (spec §2).
    let client_ip = crate::middleware::rate_limit::client_ip(
        headers,
        &state.cfg.rate_limit_client_ip_header,
    );
    let (subject, _tier, limits) = state.token_limiter.resolve(&client_ip, auth);
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let estimate = param.billed_tokens(rerank_token_estimate(&composed, &docs));
    let reservation = match state.token_limiter.reserve(&subject, limits, estimate, now, false) {
        Ok(id) => id,
        Err(_) => return degraded("token_budget_exhausted"),
    };

    // Call Voyage. Errors release the reservation and degrade.
    let mut reranker = mn_embedding::voyage::VoyageReranker::new(key, model);
    if let Some(base) = state.cfg.voyage_base_url.as_deref() {
        reranker = reranker.with_base_url(base);
    }
    let out = match reranker.rerank(composed, docs, None).await {
        Ok(o) => o,
        Err(e) => {
            state.token_limiter.release(&subject, reservation, false);
            tracing::warn!(request_id = rid, error = %e, "voyage rerank failed; degrading");
            return degraded("provider_error");
        }
    };
    let billed = param.billed_tokens(out.total_tokens);
    state.token_limiter.settle(&subject, reservation, billed, now, false);
    metrics::counter!("rerank_billed_tokens_total").increment(billed);

    // Rescore in place: Voyage relevance_score is already in [0, 1] — used
    // directly, no sigmoid. Indices refer into the pool (== `scored`) order.
    for s in &out.results {
        let Some(c) = scored.get_mut(s.index) else { continue };
        let relevance = f64::from(s.score).clamp(0.0, 1.0);
        c.relevance = relevance;
        c.rerank_score = Some(f64::from(s.score));
        c.score.confidence = state.scoring_policy.confidence(c.score.trust_score, relevance);
        c.score.factors.relevance_source = RelevanceSource::Rerank;
        c.score.factors.relevance_multiplier = relevance;
    }
    RerankOutcome {
        meta: RerankMetadata { applied: true, model: Some(model), reason: None },
        dedup: None,
    }
}
```

Then adjust the tail of the handler: the existing `min_confidence` retain / `sort_candidates(&mut scored, req.sort_by)` / dedup / truncate flow stays, EXCEPT dedup is skipped when `rerank_meta.dedup` is `Some` (it already ran pre-Voyage) — use its stats for `overlap_dropped_count`/`overlap_trimmed_count`. Emit metrics next to the response build:

```rust
if rerank_meta.meta.applied {
    metrics::counter!("rerank_applied_total").increment(1);
} else if let Some(reason) = rerank_meta.meta.reason.filter(|r| *r != "not_requested") {
    metrics::counter!("rerank_degraded_total", "reason" => reason).increment(1);
}
```

and add `rerank: rerank_meta.meta` to the `SearchMetadata` literal. Also note Borrow-checker detail: `rerank_stage` takes `&mut [ScoredCandidate]` post-truncate; restructure the handler so pool sort/dedup/truncate happen inline (as sketched in the call site above) — adjust until clean, keeping the **non-rerank path's behavior and wire output identical except for the new `rerank` metadata object**. Update the module-header doc comment (line 8) — reranking no longer "lands in later phases."

- [ ] **Step 5: Run unit tests**

Run: `cargo test -p mn-server rerank && cargo build -p mn-server`
Expected: PASS / compiles.

- [ ] **Step 6: Integration test** — create `crates/mn-server/tests/search_rerank.rs`, modeled on `tests/search_route.rs` (app boot + corpus seeding via `tests/common` + `fixtures.rs`) and `tests/code_ingest_e2e.rs` (`voyage_mock()` wiremock pattern, ~line 112). Cover four scenarios:

```rust
//! POST /v1/search inline rerank (spec §1–2): applied path, token-budget
//! degrade, provider-error degrade, rerank=none passthrough.
//
// Uses the same gating/harness as search_route.rs (CI-only; needs Postgres).

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mock Voyage /v1/rerank: reverses the document order with descending scores
/// and reports 1000 total_tokens.
async fn rerank_mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let n = body["documents"].as_array().unwrap().len();
            let data: Vec<serde_json::Value> = (0..n)
                .map(|i| serde_json::json!({
                    "index": n - 1 - i,
                    "relevance_score": 0.9 - (i as f64) * 0.1
                }))
                .collect();
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data": data, "usage": {"total_tokens": 1000}}))
        })
        .mount(&server)
        .await;
    server
}

// Test 1 (applied): boot the app with cfg.voyage_api_key = Some("test-key"),
// cfg.voyage_base_url = Some(mock.uri()), seed ≥3 chunks (reuse the
// search_route.rs seeding helpers), POST /v1/search with
// {"query": ..., "vector": ..., "client_embedding_model": ..., "rerank": "rerank-2.5"}.
// Assert: search_metadata.rerank == {"applied": true, "model": "rerank-2.5"};
// results carry scores.rerank_score; confidence_factors.relevance_source == "rerank";
// ordering follows the mock's reversed scores (the last-seeded chunk wins).

// Test 2 (budget degrade): same app but token limits configured to a tiny
// ceiling (see tests/admin_tokenlimits.rs / token_snapshot_roundtrip.rs for how
// limits are set in tests). Assert HTTP 200, results non-empty in RRF order,
// rerank == {"applied": false, "model": "rerank-2.5", "reason": "token_budget_exhausted"},
// and the wiremock received ZERO /v1/rerank requests (mock.received_requests()).

// Test 3 (provider error): mount the mock to respond 500. Assert 200, RRF
// order, reason == "provider_error", and the reservation was released — a
// follow-up GET /v1/me (or a second search) shows the budget NOT debited.

// Test 4 (none passthrough): send "rerank": "none". Assert
// rerank == {"applied": false, "reason": "not_requested"}, no rerank_score
// fields, zero mock requests. Also assert "rerank-2.5-lite" with the mock
// debits ceil(1000/2) == 500 tokens (read the budget via /v1/me before/after).
```

Write all four tests fully against the actual helper names found in `tests/search_route.rs` / `tests/common/` — the seeding/boot APIs there are the source of truth; the comments above define the assertions each must make.

- [ ] **Step 7: Compile integration tests, then run unit suite**

Run: `cargo test -p mn-server --no-run` (with whatever feature flag sibling integration tests use, e.g. `--features integration`) and `cargo test -p mn-server`
Expected: integration tests compile (CI runs them); unit tests PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/mn-server/src/routes/search.rs crates/mn-server/tests/search_rerank.rs
git commit -m "feat(mn-server): inline Voyage rerank in /v1/search with token accounting + degrade-and-flag"
```

---

### Task 5: Contracts — openapi.yaml, mcp-tools.json, contract sync

**Files:**
- Modify: `specs/001-rag-platform/contracts/openapi.yaml` (search request/response schemas)
- Modify: `specs/001-rag-platform/contracts/mcp-tools.json` (`advanced_search` input schema, ~line 616; `search`/`advanced_search`/`status` descriptions referencing "cross-encoder")
- Modify: `crates/mn-mcp/src/tools.rs` (the inline tool manifest, ~lines 56/64/173/288 — must stay byte-identical with mcp-tools.json per `crates/mn-mcp/tests/contract_sync.rs`)

- [ ] **Step 1: openapi.yaml.** In the `/v1/search` request schema add:

```yaml
rerank:
  type: string
  enum: [rerank-2.5, rerank-2.5-lite, none]
  default: rerank-2.5
  description: >-
    Server-side rerank model, or "none" to skip. Tokens are charged to the
    caller's token budget (rerank-2.5-lite at half rate, rounded up).
rerank_instructions:
  type: string
  maxLength: 400
  description: >-
    Optional natural-language rerank instruction appended to the query;
    replaces the derived default. Ignored when rerank is "none".
```

In `search_metadata` add:

```yaml
rerank:
  type: object
  required: [applied]
  properties:
    applied: { type: boolean }
    model: { type: string, enum: [rerank-2.5, rerank-2.5-lite] }
    reason:
      type: string
      enum: [not_requested, token_budget_exhausted, provider_error, disabled]
```

and add `rerank_score: { type: number }` to the per-result `scores` schema. Match the file's existing indentation/style exactly.

- [ ] **Step 2: mcp-tools.json + the mirrored manifest in `mn-mcp/src/tools.rs`.** In `advanced_search.inputSchema.properties`, alongside the existing `rerank` boolean (which keeps its shape), add:

```json
"rerank_instructions": {
  "type": "string",
  "maxLength": 400,
  "description": "Optional rerank instruction (max 400 chars). Guides relevance: emphasize aspects, filter document kinds, or disambiguate terms. Replaces the derived default instruction. Keep it terse — instruction tokens are multiplied by the candidate-pool size. See the midnight-advanced-search skill for guidance."
}
```

Update the `rerank` boolean's description (both files, identical bytes):
`"Apply VoyageAI reranking against the first query (server-side, or locally with your own VOYAGE_API_KEY). Disable for lowest latency."`
Update the three descriptions that say "cross-encoder"/"reranker readiness" (`search` ~line 56 mentions advanced_search for rerank control — fine; `advanced_search` ~line 64 keep; `status` ~line 173: change "reranker readiness" to "rerank configuration") — whatever you change, change in BOTH files identically.

- [ ] **Step 3: Run the contract sync test**

Run: `cargo test -p mn-mcp --test contract_sync`
Expected: PASS (it diff-checks tools.rs against mcp-tools.json).

- [ ] **Step 4: Commit**

```bash
git add specs/001-rag-platform/contracts/ crates/mn-mcp/src/tools.rs
git commit -m "feat(contracts): rerank + rerank_instructions on /v1/search and advanced_search"
```

---

### Task 6: CLI — placement flags, local Voyage rerank, server param

**Files:**
- Modify: `crates/mn-cli/src/commands/search.rs`
- Check/Modify: wherever `Args` help text is asserted (grep `mn-cli` tests for `--rerank`)

- [ ] **Step 1: Write failing unit tests** (append to the existing tests module in `commands/search.rs`; it already unit-tests `apply_rerank` / `build_search_request` — follow that style):

```rust
#[test]
fn build_search_request_rerank_wire_matrix() {
    use mn_core::config::RerankPlacement;
    use mn_core::rerank::RerankParam;
    let base = || SearchRequestParts {
        queries: vec![QueryPair { text: "q".into(), vector: vec![], code_vector: vec![] }],
        client_embedding_model: "m@1".into(),
        client_code_embedding_model: None,
        limit: 10,
        placement: RerankPlacement::Server,
        rerank_model: RerankParam::Rerank25,
        rerank_instructions: None,
        mode: "hybrid".into(),
        code_mode: None,
        filters: SearchFilters::default(),
    };

    // Server placement: server reranks; caller's limit goes on the wire.
    let r = build_search_request(base());
    assert_eq!(r.rerank.as_deref(), Some("rerank-2.5"));
    assert_eq!(r.limit, 10);
    assert!(r.sort_by.is_none());

    // Local placement: tell the server "none", over-fetch, sort by score.
    let mut p = base();
    p.placement = RerankPlacement::Local;
    let r = build_search_request(p);
    assert_eq!(r.rerank.as_deref(), Some("none"));
    assert_eq!(r.limit, RERANK_FETCH);
    assert_eq!(r.sort_by.as_deref(), Some("score"));

    // Off: "none", caller's limit, no over-fetch.
    let mut p = base();
    p.placement = RerankPlacement::Off;
    let r = build_search_request(p);
    assert_eq!(r.rerank.as_deref(), Some("none"));
    assert_eq!(r.limit, 10);

    // Lite model + instructions ride along on the server path.
    let mut p = base();
    p.rerank_model = RerankParam::Rerank25Lite;
    p.rerank_instructions = Some("Prefer prose.".into());
    let r = build_search_request(p);
    assert_eq!(r.rerank.as_deref(), Some("rerank-2.5-lite"));
    assert_eq!(r.rerank_instructions.as_deref(), Some("Prefer prose."));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `VOYAGE_API_KEY= cargo test -p mn-cli rerank_wire`
Expected: compile FAIL.

- [ ] **Step 3: Implement.**

In `Args` — replace the `rerank: bool` and `reranker: Option<String>` fields (lines 107–119) with:

```rust
/// Where reranking runs: auto (default; local with a Voyage key, else
/// server), local (BYOK Voyage), server, or off.
#[arg(long, default_value = "auto", value_parser = ["auto", "local", "server", "off"])]
pub rerank: String,

/// Voyage rerank model. rerank-2.5-lite is faster and billed at half rate
/// server-side. Precedence: this flag > MIDNIGHT_MANUAL_RERANK_MODEL env >
/// config [rerank].model.
#[arg(long = "rerank-model", value_parser = ["rerank-2.5", "rerank-2.5-lite"])]
pub rerank_model: Option<String>,

/// Natural-language rerank instruction (max 400 chars). Replaces the
/// derived default. Keep terse — instruction tokens multiply by pool size.
#[arg(long = "rerank-instructions")]
pub rerank_instructions: Option<String>,
```

In `run_with_paths`: resolve placement + model and validate the instruction early (fail fast before embedding):

```rust
let placement = mn_core::config::resolve_rerank_placement(
    (args.rerank != "auto").then_some(args.rerank.as_str()),
    &cfg.rerank,
    &env,
    voyage_key.is_some(),
);
let rerank_model = mn_core::config::resolve_rerank_model(
    args.rerank_model.as_deref(),
    &cfg.rerank,
    &env,
);
if let Some(i) = args.rerank_instructions.as_deref() {
    mn_core::rerank::validate_instruction(i).map_err(|e| anyhow!(e))?;
}
// Local placement requires a key: tell the user instead of silently degrading.
if matches!(placement, mn_core::config::RerankPlacement::Local) && voyage_key.is_none() {
    anyhow::bail!(
        "--rerank local needs a Voyage API key (--voyage-api-key, VOYAGE_API_KEY, or config)"
    );
}
```

`SearchRequestParts`: replace `rerank: bool` with `placement: RerankPlacement`, `rerank_model: RerankParam`, `rerank_instructions: Option<String>`. `build_search_request` becomes:

```rust
fn build_search_request(parts: SearchRequestParts) -> SearchRequest {
    use mn_core::config::RerankPlacement;
    let local = matches!(parts.placement, RerankPlacement::Local);
    let (cloud_limit, sort_by) = if local {
        (RERANK_FETCH, Some("score".to_owned()))
    } else {
        (parts.limit, None)
    };
    let (rerank, rerank_instructions) = match parts.placement {
        RerankPlacement::Server => (
            parts.rerank_model.model_name().map(str::to_owned),
            parts.rerank_instructions.clone(),
        ),
        // Local reranks client-side; Off skips. Both tell the server "none"
        // (exactly one rerank pass, structurally).
        RerankPlacement::Local | RerankPlacement::Off => (Some("none".to_owned()), None),
    };
    SearchRequest { queries: parts.queries, client_embedding_model: parts.client_embedding_model,
        client_code_embedding_model: parts.client_code_embedding_model, limit: cloud_limit,
        mode: parts.mode, code_mode: parts.code_mode, filters: parts.filters, sort_by,
        rerank, rerank_instructions }
}
```

Add to the CLI's serialize-side `SearchRequest` struct:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
rerank: Option<String>,
#[serde(skip_serializing_if = "Option::is_none")]
rerank_instructions: Option<String>,
```

Local rerank path — replace `RerankSelection`/catalog/`LoadedReranker` in `rerank_via_http` with the direct Voyage client (signature: pass `voyage_key: &str`, `voyage_base_url: Option<&str>`, `model: &'static str`, `instruction: Option<&str>`; delete the `reranker_id`/`reranker_path`/`cache_dir` plumbing):

```rust
let mut reranker = mn_embedding::voyage::VoyageReranker::new(voyage_key, model);
if let Some(base) = voyage_base_url {
    reranker = reranker.with_base_url(base);
}
let pivot = texts.first().cloned().unwrap_or_default();
// Agent instruction wins; else the same derived default the server uses
// (code_mode exclusive / version filter), so placement doesn't change results.
let derived;
let instr = match instruction {
    Some(i) => Some(i),
    None => {
        let code_exclusive = request.code_mode.as_deref() == Some("exclusive");
        let version = request.filters.language_target.any_of.first().and_then(|lt| {
            lt.version_satisfies.as_deref().map(|v| (lt.name.as_str(), v))
        });
        derived = mn_core::rerank::default_instruction(code_exclusive, version);
        derived.as_deref()
    }
};
let composed = mn_core::rerank::compose_rerank_query(&pivot, instr);
let docs: Vec<String> = resp.results.iter().map(|r| r.content.clone()).collect();
let out = reranker.rerank(composed, docs, None).await.context("voyage rerank")?;
let reordered = apply_rerank(resp.results, &out.results, limit);
```

(`apply_rerank` and `stamp_rerank_score` are unchanged — the stamped score is now a 0–1 relevance, which is what we want. Update their doc comments: "raw reranker logit" → "Voyage relevance score (0–1)".) `dispatch_search` routes on placement: `Local` → rerank path, `Server`/`Off` → plain `search_via_http`. Update the module header doc (lines 33–38) and `DispatchSearch` fields accordingly. Remove the now-unused `resolve_reranker`/`reranker_path`/`cache_dir` plumbing from this file (the global deletion is Task 8).

- [ ] **Step 4: Run tests**

Run: `VOYAGE_API_KEY= cargo test -p mn-cli`
Expected: PASS (note: the 2 `auth_integration` loopback failures are pre-existing sandbox noise — ignore exactly those).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-cli
git commit -m "feat(mn-cli): rerank placement flags (auto/local/server/off) + local Voyage rerank"
```

---

### Task 7: MCP — placement resolution, `rerank_instructions`, sigmoid fix

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`
- Modify: wherever `ParsedSearchArgs` is defined/parsed (same file; grep `parse_advanced_search_args`)
- Modify: the MCP-side cloud `SearchRequest` struct (grep `struct SearchRequest` in `mn-mcp`)

- [ ] **Step 1: Write failing unit tests** (append near the existing `rerank_postprocess` tests):

```rust
#[test]
fn rerank_postprocess_uses_voyage_scores_directly() {
    // Voyage relevance_score is already 0–1: NO sigmoid. trust=1.0 and the
    // default policy make confidence == relevance^relevance_weight; the key
    // assertion is relevance_multiplier == the raw score, not sigmoid(score).
    let results = vec![
        serde_json::json!({"content": "a", "scores": {"trust_score": 1.0,
            "confidence": 0.5, "confidence_factors": {"relevance_source": "rrf",
            "relevance_multiplier": 0.5}}}),
        serde_json::json!({"content": "b", "scores": {"trust_score": 1.0,
            "confidence": 0.5, "confidence_factors": {"relevance_source": "rrf",
            "relevance_multiplier": 0.5}}}),
    ];
    let scores = vec![
        RerankResult { index: 1, score: 0.9 },
        RerankResult { index: 0, score: 0.2 },
    ];
    let out = rerank_postprocess(results, &scores, 10);
    assert_eq!(out[0]["content"], "b"); // 0.9 outranks 0.2
    let f = &out[0]["scores"]["confidence_factors"];
    assert_eq!(f["relevance_source"], "rerank");
    let rm = f["relevance_multiplier"].as_f64().unwrap();
    assert!((rm - 0.9).abs() < 1e-9, "expected raw 0.9, got {rm} (sigmoid bug?)");
    assert!((out[0]["rerank_score"].as_f64().unwrap() - 0.9).abs() < 1e-9);
}

#[test]
fn advanced_search_parses_rerank_instructions_and_caps_length() {
    // Use the real parser; arg shape mirrors the manifest schema.
    let ok = serde_json::json!({"queries": ["q"], "rerank_instructions": "Prefer code."});
    let parsed = parse_advanced_search_args(&ok).unwrap();
    assert_eq!(parsed.rerank_instructions.as_deref(), Some("Prefer code."));

    let too_long = serde_json::json!({"queries": ["q"],
        "rerank_instructions": "x".repeat(401)});
    let err = parse_advanced_search_args(&too_long).unwrap_err();
    // Whatever the parser's error type, the message must name the 400 cap.
    assert!(format!("{err:?}").contains("400"));
}
```

(Adapt the second test's call/return shapes to the real `parse_advanced_search_args` signature — keep the two behavioral assertions.)

- [ ] **Step 2: Run to verify failure**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp rerank`
Expected: FAIL — first test fails on the sigmoid (relevance_multiplier ≈ 0.711), second on the missing field.

- [ ] **Step 3: Implement.**

1. `ParsedSearchArgs`: add `pub rerank_instructions: Option<String>`. In `parse_advanced_search_args`, parse + validate with `mn_core::rerank::validate_instruction`, returning the parser's InvalidInput error with the message. `parse_basic_search_args` sets it to `None`.
2. `rerank_postprocess` / `recompute_confidence`: replace `normalize_rerank(f64::from(s.score))` with `f64::from(s.score).clamp(0.0, 1.0)`; delete the `use mn_core::scoring::normalize_rerank;` import; update both doc comments ("sigmoid-normalized reranker logit" → "Voyage relevance score, already 0–1").
3. `run_search` placement: replace `resolve_reranker_selection` with

```rust
let placement = mn_core::config::resolve_rerank_placement(
    None, &core_cfg.rerank, &cfg_env, voyage_key.is_some(),
);
let rerank_model = mn_core::config::resolve_rerank_model(None, &core_cfg.rerank, &cfg_env);
let voyage_base_url = cfg_env.var("MIDNIGHT_MANUAL_VOYAGE_BASE_URL").filter(|s| !s.is_empty());
// The tool's `rerank: false` means off for this call regardless of placement.
let effective = if parsed.rerank { placement } else { mn_core::config::RerankPlacement::Off };
```

4. Cloud request: add `rerank: Option<String>` + `rerank_instructions: Option<String>` to the MCP-side `SearchRequest` serialize struct, and set them exactly as the CLI does (Server → model wire + instructions; Local/Off → `Some("none")`; Local also keeps the existing `RERANK_FETCH`/`sort_by: "score"` widening, Server/Off use `parsed.limit` with no `sort_by`).
5. Local rerank path: `rerank_results` drops `RerankConfig`/`load_configured_reranker` and constructs `VoyageReranker::new(key, model_wire)` (+ base-url override) directly; compose the pivot with the agent instruction else the shared `default_instruction` (code_exclusive from `parsed.code_mode == Some("exclusive")`, version from `parsed.filters.language_target.any_of.first()` — same expression as the CLI). On success call `LOADED_MARKERS.mark_reranker()` (the `status` marker now means "rerank capability exercised"; update its doc comment, lines 447–450).
6. Local placement with `voyage_key == None` cannot happen (placement auto-resolves to Server without a key; explicit `local` config without a key should surface as a `SearchError::Cloud("rerank location is 'local' but no Voyage API key is configured")` guard before the search).
7. Server placement: pass the cloud's `search_metadata.rerank` through untouched (the envelope passthrough already does this — verify nothing strips unknown metadata).
8. Delete `ResolvedReranker`, `resolve_reranker_selection`, `RerankConfig` (the local struct), `load_configured_reranker`, and the `LOADED_RERANKER` `OnceCell` (markers stay). Remove the now-dead `reranker`, `reranker_catalog`, `contextualized`(if unused), `LoadedReranker` imports from line 29.

- [ ] **Step 4: Run tests**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`
Expected: PASS, including `contract_sync` (Task 5 already aligned the manifest).

- [ ] **Step 5: Commit**

```bash
git add crates/mn-mcp
git commit -m "feat(mn-mcp): rerank placement + rerank_instructions; fix Voyage score sigmoid compression"
```

---

### Task 8: Deletions — ONNX catalog, fastembed, legacy config, `normalize_rerank`

**Files:**
- Delete: `crates/mn-embedding/src/reranker_catalog.rs`
- Rewrite: `crates/mn-embedding/src/reranker.rs` (only `RerankResult` survives)
- Modify: `crates/mn-embedding/src/lib.rs`, `crates/mn-embedding/src/error.rs`, `crates/mn-embedding/src/cache.rs` (doc refs), `crates/mn-embedding/Cargo.toml`, root `Cargo.toml`
- Modify: `crates/mn-cli/src/commands/models.rs` (drop the reranker pull, ~lines 129–149)
- Modify: `crates/mn-core/src/config.rs` (remove `ModelsConfig.reranker`/`reranker_path`, `resolve_reranker`, their tests)
- Modify: `crates/mn-core/src/scoring.rs` (remove `normalize_rerank` + its test assertions; update the module doc at lines 6–9 and the `RelevanceSource::Rerank` doc at line 22–23 to say "Voyage relevance score (server inline or client BYOK)")

- [ ] **Step 1: Shrink `reranker.rs`** to exactly:

```rust
//! Rerank result type shared by the Voyage reranker client and its callers.
//! (The local fastembed/ONNX cross-encoder subsystem was removed — see
//! docs/superpowers/specs/2026-06-11-voyage-reranking-design.md §5.)

/// One reranked document: `index` points into the input `documents` slice;
/// `score` is Voyage's `relevance_score` in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RerankResult {
    /// Index into the original `documents` input.
    pub index: usize,
    /// Voyage relevance score in `[0, 1]`.
    pub score: f32,
}
```

Delete `reranker_catalog.rs`. Fix `lib.rs` (`pub mod` list + the `LoadedReranker`/`RerankResult` re-exports — keep `pub use reranker::RerankResult;`, drop `LoadedReranker`). Remove fastembed-specific variants from `error.rs` if any become unused.

- [ ] **Step 2: Drop the dependency.** Verify nothing else uses it:

Run: `grep -rn "fastembed" crates/*/src/ | grep -v "^Binary"`
Expected: zero hits after Step 1 (before this task: reranker.rs, reranker_catalog.rs, lib.rs, error.rs, cache.rs — cache.rs hits should be doc-comment-only; reword them, the cache dir is still used for token-count tokenizers). Then remove `fastembed` from `crates/mn-embedding/Cargo.toml` and the `[workspace.dependencies]` entry in root `Cargo.toml` (lines ~157–158; KEEP `tokenizers` — `tokens.rs` uses it directly, and fix the comment that says it's transitive-via-fastembed). Check whether the explicit `hf-hub` pin (mn-embedding/Cargo.toml lines 26–30) was only there to mirror fastembed's features — if nothing else imports `hf_hub`, remove it too. Update `crates/mn-embedding/Cargo.toml`'s `description` (line 3).

- [ ] **Step 3: `models.rs`** — remove the reranker download block (the `mn_embedding::reranker::global(...)` call and any "pulling reranker" output). `mnm models pull` now only ensures tokenizer/cache assets. Update its doc comment and any test asserting reranker-pull output.

- [ ] **Step 4: `mn-core` legacy config** — remove `ModelsConfig.reranker`, `ModelsConfig.reranker_path`, their defaults (lines ~105/111), `resolve_reranker` (line 254), and the tests touching them (lines ~372–395, 458–474). Grep the workspace for stragglers:

Run: `grep -rn "resolve_reranker\|reranker_path\|MIDNIGHT_MANUAL_RERANKER\b\|bge-reranker" crates/ --include="*.rs"`
Expected: zero hits (Tasks 6–7 removed the call sites).

- [ ] **Step 5: `scoring.rs`** — delete `normalize_rerank` (lines 104–110), the three `normalize_rerank` assertions in `normalizers_are_bounded_and_monotonic` (rename it `normalize_rrf_is_bounded_and_monotonic`), and rewrite the module doc's normalization sentence (lines 6–9) to mention only `normalize_rrf`.

- [ ] **Step 6: Full workspace check**

Run: `cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && VOYAGE_API_KEY= cargo test --workspace`
Expected: clean (modulo the 2 known `auth_integration` sandbox failures). `cargo tree -i fastembed 2>&1` should report the package is not in the graph.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor!: delete fastembed/ONNX reranker subsystem; Voyage is the only reranker"
```

---

### Task 9: Telemetry — `Rerank` event (CLI + MCP)

Server-side observability landed as `metrics` counters in Task 4; client decisions emit FR-109 events.

**Files:**
- Modify: `crates/mn-telemetry/src/events.rs` (the `EventPayload` enum ~line 76 and the serialization test `cases` table ~line 305)
- Modify: `crates/mn-cli/src/commands/search.rs` (emit alongside the existing `CliCommand` event)
- Modify: `crates/mn-mcp` (emit where `McpToolCall` is emitted for search tools)

- [ ] **Step 1: Failing test** — add a case to the existing payload-serialization test table in `events.rs` (mirror the exact tuple shape used by the `McpToolCall` case there):

```rust
(
    EventPayload::Rerank {
        placement: "local".into(),
        model: Some("rerank-2.5".into()),
        applied: true,
        reason: None,
        billed_tokens: Some(1234),
    },
    "rerank",
),
```

Run: `cargo test -p mn-telemetry` → compile FAIL.

- [ ] **Step 2: Implement** — add the variant (serde attributes matching the enum's existing tagging convention — copy whatever `McpToolCall` uses):

```rust
/// One rerank decision (spec §6): where it ran, with what, and the outcome.
Rerank {
    /// "local" | "server" | "off".
    placement: String,
    /// Model attempted/applied; `None` when placement was "off".
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// Whether a rerank was actually applied.
    applied: bool,
    /// Degrade reason when not applied (mirrors search_metadata.rerank.reason).
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// Billed-equivalent tokens — known locally (Voyage reports total_tokens);
    /// `None` for server placement (the server tracks its own metrics).
    #[serde(skip_serializing_if = "Option::is_none")]
    billed_tokens: Option<u64>,
},
```

Run: `cargo test -p mn-telemetry` → PASS.

- [ ] **Step 3: Emit.** CLI: in `run_with_paths` next to the existing `telemetry.emit(...)` for `CliCommand`, emit one `Rerank` event per search (placement string from the resolved `RerankPlacement`, `applied` from whether the local rerank ran / what `search_metadata.rerank.applied` says on the server path, `billed_tokens` from the local `RerankOutput.total_tokens` run through `RerankParam::billed_tokens`). MCP: same, wherever search-tool telemetry is emitted (grep `McpToolCall` emission in mn-mcp). The existing three-mechanism opt-out wraps `TelemetryClient::emit` already — no extra work.

- [ ] **Step 4: Run + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-telemetry -p mn-cli -p mn-mcp`

```bash
git add crates/mn-telemetry crates/mn-cli crates/mn-mcp
git commit -m "feat(telemetry): rerank decision event from CLI + MCP"
```

---

### Task 10: Skill, README privacy note, CLAUDE.md

**Files:**
- Modify: `crates/mn-skills/assets/midnight-advanced-search/SKILL.md`
- Create: `crates/mn-skills/assets/midnight-advanced-search/references/rerank-instructions.md`
- Modify: `README.md` ("Telemetry & Privacy" section)
- Modify: `CLAUDE.md` (Recent Changes)

- [ ] **Step 1: SKILL.md.** Update the stale reranker copy: the "no model-pulling step / loads lazily" paragraph (lines ~74–75) becomes a short note that reranking is VoyageAI (`rerank-2.5`) — server-side by default, locally when a `VOYAGE_API_KEY` is configured — and that `advanced_search` exposes `rerank` (boolean) and `rerank_instructions`. Add a new section (place it near the rerank/`advanced_search` material, matching the file's heading style):

```markdown
## Rerank instructions

`advanced_search` accepts `rerank_instructions` (max 400 chars): a natural-language
directive that guides how results are reranked. It REPLACES the built-in default
(code-focused when `code_mode=exclusive`; version-preferring when a
`language_target` filter has `version_satisfies`), so include those concerns
yourself if you override.

Three instruction shapes work well:

- **Emphasis** — name what matters in the match:
  "Prioritize chunks that show complete, compilable examples over fragments."
- **Filtering** — name what kind of document you want:
  "Prefer API reference material; deprioritize tutorials and blog posts."
- **Disambiguation** — pin ambiguous query terms:
  "'Witness' means the Compact private-input function, not a legal term."

Rules of thumb:

1. Keep it under ~25 words. Instruction tokens are multiplied by the candidate
   pool (~50 docs), so a long instruction is the single most expensive thing
   you can add to a search.
2. Don't restate the query — the model already sees it. Add only the
   preference the query can't express.
3. Don't stack contradictory goals ("prefer code" + "prefer conceptual
   overviews"); pick the one that decides ties.
4. Omit it entirely when the defaults fit: the derived defaults already handle
   code-heavy and version-pinned searches.
5. `rerank: false` is the cheap-exploration switch — use it for broad recon
   sweeps where ordering precision doesn't matter, then rerank the refined query.

See references/rerank-instructions.md for worked examples against this corpus.
```

Create `references/rerank-instructions.md` with 4–6 worked examples (query + filters + instruction + why), following the formatting of the existing `references/advanced-techniques.md`. Example pair to include:

```markdown
**Query:** `compact contract upgrade pattern`
**Instruction:** `Prefer official Midnight documentation over community examples; deprioritize deprecated patterns.`
**Why:** attribution/deprecation live in trust scoring, but the instruction also
steers relevance toward chunks that *discuss* the current pattern rather than
merely mention upgrades.
```

- [ ] **Step 2: README.** In "Telemetry & Privacy", after the embeddings-proxy disclosure, add:

```markdown
When server-side reranking is enabled (the default), the search query — plus any
`rerank_instructions` — and the text of candidate result chunks are sent to
VoyageAI's rerank API, the same third-party exposure class as the embeddings
proxy. Send `rerank: "none"` (CLI: `--rerank off`) to keep a search's
candidates out of the rerank call, or rerank locally with your own
`VOYAGE_API_KEY`.
```

- [ ] **Step 3: CLAUDE.md** — add a Recent Changes bullet (top of the list, dated 2026-06-XX with the actual date):

```markdown
- 2026-06-XX — VoyageAI reranking: inline server rerank in `/v1/search`
  (`rerank` = rerank-2.5 default | rerank-2.5-lite at half-rate billing | none;
  degrade-and-flag on budget/provider failure; `MIDNIGHT_MANUAL_SERVER_RERANK`
  kill switch), client placement auto-resolution (`VOYAGE_API_KEY` ⇒ local
  BYOK), instruction-following (`rerank_instructions`, 400-char cap, derived
  defaults), and full removal of the fastembed/ONNX reranker catalog.
```

- [ ] **Step 4: Commit**

```bash
git add crates/mn-skills README.md CLAUDE.md
git commit -m "docs: rerank instruction-writing guidance, privacy note, CLAUDE.md entry"
```

---

### Task 11: Final verification sweep

- [ ] **Step 1: Full CI-surface check** (per the project rule: package builds miss test targets and feature-gated files)

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
VOYAGE_API_KEY= cargo test --workspace
cargo test -p mn-server --no-run --features integration   # match sibling gating
```

Expected: all clean except the 2 known `mn-cli auth_integration` loopback failures (sandbox-only).

- [ ] **Step 2: Behavioral spot-checks** (no DB needed)

```bash
cargo run -p mn-cli -- search --help            # new --rerank/--rerank-model/--rerank-instructions
cargo run -p mn-cli -- search "x" --rerank local 2>&1 | head -3   # with VOYAGE_API_KEY unset: clear "needs a Voyage API key" error
```

- [ ] **Step 3: Spec coverage re-read** — open `docs/superpowers/specs/2026-06-11-voyage-reranking-design.md` and confirm each section maps to landed code (§1 Task 4, §2 Tasks 1+4, §3 Tasks 1+4+6+7, §4 Tasks 2+6+7, §5 Task 8, §6 Tasks 5+9+10, §7 Tasks 1–9 tests). Fix anything missed before declaring done.

- [ ] **Step 4: Commit any stragglers; do NOT push** — integration tests run in CI on the PR.

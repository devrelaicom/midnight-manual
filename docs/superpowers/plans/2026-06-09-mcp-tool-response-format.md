# MCP Tool Response Format Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reshape every `mn-mcp` tool result into a concise summary + duplicated-trimmed-JSON `content` block plus a full `structuredContent` payload with advertised `outputSchema` and `isError` failure envelopes, enrich cloud responses with readable identity, and capture retrieval-quality telemetry with a long-lived dimensional rollup.

**Architecture:** Three phases. (1) Cloud enrichment threads readable identity (`source.display_name`, `document.source_path`/URLs, `chunk.heading_path`/`symbol_path`) into the search result shape in `mn-server`. (2) `mn-mcp` gains a `render` module (`ToolOutcome`/`ToolFailure`/`NextAction` + one projector per response shape); `server.rs` dispatch becomes a thin adapter that wraps projector output into the new `ToolCallResult` (with `structuredContent`) or an `isError` envelope, reserving JSON-RPC errors for protocol faults; every tool advertises an `outputSchema`. (3) The existing `McpToolCall` telemetry event gains additive scalar fields fed from the search projector; a new `telemetry_search_daily` table + sweep step preserves them past raw retention, which rises 7 → 90 days.

**Tech Stack:** Rust (workspace MSRV 1.91), `axum`, `sqlx`/Postgres/pgvector, `serde_json`, `jsonschema` (conformance tests), hand-rolled MCP JSON-RPC in `mn-mcp`.

---

## File structure / decomposition map

**Phase 1 — enrichment (mn-server, mn-store, contracts):**
- `crates/mn-server/src/routes/search.rs` — extend `fetch_scoring_rows` SQL + `ScoringRow` + `ScoredCandidate` + `SearchResult` + `into_result`.
- `crates/mn-store/src/entities/chunk.rs` — add `display_name` to `SourceSummary` and the chunk-context queries.
- `crates/mn-store/src/entities/document.rs` — add `display_name` to the overview/full/window source lookups.
- `specs/001-rag-platform/contracts/openapi.yaml` — extend `SearchResult`/`Source` schema shapes.

**Phase 2 — mn-mcp reformat:**
- `crates/mn-mcp/src/protocol.rs` — add `structured_content` to `ToolCallResult`, `output_schema` to `ToolDescription`.
- `crates/mn-mcp/src/render.rs` — **new** module: `NextAction`, `ToolOutcome`, `ToolFailure`, `ErrorKind`, `SearchTelemetry`, and one projector per response shape.
- `crates/mn-mcp/src/schemas.rs` — **new** module: one `fn <tool>_output_schema() -> Value` per tool.
- `crates/mn-mcp/src/tools.rs` — wire `output_schema` into each `ToolDescription` in `list()`.
- `crates/mn-mcp/src/server.rs` — dispatch adapter: run tool → project → `ToolCallResult`; map errors to `ToolFailure`; thread `SearchTelemetry` to the event.
- `crates/mn-mcp/src/lib.rs` — `mod render; mod schemas;`.
- `specs/001-rag-platform/contracts/mcp-tools.json` — add `outputSchema` per tool.
- `crates/mn-mcp/tests/*` — new shape/conformance tests; rewrite dump-shape assertions.

**Phase 3 — telemetry:**
- `crates/mn-telemetry/src/events.rs` — additive `Option` fields on `McpToolCall`.
- `crates/mn-store/migrations/0010_telemetry_search_daily.sql` — **new** dimensional table.
- `crates/mn-server/src/jobs/telemetry_sweep.rs` — second aggregate step.
- `crates/mn-server/src/config.rs` — retention default 7 → 90.
- `crates/mn-server/src/routes/telemetry.rs` — (no schema change; validator already allows `mcp_tool_call`).
- `crates/mn-telemetry/tests/canary_suite.rs` — cover the new fields.

---

# PHASE 1 — Cloud enrichment

Readable identity already exists in the DB. The chunk/document passthrough endpoints already
return `source_path`/`heading_path`/`symbol_path`/`slug`; only **search** results lack readable
identity, and **no** endpoint exposes `source.display_name`. Phase 1 closes both gaps.

### Task 1.1: Add `display_name` to the store `SourceSummary`

**Files:**
- Modify: `crates/mn-store/src/entities/chunk.rs` (the `SourceSummary` struct, ~lines 44-48; `ChunkWithContextRow` ~419-447; `get_with_context`/`list_next`/`list_prev` SELECTs; the `TryFrom<ChunkWithContextRow>` impl)
- Test: `crates/mn-store/src/entities/chunk.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Extend `SourceSummary`**

Find the `SourceSummary` struct in `chunk.rs` (currently `pub struct SourceSummary { pub slug: String }`) and add `display_name`:

```rust
/// Minimal source identity attached to chunk/document responses.
#[derive(Debug, Clone, Serialize)]
pub struct SourceSummary {
    /// URL-safe stable handle, e.g. `compact-docs`.
    pub slug: String,
    /// Human-readable name, e.g. `Compact Docs`.
    pub display_name: String,
}
```

- [ ] **Step 2: Select `display_name` in the three chunk-context queries**

In `get_with_context`, `list_next`, and `list_prev`, the SELECT list ends with `s.slug AS s_slug`.
In **each** of the three queries change that to:

```
s.slug AS s_slug, s.display_name AS s_display_name
```

- [ ] **Step 3: Add the column to `ChunkWithContextRow`**

In `ChunkWithContextRow`, after `s_slug: String,` add:

```rust
    s_display_name: String,
```

- [ ] **Step 4: Populate it in `TryFrom<ChunkWithContextRow>`**

Find the `TryFrom<ChunkWithContextRow> for ChunkWithContext` impl. Wherever it constructs
`SourceSummary { slug: row.s_slug }`, change to:

```rust
SourceSummary { slug: row.s_slug, display_name: row.s_display_name }
```

- [ ] **Step 5: Run store unit tests (no DB)**

Run: `cargo test -p mn-store --lib`
Expected: PASS (compile + non-DB unit tests). DB-backed query tests are integration-gated and run in CI.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-store/src/entities/chunk.rs
git commit -m "feat(mn-store): add source.display_name to chunk-context responses"
```

### Task 1.2: Add `display_name` to document overview/full/window source lookups

**Files:**
- Modify: `crates/mn-store/src/entities/document.rs` (`get_overview`, `get_full`, `list_chunks_window` — each does `SELECT s.slug ... fetch_one` then builds `SourceSummary { slug: source_slug }`)

- [ ] **Step 1: Fetch slug + display_name together in all three functions**

Each of `get_overview`, `get_full`, `list_chunks_window` currently runs:

```rust
let source_slug = sqlx::query_scalar::<_, String>(
    "SELECT s.slug FROM source s \
     JOIN source_version sv ON sv.source_id = s.id \
     WHERE sv.id = $1",
)
.bind(document.source_version_id)
.fetch_one(pool)
.await?;
```

Replace each occurrence with a two-column fetch:

```rust
let (source_slug, source_display_name) = sqlx::query_as::<_, (String, String)>(
    "SELECT s.slug, s.display_name FROM source s \
     JOIN source_version sv ON sv.source_id = s.id \
     WHERE sv.id = $1",
)
.bind(document.source_version_id)
.fetch_one(pool)
.await?;
```

- [ ] **Step 2: Pass `display_name` into each `SourceSummary`**

In all three functions, change `crate::entities::chunk::SourceSummary { slug: source_slug }` to:

```rust
crate::entities::chunk::SourceSummary { slug: source_slug, display_name: source_display_name }
```

- [ ] **Step 3: Compile + unit tests**

Run: `cargo test -p mn-store --lib`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-store/src/entities/document.rs
git commit -m "feat(mn-store): add source.display_name to document overview/full/window"
```

### Task 1.3: Enrich search results with readable identity

**Files:**
- Modify: `crates/mn-server/src/routes/search.rs` (`ScoringRow` ~745-756; `fetch_scoring_rows` SQL ~752-789; `ScoredCandidate` ~631-648; the candidate-assembly site that fills `ScoredCandidate` from a `ScoringRow`; `SearchResult` ~145-165; `into_result` ~653-676)
- Test: `crates/mn-server/tests/` (integration-gated) + an inline unit test for `into_result`

- [ ] **Step 1: Add a failing unit test for `into_result` readable fields**

Add to the `#[cfg(test)]` module in `search.rs`:

```rust
#[test]
fn into_result_carries_readable_identity() {
    let c = ScoredCandidate {
        chunk_id: Uuid::nil(),
        content: "x".into(),
        document_id: Uuid::nil(),
        source_version_id: Uuid::nil(),
        chunk_index: 0,
        total_chunks: 1,
        start_byte: 0,
        end_byte: 1,
        created_at: OffsetDateTime::UNIX_EPOCH,
        rrf_score: 0.0,
        vector_similarity: 0.0,
        matched_queries: vec![],
        relevance: 0.0,
        score: ScoreResult::default(),
        source_slug: "compact-docs".into(),
        source_display_name: "Compact Docs".into(),
        source_path: "docs/intro.md".into(),
        published_url: Some("https://x/intro".into()),
        source_url: None,
        heading_path: vec!["Compiling".into(), "Witnesses".into()],
        symbol_path: vec![],
    };
    let r = c.into_result(false);
    assert_eq!(r.source_path, "docs/intro.md");
    assert_eq!(r.source_display_name, "Compact Docs");
    assert_eq!(r.heading_path, vec!["Compiling".to_string(), "Witnesses".to_string()]);
}
```

(If `ScoreResult` has no `Default`, build it explicitly to match its real fields — check the
`ScoreResult` definition in `mn-retrieval`/`mn-core` and fill the test literal accordingly.)

- [ ] **Step 2: Run it; expect a compile failure**

Run: `cargo test -p mn-server --lib into_result_carries_readable_identity`
Expected: FAIL — `ScoredCandidate`/`SearchResult` have no `source_path` etc. yet.

- [ ] **Step 3: Add readable columns to `ScoringRow`**

In `ScoringRow` (after `ingested_at: OffsetDateTime,`) add:

```rust
    source_slug: String,
    source_display_name: String,
    source_path: String,
    published_url: Option<String>,
    source_url: Option<String>,
    heading_path: Vec<String>,
    symbol_path: Vec<String>,
```

- [ ] **Step 4: Extend the `fetch_scoring_rows` SQL + decode**

In `fetch_scoring_rows`, change the query so it also joins `source` and selects the new columns.
The current SELECT/JOIN block becomes:

```rust
    let rows = sqlx::query(
        "SELECT chunk.id, chunk.document_id, chunk.source_version_id, chunk.chunk_index, \
                chunk.total_chunks, chunk.start_byte, chunk.end_byte, chunk.content, chunk.created_at, \
                chunk.heading_path AS heading_path, chunk.symbol_path AS symbol_path, \
                d.provenance AS provenance, d.source_modified_at AS source_modified_at, \
                d.source_path AS source_path, d.published_url AS published_url, d.source_url AS source_url, \
                s.slug AS source_slug, s.display_name AS source_display_name, \
                sv.ingested_at AS ingested_at \
         FROM chunk \
         JOIN document d ON d.id = chunk.document_id \
         JOIN source_version sv ON sv.id = chunk.source_version_id \
         JOIN source s ON s.id = sv.source_id \
         WHERE chunk.id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
```

`symbol_path` is stored as JSONB (`Json<Vec<SymbolSegment>>` in the chunk entity). For the
scoring row we only need display strings: decode it as `serde_json::Value` and flatten to the
segment names. In the row-build loop, after the existing `provenance` decode, add:

```rust
        let symbol_json: serde_json::Value = r.try_get("symbol_path").unwrap_or(serde_json::Value::Null);
        let symbol_path: Vec<String> = symbol_json
            .as_array()
            .map(|segs| {
                segs.iter()
                    .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
```

and set the new `ScoringRow` fields:

```rust
                source_slug: r.try_get("source_slug")?,
                source_display_name: r.try_get("source_display_name")?,
                source_path: r.try_get("source_path")?,
                published_url: r.try_get("published_url")?,
                source_url: r.try_get("source_url")?,
                heading_path: r.try_get("heading_path")?,
                symbol_path,
```

> Note: confirm `symbol_path`'s stored JSON element key is `name` by checking
> `mn_core::types::SymbolSegment`'s `Serialize` derive. If the field is named differently,
> use that key. (The chunk entity stores `Json<Vec<SymbolSegment>>`.)

- [ ] **Step 5: Add the fields to `ScoredCandidate`**

In `ScoredCandidate` (after `score: ScoreResult,`) add:

```rust
    source_slug: String,
    source_display_name: String,
    source_path: String,
    published_url: Option<String>,
    source_url: Option<String>,
    heading_path: Vec<String>,
    symbol_path: Vec<String>,
```

- [ ] **Step 6: Thread them where `ScoredCandidate` is built from a `ScoringRow`**

Find the site that constructs `ScoredCandidate { ... }` pulling from the `ScoringRow` map
(search for `ScoredCandidate {` in `search.rs`). Add the seven fields, cloning from the row:

```rust
        source_slug: row.source_slug.clone(),
        source_display_name: row.source_display_name.clone(),
        source_path: row.source_path.clone(),
        published_url: row.published_url.clone(),
        source_url: row.source_url.clone(),
        heading_path: row.heading_path.clone(),
        symbol_path: row.symbol_path.clone(),
```

- [ ] **Step 7: Add the fields to `SearchResult`**

In `SearchResult` (after `pub created_at: OffsetDateTime,`, before `scores`) add:

```rust
    /// URL-safe source handle.
    pub source_slug: String,
    /// Human-readable source name.
    pub source_display_name: String,
    /// Source-relative path of the parent document, e.g. `docs/intro.md`.
    pub source_path: String,
    /// Canonical published URL, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_url: Option<String>,
    /// Original source URL, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// Markdown heading breadcrumb for this chunk.
    pub heading_path: Vec<String>,
    /// Code symbol breadcrumb for this chunk.
    pub symbol_path: Vec<String>,
```

- [ ] **Step 8: Populate them in `into_result`**

In `into_result`, extend the returned `SearchResult { ... }` with:

```rust
            source_slug: self.source_slug,
            source_display_name: self.source_display_name,
            source_path: self.source_path,
            published_url: self.published_url,
            source_url: self.source_url,
            heading_path: self.heading_path,
            symbol_path: self.symbol_path,
```

- [ ] **Step 9: Run the unit test**

Run: `cargo test -p mn-server --lib into_result_carries_readable_identity`
Expected: PASS.

- [ ] **Step 10: Full-surface check for this crate**

Run: `cargo clippy -p mn-server --all-targets --all-features -- -D warnings && cargo test -p mn-server --lib`
Expected: PASS (no warnings).

- [ ] **Step 11: Commit**

```bash
git add crates/mn-server/src/routes/search.rs
git commit -m "feat(mn-server): enrich search results with readable identity (path, breadcrumb, source name)"
```

### Task 1.4: Update the OpenAPI contract

**Files:**
- Modify: `specs/001-rag-platform/contracts/openapi.yaml` (`SearchResult` is represented via `ChunkResult`/`Source`; add the readable fields to the search result and `Source.display_name`)

- [ ] **Step 1: Add `display_name` to the `Source` schema**

Locate `components.schemas.Source` and add a `display_name` property:

```yaml
        display_name: { type: string, description: "Human-readable source name." }
```

- [ ] **Step 2: Document the enriched search result fields**

The `/v1/search` response serializes the Rust `SearchResult`. In the schema block that
describes a search result (the `results[]` item), add:

```yaml
        source_slug: { type: string }
        source_display_name: { type: string }
        source_path: { type: string, description: "Source-relative document path, e.g. docs/intro.md" }
        published_url: { type: string, nullable: true }
        source_url: { type: string, nullable: true }
        heading_path: { type: array, items: { type: string } }
        symbol_path: { type: array, items: { type: string } }
```

- [ ] **Step 3: Commit**

```bash
git add specs/001-rag-platform/contracts/openapi.yaml
git commit -m "docs(contracts): document enriched search-result readable identity"
```

---

# PHASE 2 — mn-mcp response reformat

### Task 2.1: Extend the protocol types

**Files:**
- Modify: `crates/mn-mcp/src/protocol.rs` (`ToolCallResult` ~276-284; `ToolDescription` ~190-200)
- Test: `crates/mn-mcp/src/protocol.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Add a failing serialization test**

Add to `protocol.rs` tests:

```rust
#[test]
fn tool_call_result_serializes_structured_content() {
    let r = ToolCallResult {
        content: vec![ContentBlock::Text { text: "hi".into() }],
        structured_content: Some(serde_json::json!({ "k": 1 })),
        is_error: false,
    };
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["structuredContent"]["k"], 1);
    assert!(v.get("isError").is_none()); // false is skipped
}

#[test]
fn tool_description_serializes_output_schema() {
    let d = ToolDescription {
        name: "x",
        description: "y",
        input_schema: serde_json::json!({}),
        output_schema: Some(serde_json::json!({ "type": "object" })),
    };
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(v["outputSchema"]["type"], "object");
}
```

- [ ] **Step 2: Run; expect compile failure**

Run: `cargo test -p mn-mcp --lib protocol::tests`
Expected: FAIL — fields don't exist.

- [ ] **Step 3: Add `structured_content` to `ToolCallResult`**

```rust
/// `tools/call` response payload.
#[derive(Debug, Serialize)]
pub struct ToolCallResult {
    /// Output content blocks (we always emit a single `text` block).
    pub content: Vec<ContentBlock>,
    /// Machine-readable result; conforms to the tool's `outputSchema` on success.
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    /// Set when the tool reported an error condition (vs. a hard JSON-RPC error).
    #[serde(rename = "isError", skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}
```

- [ ] **Step 4: Add `output_schema` to `ToolDescription`**

```rust
/// One tool declaration in `tools/list` response.
#[derive(Debug, Serialize)]
pub struct ToolDescription {
    /// Tool name (e.g. "search").
    pub name: &'static str,
    /// Human-readable description (shown by AI clients).
    pub description: &'static str,
    /// JSON Schema for the tool's input parameters.
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
    /// JSON Schema for the tool's `structuredContent`.
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}
```

- [ ] **Step 5: Fix existing `ToolDescription`/`ToolCallResult` constructors**

`list()` in `tools.rs` builds `ToolDescription { name, description, input_schema }` 14 times —
add `output_schema: None,` to each (Task 2.7 fills them in). `server.rs` builds
`ToolCallResult { content, is_error }` — add `structured_content: None,`. Compile to find every
site:

Run: `cargo build -p mn-mcp`
Expected: errors listing each struct literal missing a field; add the field at each.

- [ ] **Step 6: Run tests**

Run: `cargo test -p mn-mcp --lib protocol::tests`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/mn-mcp/src/protocol.rs crates/mn-mcp/src/tools.rs crates/mn-mcp/src/server.rs
git commit -m "feat(mn-mcp): add structuredContent + outputSchema to protocol types"
```

### Task 2.2: Create the `render` module — `NextAction`, `ToolOutcome`, `ToolFailure`

**Files:**
- Create: `crates/mn-mcp/src/render.rs`
- Modify: `crates/mn-mcp/src/lib.rs` (add `mod render;`)
- Test: `crates/mn-mcp/src/render.rs` (inline tests)

- [ ] **Step 1: Write the module with a failing test**

Create `crates/mn-mcp/src/render.rs`:

```rust
//! Shapes every tool result into the MCP "summary + structuredContent" form.
//!
//! Success → one `text` content block (`summary` + the trimmed JSON in a fenced
//! ```json block) plus full-fidelity `structuredContent` (with `next_actions`).
//! Failure → an `isError: true` result carrying a shared error envelope.

use serde_json::{json, Value};

use crate::protocol::{ContentBlock, ToolCallResult};

/// A suggested follow-up tool call surfaced to the agent.
#[derive(Debug, Clone)]
pub struct NextAction {
    /// Tool name to call next.
    pub tool: &'static str,
    /// Arguments object for that call.
    pub arguments: Value,
}

impl NextAction {
    fn to_value(&self) -> Value {
        json!({ "tool": self.tool, "arguments": self.arguments })
    }
}

fn next_actions_value(actions: &[NextAction]) -> Value {
    Value::Array(actions.iter().map(NextAction::to_value).collect())
}

/// Retrieval-quality facts the search projector hands to the telemetry emitter.
#[derive(Debug, Clone, Default)]
pub struct SearchTelemetry {
    pub corpus_model: Option<String>,
    pub reranker_used: Option<String>,
    pub top_confidence_bucket: Option<&'static str>,
    pub top_attribution: Option<String>,
    pub top_source: Option<String>,
    pub filtered_by_confidence: Option<u32>,
    pub deduplicated_count: Option<u32>,
    pub result_count: u32,
}

/// A successful tool result, pre-render.
pub struct ToolOutcome {
    /// Concise, agent-facing summary line(s).
    pub summary: String,
    /// Full canonical payload (becomes `structuredContent`; `next_actions` injected at render).
    pub structured: Value,
    /// Essentials-only view embedded as the fenced JSON in the text block.
    pub trimmed: Value,
    /// Suggested follow-ups.
    pub next_actions: Vec<NextAction>,
    /// Optional telemetry facts (search only).
    pub telemetry: Option<SearchTelemetry>,
}

impl ToolOutcome {
    /// Convenience constructor for non-search tools (no telemetry facts).
    pub fn new(summary: String, structured: Value, trimmed: Value, next_actions: Vec<NextAction>) -> Self {
        Self { summary, structured, trimmed, next_actions, telemetry: None }
    }

    /// Render into the wire `ToolCallResult`.
    pub fn into_result(self) -> ToolCallResult {
        let mut structured = self.structured;
        if let Value::Object(map) = &mut structured {
            map.insert("next_actions".to_owned(), next_actions_value(&self.next_actions));
        }
        let trimmed = serde_json::to_string(&self.trimmed).unwrap_or_else(|_| "{}".to_owned());
        ToolCallResult {
            content: vec![ContentBlock::Text { text: format!("{}\n\n```json\n{trimmed}\n```", self.summary) }],
            structured_content: Some(structured),
            is_error: false,
        }
    }
}

/// Closed set of tool-execution error kinds.
#[derive(Debug, Clone, Copy)]
pub enum ErrorKind {
    InvalidInput,
    NotFound,
    EmbeddingModelMismatch,
    TooManyChunks,
    CloudError,
    ModelLoadFailed,
    InstallFailed,
}

impl ErrorKind {
    fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "INVALID_INPUT",
            Self::NotFound => "NOT_FOUND",
            Self::EmbeddingModelMismatch => "EMBEDDING_MODEL_MISMATCH",
            Self::TooManyChunks => "TOO_MANY_CHUNKS",
            Self::CloudError => "CLOUD_ERROR",
            Self::ModelLoadFailed => "MODEL_LOAD_FAILED",
            Self::InstallFailed => "INSTALL_FAILED",
        }
    }
    fn retryable(self) -> bool {
        match self {
            Self::NotFound => false,
            Self::InvalidInput
            | Self::EmbeddingModelMismatch
            | Self::TooManyChunks
            | Self::CloudError
            | Self::ModelLoadFailed
            | Self::InstallFailed => true,
        }
    }
}

/// A tool-execution failure, pre-render (becomes an `isError: true` result).
pub struct ToolFailure {
    pub kind: ErrorKind,
    pub message: String,
    pub guidance: String,
    /// Extra fields merged into the `error` object (e.g. mismatch / too_many_chunks data).
    pub details: Value,
    pub next_actions: Vec<NextAction>,
}

impl ToolFailure {
    /// Minimal failure with no extra details and no next actions.
    pub fn simple(kind: ErrorKind, message: impl Into<String>, guidance: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            guidance: guidance.into(),
            details: Value::Null,
            next_actions: Vec::new(),
        }
    }

    /// Render into the wire `ToolCallResult` (`isError: true`).
    pub fn into_result(self) -> ToolCallResult {
        let mut error = json!({
            "code": self.kind.code(),
            "retryable": self.kind.retryable(),
            "message": self.message,
        });
        if let (Value::Object(emap), Value::Object(dmap)) = (&mut error, &self.details) {
            for (k, v) in dmap {
                emap.insert(k.clone(), v.clone());
            }
        }
        let structured = json!({
            "error": error,
            "next_actions": next_actions_value(&self.next_actions),
        });
        let trimmed = json!({ "error": { "code": self.kind.code(), "retryable": self.kind.retryable() } });
        let trimmed = serde_json::to_string(&trimmed).unwrap_or_else(|_| "{}".to_owned());
        ToolCallResult {
            content: vec![ContentBlock::Text { text: format!("{}\n\n```json\n{trimmed}\n```", self.guidance) }],
            structured_content: Some(structured),
            is_error: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_renders_summary_then_fenced_json_and_structured() {
        let o = ToolOutcome::new(
            "Found 1.".into(),
            json!({ "results": [1] }),
            json!({ "match_count": 1 }),
            vec![NextAction { tool: "get_chunk", arguments: json!({ "id": "abc" }) }],
        );
        let r = o.into_result();
        assert!(!r.is_error);
        let text = match &r.content[0] { ContentBlock::Text { text } => text };
        assert!(text.starts_with("Found 1.\n\n```json\n"));
        assert!(text.contains("\"match_count\":1"));
        let sc = r.structured_content.unwrap();
        assert_eq!(sc["results"][0], 1);
        assert_eq!(sc["next_actions"][0]["tool"], "get_chunk");
    }

    #[test]
    fn failure_renders_iserror_with_envelope() {
        let f = ToolFailure::simple(ErrorKind::NotFound, "no chunk abc", "Verify the id from a recent search.");
        let r = f.into_result();
        assert!(r.is_error);
        let sc = r.structured_content.unwrap();
        assert_eq!(sc["error"]["code"], "NOT_FOUND");
        assert_eq!(sc["error"]["retryable"], false);
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/mn-mcp/src/lib.rs` add (with the other `mod` lines): `mod render;`

- [ ] **Step 3: Run the tests**

Run: `cargo test -p mn-mcp --lib render::tests`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-mcp/src/render.rs crates/mn-mcp/src/lib.rs
git commit -m "feat(mn-mcp): render module — ToolOutcome/ToolFailure/NextAction"
```

### Task 2.3: Search projector + telemetry facts

**Files:**
- Modify: `crates/mn-mcp/src/render.rs` (add `project_search`)
- Test: `crates/mn-mcp/src/render.rs`

The cloud search envelope (after Phase 1) looks like:
`{ "corpus_embedding_model": "voyage-code-3@1", "results": [ { "chunk_id", "document_id",
"source_slug", "source_display_name", "source_path", "heading_path": [..], "symbol_path": [..],
"content", "rerank_score"?, "scores": { "confidence", "trust_score", "confidence_factors": {
"attribution", "verified", .. }, .. } } ], "search_metadata": { "filtered_by_confidence",
"deduplicated_count", .. } }`. The reranker name (when local rerank ran) is added by the caller
(Task 2.8) — the projector accepts it as a parameter.

- [ ] **Step 1: Write a failing test**

Add to `render.rs` tests:

```rust
fn sample_search_envelope() -> Value {
    json!({
        "corpus_embedding_model": "voyage-code-3@1",
        "results": [{
            "chunk_id": "1f39", "document_id": "7d5c",
            "source_slug": "compact-docs", "source_display_name": "Compact Docs",
            "source_path": "docs/intro.md", "heading_path": ["Compiling", "Witnesses"],
            "symbol_path": [], "content": "withVacantWitnesses ...",
            "scores": { "confidence": 0.81, "trust_score": 1.0,
                        "confidence_factors": { "attribution": "foundation", "verified": true } }
        }],
        "search_metadata": { "filtered_by_confidence": 0, "deduplicated_count": 0 }
    })
}

#[test]
fn project_search_summary_and_telemetry() {
    let o = super::project_search(sample_search_envelope(), Some("bge-reranker-base"));
    assert!(o.summary.contains("docs/intro.md"));
    assert!(o.summary.contains("Compact Docs") || o.summary.contains("foundation"));
    // trimmed drops scoring
    assert!(o.trimmed["results"][0].get("scores").is_none());
    assert_eq!(o.trimmed["results"][0]["source_path"], "docs/intro.md");
    // structured keeps scoring
    assert!(o.structured["results"][0].get("scores").is_some());
    // next actions point at the top chunk + document
    assert_eq!(o.next_actions[0].tool, "get_chunk");
    assert_eq!(o.next_actions[0].arguments["id"], "1f39");
    // telemetry facts populated
    let t = o.telemetry.unwrap();
    assert_eq!(t.result_count, 1);
    assert_eq!(t.corpus_model.as_deref(), Some("voyage-code-3@1"));
    assert_eq!(t.top_attribution.as_deref(), Some("foundation"));
    assert_eq!(t.top_source.as_deref(), Some("Compact Docs"));
    assert_eq!(t.reranker_used.as_deref(), Some("bge-reranker-base"));
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo test -p mn-mcp --lib render::tests::project_search_summary_and_telemetry`
Expected: FAIL — `project_search` undefined.

- [ ] **Step 3: Implement `project_search`**

Add to `render.rs`:

```rust
/// Map a confidence in [0,1] to a coarse bucket label (telemetry-safe; never the raw float).
fn confidence_bucket(c: f64) -> &'static str {
    if c >= 0.85 { "high" } else if c >= 0.7 { "medium" } else if c >= 0.5 { "low" } else { "very_low" }
}

fn str_field<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path { cur = cur.get(p)?; }
    cur.as_str()
}

/// Project the cloud search envelope. `reranker_used` is the local reranker name when local
/// rerank ran, else `None`.
pub fn project_search(envelope: Value, reranker_used: Option<&str>) -> ToolOutcome {
    let corpus_model = envelope.get("corpus_embedding_model").and_then(Value::as_str).map(str::to_owned);
    let results = envelope.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
    let result_count = u32::try_from(results.len()).unwrap_or(u32::MAX);
    let filtered = envelope.pointer("/search_metadata/filtered_by_confidence").and_then(Value::as_u64);
    let deduped = envelope.pointer("/search_metadata/deduplicated_count").and_then(Value::as_u64);

    // Trimmed: per-result essentials, scoring stripped.
    let trimmed_results: Vec<Value> = results.iter().enumerate().map(|(i, r)| {
        json!({
            "rank": i + 1,
            "chunk_id": r.get("chunk_id").cloned().unwrap_or(Value::Null),
            "document_id": r.get("document_id").cloned().unwrap_or(Value::Null),
            "source_path": r.get("source_path").cloned().unwrap_or(Value::Null),
            "source_display_name": r.get("source_display_name").cloned().unwrap_or(Value::Null),
            "heading_path": r.get("heading_path").cloned().unwrap_or(json!([])),
            "confidence": r.pointer("/scores/confidence").cloned().unwrap_or(Value::Null),
            "attribution": str_field(r, &["scores", "confidence_factors", "attribution"]).unwrap_or(""),
            "content": r.get("content").cloned().unwrap_or(Value::Null),
        })
    }).collect();

    // Summary from the top result.
    let top = results.first();
    let summary = match top {
        Some(t) => {
            let path = t.get("source_path").and_then(Value::as_str).unwrap_or("(unknown)");
            let heading = t.get("heading_path").and_then(Value::as_array)
                .map(|h| h.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" › "))
                .filter(|s| !s.is_empty());
            let attr = str_field(t, &["scores", "confidence_factors", "attribution"]).unwrap_or("unknown");
            let conf = t.pointer("/scores/confidence").and_then(Value::as_f64).unwrap_or(0.0);
            let chunk_id = t.get("chunk_id").and_then(Value::as_str).unwrap_or("?");
            let model = corpus_model.as_deref().unwrap_or("?");
            let where_ = heading.map_or_else(|| path.to_owned(), |h| format!("{path} › {h}"));
            format!("Search: {result_count} matches, corpus {model}. Top: {where_} [{attr} · {conf:.2}] chunk {chunk_id} — fetch with get_chunk.")
        }
        None => format!("Search: 0 matches, corpus {}.", corpus_model.as_deref().unwrap_or("?")),
    };

    // next_actions from the top result.
    let next_actions = top.map(|t| {
        let mut v = Vec::new();
        if let Some(id) = t.get("chunk_id").and_then(Value::as_str) {
            v.push(NextAction { tool: "get_chunk", arguments: json!({ "id": id }) });
        }
        if let Some(id) = t.get("document_id").and_then(Value::as_str) {
            v.push(NextAction { tool: "get_document", arguments: json!({ "id": id }) });
        }
        v
    }).unwrap_or_default();

    // Telemetry facts.
    let telemetry = SearchTelemetry {
        corpus_model: corpus_model.clone(),
        reranker_used: reranker_used.map(str::to_owned),
        top_confidence_bucket: top.and_then(|t| t.pointer("/scores/confidence").and_then(Value::as_f64)).map(confidence_bucket),
        top_attribution: top.and_then(|t| str_field(t, &["scores", "confidence_factors", "attribution"]).map(str::to_owned)),
        top_source: top.and_then(|t| t.get("source_display_name").and_then(Value::as_str).map(str::to_owned)),
        filtered_by_confidence: filtered.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
        deduplicated_count: deduped.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
        result_count,
    };

    ToolOutcome {
        summary,
        structured: envelope,
        trimmed: json!({ "results": trimmed_results, "match_count": result_count }),
        next_actions,
        telemetry: Some(telemetry),
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p mn-mcp --lib render::tests::project_search_summary_and_telemetry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-mcp/src/render.rs
git commit -m "feat(mn-mcp): search projector with telemetry facts"
```

### Task 2.4: Chunk / document projectors

**Files:**
- Modify: `crates/mn-mcp/src/render.rs`
- Test: `crates/mn-mcp/src/render.rs`

These consume the cloud passthrough envelopes, which already carry readable identity. The
single-chunk envelope (from `get_chunk`) is a `ChunkWithContext`: `{ chunk: { id, chunk_index,
total_chunks, content, heading_path, symbol_path, .. }, document: { source_path, .. }, source:
{ slug, display_name } }` (confirm exact nesting against `ChunkWithContext`'s `Serialize`).
List envelopes are `{ "chunks": [ ChunkWithContext, .. ] }`. Neighbors is `{ prev: {chunks:[..]},
chunk: ChunkWithContext, next: {chunks:[..]} }`. Parents is a JSON array of nodes. Document
overview/full/window carry `document`, `source`, and `chunk_ids`/`chunks`.

> Before writing each projector, open one real serialized example (run the relevant store
> entity's `Serialize` or check `tests/tools_dispatch.rs` fixtures) to confirm the exact key
> nesting; the helpers below read defensively with `pointer`/`get` so a missing key degrades to
> a still-valid (if terse) summary rather than panicking.

- [ ] **Step 1: Failing tests for chunk + chunk-list + document projectors**

```rust
#[test]
fn project_chunk_summary() {
    let env = json!({
        "chunk": { "id": "c1", "chunk_index": 4, "total_chunks": 35, "content": "body",
                   "heading_path": ["A", "B"] },
        "document": { "source_path": "docs/intro.md" },
        "source": { "display_name": "Compact Docs" }
    });
    let o = super::project_chunk(env);
    assert!(o.summary.contains("docs/intro.md"));
    assert!(o.summary.contains("4")); // index
    assert_eq!(o.next_actions.iter().filter(|a| a.tool == "get_chunk_next").count(), 1);
}

#[test]
fn project_chunk_list_counts() {
    let env = json!({ "chunks": [ { "chunk": { "id": "a" } }, { "chunk": { "id": "b" } } ] });
    let o = super::project_chunk_list(env, "after");
    assert!(o.summary.contains("2"));
    assert_eq!(o.trimmed["count"], 2);
}

#[test]
fn project_document_overview_summary() {
    let env = json!({
        "document": { "id": "d1", "source_path": "docs/intro.md" },
        "source": { "display_name": "Compact Docs" },
        "chunk_ids": ["a", "b", "c"]
    });
    let o = super::project_document_overview(env);
    assert!(o.summary.contains("docs/intro.md"));
    assert!(o.summary.contains("3"));
    assert!(o.next_actions.iter().any(|a| a.tool == "get_document_full"));
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo test -p mn-mcp --lib render::tests::project_chunk`
Expected: FAIL — projectors undefined.

- [ ] **Step 3: Implement the chunk/document projectors**

Add to `render.rs`:

```rust
fn chunk_label(chunk_env: &Value) -> (String, Option<String>, String) {
    // Returns (source_path, heading breadcrumb, chunk_id).
    let path = chunk_env.pointer("/document/source_path").and_then(Value::as_str)
        .or_else(|| chunk_env.get("source_path").and_then(Value::as_str))
        .unwrap_or("(unknown)").to_owned();
    let heading = chunk_env.pointer("/chunk/heading_path").or_else(|| chunk_env.get("heading_path"))
        .and_then(Value::as_array)
        .map(|h| h.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" › "))
        .filter(|s| !s.is_empty());
    let id = chunk_env.pointer("/chunk/id").and_then(Value::as_str)
        .or_else(|| chunk_env.get("id").and_then(Value::as_str))
        .unwrap_or("?").to_owned();
    (path, heading, id)
}

/// `get_chunk`: single chunk-with-context.
pub fn project_chunk(env: Value) -> ToolOutcome {
    let (path, heading, id) = chunk_label(&env);
    let idx = env.pointer("/chunk/chunk_index").and_then(Value::as_i64);
    let total = env.pointer("/chunk/total_chunks").and_then(Value::as_i64);
    let where_ = heading.map_or_else(|| path.clone(), |h| format!("{path} › {h}"));
    let pos = match (idx, total) { (Some(i), Some(t)) => format!(" (idx {i}/{t})"), _ => String::new() };
    let summary = format!("Chunk {id} — {where_}{pos}.");
    let next_actions = vec![
        NextAction { tool: "get_chunk_next", arguments: json!({ "id": id }) },
        NextAction { tool: "get_chunk_prev", arguments: json!({ "id": id }) },
        NextAction { tool: "get_chunk_neighbors", arguments: json!({ "id": id }) },
        NextAction { tool: "get_chunk_parents", arguments: json!({ "id": id }) },
    ];
    ToolOutcome::new(summary, env.clone(), env, next_actions)
}

fn chunk_list_len(env: &Value) -> usize {
    env.get("chunks").and_then(Value::as_array).map_or(0, Vec::len)
}

/// `get_chunk_next` / `get_chunk_prev`: `{ chunks: [..] }`. `direction` is "after"/"before".
pub fn project_chunk_list(env: Value, direction: &str) -> ToolOutcome {
    let n = chunk_list_len(&env);
    let anchor = env.pointer("/chunks/0/chunk/id").and_then(Value::as_str).unwrap_or("?");
    let summary = format!("{n} chunk(s) {direction} {anchor}.");
    let trimmed = json!({ "count": n });
    ToolOutcome::new(summary, env, trimmed, vec![])
}

/// `get_chunk_neighbors`: `{ prev: {chunks:[..]}, chunk: {..}, next: {chunks:[..]} }`.
pub fn project_neighbors(env: Value) -> ToolOutcome {
    let prev = env.pointer("/prev/chunks").and_then(Value::as_array).map_or(0, Vec::len);
    let next = env.pointer("/next/chunks").and_then(Value::as_array).map_or(0, Vec::len);
    let id = env.pointer("/chunk/chunk/id").and_then(Value::as_str)
        .or_else(|| env.pointer("/chunk/id").and_then(Value::as_str)).unwrap_or("?");
    let summary = format!("{} neighbor(s) around {id} ({prev} before, {next} after).", prev + next);
    let trimmed = json!({ "prev": prev, "next": next });
    ToolOutcome::new(summary, env, trimmed, vec![
        NextAction { tool: "get_document", arguments: json!({}) },
    ])
}

/// `get_chunk_parents`: JSON array of ancestor nodes.
pub fn project_parents(env: Value) -> ToolOutcome {
    let n = env.as_array().map_or(0, Vec::len);
    let names: Vec<&str> = env.as_array().map(|a| a.iter()
        .filter_map(|node| node.get("name").and_then(Value::as_str)).collect()).unwrap_or_default();
    let summary = format!("{n} ancestor node(s): {}.", names.join(" / "));
    let trimmed = json!({ "count": n, "names": names });
    ToolOutcome::new(summary, env, trimmed, vec![])
}

/// `get_document`: overview with `chunk_ids`.
pub fn project_document_overview(env: Value) -> ToolOutcome {
    let path = env.pointer("/document/source_path").and_then(Value::as_str).unwrap_or("(unknown)");
    let name = env.pointer("/source/display_name").and_then(Value::as_str).unwrap_or("");
    let id = env.pointer("/document/id").and_then(Value::as_str).unwrap_or("?");
    let n = env.get("chunk_ids").and_then(Value::as_array).map_or(0, Vec::len);
    let summary = format!("{path} ({name}): {n} chunks.");
    let next_actions = vec![
        NextAction { tool: "get_document_full", arguments: json!({ "id": id }) },
        NextAction { tool: "get_document_chunks", arguments: json!({ "id": id }) },
    ];
    let trimmed = json!({ "source_path": path, "chunk_count": n });
    ToolOutcome::new(summary, env, trimmed, next_actions)
}

/// `get_document_full`: full document with inline `chunks`.
pub fn project_document_full(env: Value) -> ToolOutcome {
    let path = env.pointer("/document/source_path").and_then(Value::as_str).unwrap_or("(unknown)");
    let chunks = env.get("chunks").and_then(Value::as_array);
    let n = chunks.map_or(0, Vec::len);
    let chars: usize = chunks.map(|c| c.iter()
        .filter_map(|x| x.get("content").and_then(Value::as_str)).map(str::len).sum()).unwrap_or(0);
    let summary = format!("Full {path}: {n} chunks (~{chars} chars).");
    let trimmed = json!({ "source_path": path, "chunk_count": n, "char_count": chars });
    ToolOutcome::new(summary, env, trimmed, vec![])
}

/// `get_document_chunks`: windowed slice `{ chunks, from, limit, total_chunks, .. }`.
pub fn project_document_window(env: Value) -> ToolOutcome {
    let path = env.pointer("/document/source_path").and_then(Value::as_str).unwrap_or("(unknown)");
    let from = env.get("from").and_then(Value::as_u64).unwrap_or(0);
    let n = env.get("chunks").and_then(Value::as_array).map_or(0, Vec::len) as u64;
    let total = env.get("total_chunks").and_then(Value::as_u64).unwrap_or(0);
    let id = env.pointer("/document/id").and_then(Value::as_str).unwrap_or("?").to_owned();
    let to = from + n;
    let summary = format!("Chunks {from}..{to} of {path} (of {total}).");
    let next_actions = vec![
        NextAction { tool: "get_document_chunks", arguments: json!({ "id": id, "from": to }) },
    ];
    let trimmed = json!({ "source_path": path, "from": from, "to": to, "total_chunks": total });
    ToolOutcome::new(summary, env, trimmed, next_actions)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p mn-mcp --lib render::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-mcp/src/render.rs
git commit -m "feat(mn-mcp): chunk + document projectors"
```

### Task 2.5: Sources / facets / status / pull_models / install projectors

**Files:**
- Modify: `crates/mn-mcp/src/render.rs`
- Test: `crates/mn-mcp/src/render.rs`

`list_sources` returns `{ sources: [ { slug, display_name, .. } ] }` (confirm key). `facets`
returns `{ <dim>: [ { value, count } ], .. }`. `status` is `StatusOutput`, `pull_models` is
`PullModelsOutput`, `install_search_skill` returns a JSON string today (Task 2.8 parses it; the
projector takes the already-parsed `Value`).

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn project_sources_lists_names() {
    let env = json!({ "sources": [ { "slug": "compact-docs", "display_name": "Compact Docs" },
                                    { "slug": "midnight-js", "display_name": "Midnight JS" } ] });
    let o = super::project_sources(env);
    assert!(o.summary.contains("2 sources"));
    assert!(o.next_actions.iter().any(|a| a.tool == "search"));
}

#[test]
fn project_status_reports_state() {
    let env = json!({ "server_version": "0.1.0", "reranker": "bge-reranker-base",
                      "model_state": "ready", "cache_dir": null });
    let o = super::project_status(env);
    assert!(o.summary.to_lowercase().contains("reranker"));
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo test -p mn-mcp --lib render::tests::project_sources_lists_names`
Expected: FAIL.

- [ ] **Step 3: Implement the projectors**

```rust
/// `list_sources`.
pub fn project_sources(env: Value) -> ToolOutcome {
    let sources = env.get("sources").and_then(Value::as_array).cloned().unwrap_or_default();
    let n = sources.len();
    let names: Vec<&str> = sources.iter()
        .filter_map(|s| s.get("display_name").and_then(Value::as_str)
            .or_else(|| s.get("slug").and_then(Value::as_str))).collect();
    let summary = format!("{n} sources: {}.", names.join(", "));
    let trimmed = json!({ "count": n, "sources": names });
    ToolOutcome::new(summary, env, trimmed, vec![
        NextAction { tool: "search", arguments: json!({ "query": "<terms>" }) },
    ])
}

/// `facets`.
pub fn project_facets(env: Value) -> ToolOutcome {
    let dims: Vec<String> = env.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    let summary = format!("Facets across {} dimension(s): {}.", dims.len(), dims.join(", "));
    let trimmed = json!({ "dimensions": dims });
    ToolOutcome::new(summary, env.clone(), trimmed, vec![
        NextAction { tool: "search", arguments: json!({ "query": "<terms>", "filters": {} }) },
    ])
}

/// `status` (StatusOutput as JSON).
pub fn project_status(env: Value) -> ToolOutcome {
    let reranker = env.get("reranker").and_then(Value::as_str).unwrap_or("?");
    let state = env.get("model_state").and_then(Value::as_str).unwrap_or("?");
    let ver = env.get("server_version").and_then(Value::as_str).unwrap_or("?");
    let summary = format!("Server {ver}; reranker {reranker}; model state {state}.");
    let mut next = vec![];
    if state != "ready" {
        next.push(NextAction { tool: "pull_models", arguments: json!({}) });
    }
    ToolOutcome::new(summary, env.clone(), env, next)
}

/// `pull_models` (PullModelsOutput as JSON).
pub fn project_pull_models(env: Value) -> ToolOutcome {
    let reranker = env.get("reranker").and_then(Value::as_str).unwrap_or("?");
    let loaded = env.get("reranker_loaded").and_then(Value::as_bool).unwrap_or(false);
    let summary = format!("Models pulled. Reranker {reranker} {}.", if loaded { "ready" } else { "not loaded" });
    ToolOutcome::new(summary, env.clone(), env, vec![
        NextAction { tool: "status", arguments: json!({}) },
    ])
}

/// `install_search_skill` (already-parsed result Value, e.g. `{ installed: [..], scope, .. }`).
pub fn project_install(env: Value) -> ToolOutcome {
    let scope = env.get("scope").and_then(Value::as_str).unwrap_or("user");
    let installed = env.get("installed").and_then(Value::as_array).map_or(0, Vec::len);
    let summary = format!("Installed search skill for {installed} harness(es) (scope: {scope}).");
    ToolOutcome::new(summary, env.clone(), env, vec![])
}
```

> If `run_install_search_skill` returns a bare confirmation string rather than a JSON object,
> wrap it as `json!({ "message": <string> })` in the dispatch (Task 2.8) before calling
> `project_install`, and have the projector read `message`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p mn-mcp --lib render::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-mcp/src/render.rs
git commit -m "feat(mn-mcp): sources/facets/status/pull_models/install projectors"
```

### Task 2.6: Output schemas module

**Files:**
- Create: `crates/mn-mcp/src/schemas.rs`
- Modify: `crates/mn-mcp/src/lib.rs` (`mod schemas;`)
- Test: `crates/mn-mcp/src/schemas.rs`

- [ ] **Step 1: Write the schema functions + a self-consistency test**

Create `crates/mn-mcp/src/schemas.rs`. Define shared fragments and one schema per tool. All
success `structuredContent` payloads must validate against these, so keep them permissive
(`additionalProperties: true`) except where we assert specific keys.

```rust
//! `outputSchema` JSON Schemas advertised per tool (MCP). `structuredContent`
//! on a success result conforms to the matching schema; conformance is asserted
//! in tests.

use serde_json::{json, Value};

fn chunk_fragment() -> Value {
    json!({
        "type": "object",
        "properties": {
            "chunk_id": { "type": "string" },
            "document_id": { "type": "string" },
            "source_slug": { "type": "string" },
            "source_display_name": { "type": "string" },
            "source_path": { "type": "string" },
            "heading_path": { "type": "array", "items": { "type": "string" } },
            "symbol_path": { "type": "array", "items": { "type": "string" } },
            "content": { "type": "string" }
        },
        "additionalProperties": true
    })
}

fn next_actions_fragment() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": { "tool": { "type": "string" }, "arguments": { "type": "object" } },
            "required": ["tool"]
        }
    })
}

pub fn search_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "corpus_embedding_model": { "type": "string" },
            "results": { "type": "array", "items": chunk_fragment() },
            "search_metadata": { "type": "object", "additionalProperties": true },
            "next_actions": next_actions_fragment()
        },
        "required": ["results"],
        "additionalProperties": true
    })
}

fn passthrough_object_schema() -> Value {
    json!({ "type": "object", "additionalProperties": true, "properties": { "next_actions": next_actions_fragment() } })
}

pub fn chunk_output_schema() -> Value { passthrough_object_schema() }
pub fn chunk_list_output_schema() -> Value { passthrough_object_schema() }
pub fn neighbors_output_schema() -> Value { passthrough_object_schema() }
pub fn document_output_schema() -> Value { passthrough_object_schema() }
pub fn sources_output_schema() -> Value { passthrough_object_schema() }
pub fn facets_output_schema() -> Value { passthrough_object_schema() }
pub fn status_output_schema() -> Value { passthrough_object_schema() }
pub fn pull_models_output_schema() -> Value { passthrough_object_schema() }
pub fn install_output_schema() -> Value { passthrough_object_schema() }

/// `get_chunk_parents` returns a JSON array at the top level (no envelope object).
pub fn parents_output_schema() -> Value {
    json!({ "type": "object", "additionalProperties": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_valid_json_schema_objects() {
        for s in [
            search_output_schema(), chunk_output_schema(), document_output_schema(),
            sources_output_schema(), facets_output_schema(), status_output_schema(),
        ] {
            // Compiles as a schema (catches malformed schema definitions).
            jsonschema::validator_for(&s).expect("schema compiles");
        }
    }
}
```

> Note: `get_chunk_parents` and `get_chunk_neighbors` return arrays/composites; the projectors
> wrap them (parents stays an array in `structured`). Keep `parents_output_schema` permissive,
> or have `project_parents` wrap the array as `{ "parents": [...] }` in `structured` so it stays
> an object (recommended — adjust Task 2.4's `project_parents` to set
> `structured: json!({ "parents": env })`). Pick one and keep schema + projector consistent.

- [ ] **Step 2: Register the module + run**

Add `mod schemas;` to `lib.rs`. Run:
`cargo test -p mn-mcp --lib schemas::tests`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/mn-mcp/src/schemas.rs crates/mn-mcp/src/lib.rs
git commit -m "feat(mn-mcp): per-tool output schemas"
```

### Task 2.7: Advertise `output_schema` in `list()`

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs` (`list()` ~45-150)
- Test: `crates/mn-mcp/src/tools.rs`

- [ ] **Step 1: Failing test — every tool advertises an output schema**

Add to the `tools.rs` test module:

```rust
#[test]
fn every_tool_advertises_output_schema() {
    for t in list().tools {
        assert!(t.output_schema.is_some(), "tool {} missing outputSchema", t.name);
    }
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo test -p mn-mcp --lib every_tool_advertises_output_schema`
Expected: FAIL — all are `None`.

- [ ] **Step 3: Fill each `output_schema`**

In `list()`, set `output_schema` on each `ToolDescription` to `Some(crate::schemas::<fn>())`:

| Tool | schema fn |
|---|---|
| `search` | `search_output_schema()` |
| `get_chunk` | `chunk_output_schema()` |
| `get_chunk_next` / `get_chunk_prev` | `chunk_list_output_schema()` |
| `get_chunk_neighbors` | `neighbors_output_schema()` |
| `get_chunk_parents` | `parents_output_schema()` |
| `get_document` / `get_document_full` / `get_document_chunks` | `document_output_schema()` |
| `list_sources` | `sources_output_schema()` |
| `facets` | `facets_output_schema()` |
| `pull_models` | `pull_models_output_schema()` |
| `status` | `status_output_schema()` |
| `install_search_skill` | `install_output_schema()` |

Example for one entry:

```rust
ToolDescription {
    name: "search",
    description: "...",
    input_schema: search_input_schema(),
    output_schema: Some(crate::schemas::search_output_schema()),
},
```

- [ ] **Step 4: Run**

Run: `cargo test -p mn-mcp --lib every_tool_advertises_output_schema`
Expected: PASS.

- [ ] **Step 5: Update the `mcp-tools.json` contract**

In `specs/001-rag-platform/contracts/mcp-tools.json`, add an `outputSchema` key mirroring each
tool's schema (copy the JSON produced by the matching `schemas.rs` fn). Keep it in sync with the
Rust source of truth.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-mcp/src/tools.rs specs/001-rag-platform/contracts/mcp-tools.json
git commit -m "feat(mn-mcp): advertise outputSchema per tool + update mcp-tools.json"
```

### Task 2.8: Rewire `server.rs` dispatch through projectors + failures

**Files:**
- Modify: `crates/mn-mcp/src/server.rs` (`dispatch_tool` ~220-262, `dispatch_tool_inner` ~284-336, `run_search_dispatch` ~338-364, `run_passthrough` ~454-482, `mismatch_response`/`too_many_chunks_response`)
- Test: `crates/mn-mcp/tests/tools_dispatch.rs`

This is the integration point. Goal: each tool runs, its result/error is projected into a
`ToolCallResult` (success or `isError`), wrapped in `Response::success`. Only unknown-tool stays
a JSON-RPC error. The search telemetry facts flow back to `dispatch_tool`.

- [ ] **Step 1: Introduce a `ToolResponse` carrier**

At the top of `server.rs` (or in `render.rs`), add:

```rust
/// What `dispatch_tool_inner` hands back to the telemetry-aware caller.
struct ToolResponse {
    result: crate::protocol::ToolCallResult,
    telemetry: Option<crate::render::SearchTelemetry>,
    outcome: mn_telemetry::events::Outcome,
}
```

- [ ] **Step 2: Rewrite `dispatch_tool_inner` to return `ToolResponse` or a protocol `Response`**

Change its signature to `-> Result<ToolResponse, Response>` (the `Err` is reserved for unknown
tool only). For each tool, call the run fn, then map Ok → projector → `ToolOutcome::into_result`,
and Err → a `ToolFailure` builder → `into_result`. Use `mn_telemetry::events::Outcome` for the
event. Concrete shape:

```rust
async fn dispatch_tool_inner(
    id: RequestId,
    params: ToolCallParams,
    state: &ServerState,
) -> Result<ToolResponse, Response> {
    use mn_telemetry::events::Outcome;
    use crate::render::{self, ToolFailure, ErrorKind, NextAction};

    let ok = |result, telemetry| ToolResponse { result, telemetry, outcome: Outcome::Ok };
    let err = |result, outcome| ToolResponse { result, telemetry: None, outcome };

    Ok(match params.name.as_str() {
        "status" => {
            let out = tools::run_status(Some(&state.cfg.cache_dir));
            let v = serde_json::to_value(out).unwrap_or(serde_json::Value::Null);
            ok(render::project_status(v).into_result(), None)
        }
        "pull_models" => match tools::run_pull_models(state.cfg.cache_dir.clone()).await {
            Ok(out) => {
                let v = serde_json::to_value(out).unwrap_or(serde_json::Value::Null);
                ok(render::project_pull_models(v).into_result(), None)
            }
            Err(msg) => err(
                ToolFailure::simple(ErrorKind::ModelLoadFailed, msg, "Model download failed; retry pull_models.").into_result(),
                Outcome::Error,
            ),
        },
        "search" => return Ok(run_search_dispatch(&params, state).await),
        "get_chunk" | "get_chunk_next" | "get_chunk_prev" | "get_chunk_neighbors"
        | "get_chunk_parents" | "get_document" | "get_document_full" | "get_document_chunks" =>
            return Ok(run_passthrough_tool(&params, state).await),
        "list_sources" => match state.cloud.list_sources().await {
            Ok(v) => ok(render::project_sources(v).into_result(), None),
            Err(e) => err(cloud_failure(&e).into_result(), Outcome::Error),
        },
        "facets" => match state.cloud.get_facets().await {
            Ok(v) => ok(render::project_facets(v).into_result(), None),
            Err(e) => err(cloud_failure(&e).into_result(), Outcome::Error),
        },
        "install_search_skill" => match tools::run_install_search_skill(&params.arguments) {
            Ok(text) => {
                let v = serde_json::from_str::<serde_json::Value>(&text)
                    .unwrap_or_else(|_| serde_json::json!({ "message": text }));
                ok(render::project_install(v).into_result(), None)
            }
            Err((_, msg)) => err(
                ToolFailure::simple(ErrorKind::InstallFailed, msg.clone(), msg).into_result(),
                Outcome::InvalidInput,
            ),
        },
        other => return Err(Response::err(id, ErrorCode::ToolNotFound, format!("unknown tool: {other}"))),
    })
}
```

Add the helper:

```rust
fn cloud_failure(e: &CloudError) -> crate::render::ToolFailure {
    use crate::render::{ErrorKind, ToolFailure};
    match e {
        CloudError::NotFound(msg) => ToolFailure {
            kind: ErrorKind::NotFound,
            message: format!("not found: {msg}"),
            guidance: "Resource not found — verify the id from a recent search result.".into(),
            details: serde_json::Value::Null,
            next_actions: vec![crate::render::NextAction { tool: "search", arguments: serde_json::json!({ "query": "<terms>" }) }],
        },
        other => ToolFailure::simple(ErrorKind::CloudError, other.to_string(), "Upstream call failed; retry shortly."),
    }
}
```

- [ ] **Step 3: Rewrite `run_search_dispatch` to project + carry telemetry**

```rust
async fn run_search_dispatch(params: &ToolCallParams, state: &ServerState) -> ToolResponse {
    use mn_telemetry::events::Outcome;
    use crate::render::{self, ToolFailure, ErrorKind, NextAction};

    // Did local rerank run? (search default is true; see parse_search_args.)
    let rerank_on = params.arguments.get("rerank").and_then(serde_json::Value::as_bool).unwrap_or(true);
    let reranker = if rerank_on { Some(state.cfg.reranker_name()) } else { None };

    match tools::run_search(&params.arguments, &state.cfg, &state.cloud).await {
        Ok(envelope) => {
            let outcome = render::project_search(envelope, reranker.as_deref());
            let telemetry = outcome.telemetry.clone();
            ToolResponse { result: outcome.into_result(), telemetry, outcome: Outcome::Ok }
        }
        Err(tools::SearchError::InvalidInput(msg)) => ToolResponse {
            result: ToolFailure::simple(ErrorKind::InvalidInput, msg.clone(), msg).into_result(),
            telemetry: None, outcome: Outcome::InvalidInput,
        },
        Err(tools::SearchError::Mismatch { corpus_model, client_model, message, remediation }) => ToolResponse {
            result: ToolFailure {
                kind: ErrorKind::EmbeddingModelMismatch,
                message,
                guidance: remediation.clone(),
                details: serde_json::json!({ "corpus_model": corpus_model, "client_model": client_model, "remediation": remediation }),
                next_actions: vec![NextAction { tool: "pull_models", arguments: serde_json::json!({}) }],
            }.into_result(),
            telemetry: None, outcome: Outcome::Error,
        },
        Err(tools::SearchError::Cloud(msg)) => ToolResponse {
            result: ToolFailure::simple(ErrorKind::CloudError, msg, "Search failed upstream; retry shortly.").into_result(),
            telemetry: None, outcome: Outcome::Error,
        },
    }
}
```

> `SearchTelemetry` must derive `Clone` (it does, Task 2.3). `state.cfg.reranker_name()` —
> use whatever exposes the configured reranker name; if none exists, read it from `run_status`
> output or hardcode the constant the config resolves to. Confirm the accessor and adjust.

- [ ] **Step 4: Rewrite `run_passthrough_tool` to project per-tool + map errors**

Replace the `Result<String, Response>` plumbing. Each arm runs its future, then on Ok calls the
matching projector, on Err maps `PassthroughError` → `ToolFailure`. Shared error mapper:

```rust
fn passthrough_failure(e: tools::PassthroughError) -> crate::render::ToolFailure {
    use crate::render::{ErrorKind, ToolFailure, NextAction};
    use serde_json::json;
    match e {
        tools::PassthroughError::InvalidInput(msg) => ToolFailure::simple(ErrorKind::InvalidInput, msg.clone(), msg),
        tools::PassthroughError::NotFound(msg) => ToolFailure {
            kind: ErrorKind::NotFound,
            message: format!("not found: {msg}"),
            guidance: "Not found — verify the id from a recent search result.".into(),
            details: json!({}),
            next_actions: vec![NextAction { tool: "search", arguments: json!({ "query": "<terms>" }) }],
        },
        tools::PassthroughError::TooManyChunks { chunk_count, cap, hint } => ToolFailure {
            kind: ErrorKind::TooManyChunks,
            message: format!("document has {chunk_count} chunks (cap {cap})"),
            guidance: hint.clone(),
            details: json!({ "chunk_count": chunk_count, "cap": cap, "hint": hint }),
            next_actions: vec![NextAction { tool: "get_document_chunks", arguments: json!({ "from": 0, "limit": 20 }) }],
        },
        tools::PassthroughError::Cloud(msg) => ToolFailure::simple(ErrorKind::CloudError, msg, "Upstream call failed; retry shortly."),
    }
}
```

The dispatch arm (one per tool) — for example `get_chunk`:

```rust
"get_chunk" => {
    let r = tools::run_passthrough_id(args, cloud, tools::PassthroughKind::Chunk).await;
    match r {
        Ok(v) => ToolResponse { result: render::project_chunk(v).into_result(), telemetry: None, outcome: Outcome::Ok },
        Err(e) => ToolResponse { result: passthrough_failure(e).into_result(), telemetry: None, outcome: outcome_of(&e_kind) },
    }
}
```

Wire each tool to its projector: `get_chunk` → `project_chunk`; `get_chunk_next` →
`project_chunk_list(v, "after")`; `get_chunk_prev` → `project_chunk_list(v, "before")`;
`get_chunk_neighbors` → `project_neighbors`; `get_chunk_parents` → `project_parents`;
`get_document` → `project_document_overview`; `get_document_full` → `project_document_full`;
`get_document_chunks` → `project_document_window`. For the `outcome`, map `InvalidInput` →
`Outcome::InvalidInput`, everything else → `Outcome::Error` (compute it from the error before
moving it into `passthrough_failure`, since `PassthroughError` is consumed).

- [ ] **Step 5: Rewrite `dispatch_tool` to consume `ToolResponse` and emit telemetry**

```rust
async fn dispatch_tool(id: RequestId, params: ToolCallParams, state: &ServerState) -> Response {
    use mn_telemetry::events::{Component, EventPayload, ModelState, Outcome};
    let started = Instant::now();
    let name_for_event = tool_name_for_event(&params.name);
    let rerank_on = params.name == "search"
        && params.arguments.get("rerank").and_then(serde_json::Value::as_bool).unwrap_or(true);

    let (response, telemetry, outcome) = match dispatch_tool_inner(id.clone(), params, state).await {
        Ok(tr) => (Response::success(id, serde_json::to_value(tr.result).expect("serialize result")), tr.telemetry, tr.outcome),
        Err(resp) => (resp, None, Outcome::Error),
    };

    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    state.tools_served.fetch_add(1, Ordering::Relaxed);
    if let Some(name) = name_for_event {
        let t = telemetry.unwrap_or_default();
        state.telemetry.emit(Event::new(
            Component::Mcp,
            crate::VERSION,
            EventPayload::McpToolCall {
                tool_name: name,
                latency_ms,
                result_count: t.result_count,
                model_state: ModelState::Missing,
                rerank_on,
                outcome,
                corpus_model: t.corpus_model,
                reranker_used: t.reranker_used,
                top_confidence: t.top_confidence_bucket.map(str::to_owned),
                top_attribution: t.top_attribution,
                top_source: t.top_source,
                filtered_by_confidence: t.filtered_by_confidence,
                deduplicated_count: t.deduplicated_count,
            },
        )).await;
    }
    response
}
```

> The new `McpToolCall` fields land in Task 3.1 — this step won't compile until then. That's
> fine for plan order; if you execute strictly, do Task 3.1 first (it's additive and
> independent) then return here. The `SearchTelemetry: Default` derive (Task 2.3) makes
> `unwrap_or_default()` valid.

- [ ] **Step 6: Delete the now-dead helpers**

Remove `mismatch_response`, `too_many_chunks_response`, `TooManyChunksPolicy`, the old
`run_passthrough`, and the old `Ok(result_text) => ToolCallResult { ... }` wrapping. Run the
compiler to find dead code:
Run: `cargo build -p mn-mcp`
Expected: clean after removing unused items.

- [ ] **Step 7: Rewrite the dispatch tests**

In `tests/tools_dispatch.rs`, the old assertions read the raw dump (`v["id"]`, `v["chunks"]`,
`v["from"]`, `v["chunk"]["id"]`, `v["prev"]["chunks"]`). Those JSON shapes now live under
`structuredContent`, not the `content` text. Update each test to parse the tool result and
assert on `structuredContent`. Example pattern (adapt per test):

```rust
// before: let v: Value = serde_json::from_str(&result.content[0].text).unwrap();
// after:
let sc = result.structured_content.as_ref().expect("structuredContent");
assert_eq!(sc["chunk"]["id"], id);          // get_chunk
assert!(!result.is_error);
```

For the neighbors test, assert `sc["prev"]["chunks"]` etc. (the full envelope is preserved in
`structuredContent`). For `get_document_chunks`, assert `sc["from"] == 3 && sc["limit"] == 7`.

- [ ] **Step 8: Run dispatch tests**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp --test tools_dispatch`
Expected: PASS. (Unset `VOYAGE_API_KEY` so BYOK-path tests behave — sandbox note.)

- [ ] **Step 9: Commit**

```bash
git add crates/mn-mcp/src/server.rs crates/mn-mcp/tests/tools_dispatch.rs
git commit -m "feat(mn-mcp): dispatch through projectors + isError envelopes; carry search telemetry"
```

### Task 2.9: Conformance + shape tests

**Files:**
- Create: `crates/mn-mcp/tests/result_shape.rs`
- Test: same

- [ ] **Step 1: Write conformance + shape tests**

```rust
//! Asserts the new result contract: success = one text block (summary + fenced
//! json) + structuredContent conforming to outputSchema; failure = isError.

use serde_json::Value;

#[test]
fn search_structured_conforms_to_output_schema() {
    // Build a representative structured payload via the projector.
    let env = serde_json::json!({
        "corpus_embedding_model": "voyage-code-3@1",
        "results": [{ "chunk_id": "a", "document_id": "b", "source_path": "docs/x.md",
                      "source_slug": "s", "source_display_name": "S", "heading_path": [],
                      "symbol_path": [], "content": "c",
                      "scores": { "confidence": 0.9, "trust_score": 1.0,
                                  "confidence_factors": { "attribution": "foundation", "verified": true } } }],
        "search_metadata": { "filtered_by_confidence": 0, "deduplicated_count": 0 }
    });
    let result = mn_mcp::render::project_search(env, None).into_result();
    let sc = result.structured_content.as_ref().unwrap();
    let schema = mn_mcp::schemas::search_output_schema();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(sc), "search structuredContent must conform to outputSchema");

    // text block = summary + fenced json
    let text = match &result.content[0] { mn_mcp::protocol::ContentBlock::Text { text } => text };
    assert!(text.contains("```json"));
    assert!(!result.is_error);
}
```

> This requires `render`, `schemas`, and `protocol` to be reachable from integration tests.
> Make them `pub mod` in `lib.rs` (they are crate-internal today). Add `pub mod render; pub mod
> schemas;` and ensure `protocol`/`ContentBlock` are `pub`. If you prefer to keep them private,
> move these assertions into `#[cfg(test)]` unit tests inside `render.rs`/`schemas.rs` instead.

- [ ] **Step 2: Make the modules reachable (if using an integration test)**

In `lib.rs`, change `mod render;`/`mod schemas;` to `pub mod render;`/`pub mod schemas;` and
confirm `pub mod protocol;`.

- [ ] **Step 3: Run**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp --test result_shape`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/mn-mcp/tests/result_shape.rs crates/mn-mcp/src/lib.rs
git commit -m "test(mn-mcp): outputSchema conformance + result-shape tests"
```

### Task 2.10: Full mn-mcp CI surface

- [ ] **Step 1: Run the full gate**

Run: `VOYAGE_API_KEY= cargo clippy -p mn-mcp --all-targets --all-features -- -D warnings && VOYAGE_API_KEY= cargo test -p mn-mcp && cargo fmt --check`
Expected: PASS, no warnings. Fix any fallout (e.g. `server_loop.rs` schema/shape assertions —
update the `input_schema` checks if needed; they should be unaffected, but the `status` output
shape test at `server_loop.rs:148` may need to read the projected result).

- [ ] **Step 2: Commit any fixes**

```bash
git add -A && git commit -m "chore(mn-mcp): clippy/fmt + residual test fixes for new result shape"
```

---

# PHASE 3 — Telemetry capture

### Task 3.1: Add additive fields to `McpToolCall`

**Files:**
- Modify: `crates/mn-telemetry/src/events.rs` (`McpToolCall` variant ~77-91)
- Test: `crates/mn-telemetry/src/events.rs`

- [ ] **Step 1: Failing round-trip test**

Add to the `events.rs` test module:

```rust
#[test]
fn mcp_tool_call_serializes_new_search_fields() {
    let e = Event::new(Component::Mcp, "0.1.0", EventPayload::McpToolCall {
        tool_name: McpToolName::Search,
        latency_ms: 12,
        result_count: 3,
        model_state: ModelState::Ready,
        rerank_on: true,
        outcome: Outcome::Ok,
        corpus_model: Some("voyage-code-3@1".into()),
        reranker_used: Some("bge-reranker-base".into()),
        top_confidence: Some("high".into()),
        top_attribution: Some("foundation".into()),
        top_source: Some("Compact Docs".into()),
        filtered_by_confidence: Some(0),
        deduplicated_count: Some(0),
    });
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["payload"]["corpus_model"], "voyage-code-3@1");
    assert_eq!(v["payload"]["top_source"], "Compact Docs");
}

#[test]
fn mcp_tool_call_omits_absent_search_fields_for_other_tools() {
    let e = Event::new(Component::Mcp, "0.1.0", EventPayload::McpToolCall {
        tool_name: McpToolName::GetChunk,
        latency_ms: 1, result_count: 0, model_state: ModelState::Missing,
        rerank_on: false, outcome: Outcome::Ok,
        corpus_model: None, reranker_used: None, top_confidence: None,
        top_attribution: None, top_source: None, filtered_by_confidence: None, deduplicated_count: None,
    });
    let v = serde_json::to_value(&e).unwrap();
    assert!(v["payload"].get("corpus_model").is_none());
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo test -p mn-telemetry --lib mcp_tool_call_serializes_new_search_fields`
Expected: FAIL — fields don't exist.

- [ ] **Step 3: Extend the `McpToolCall` variant**

Append to the variant (after `outcome: Outcome,`):

```rust
        /// Corpus embedding model id (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corpus_model: Option<String>,
        /// Reranker that actually ran (search only; `None` when rerank was off).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reranker_used: Option<String>,
        /// Coarse confidence bucket of the top result (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        top_confidence: Option<String>,
        /// Attribution tier of the top result (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        top_attribution: Option<String>,
        /// Display name of the top result's source (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        top_source: Option<String>,
        /// Count dropped below the confidence threshold (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filtered_by_confidence: Option<u32>,
        /// Count removed by dedup (search only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deduplicated_count: Option<u32>,
```

- [ ] **Step 4: Fix the existing test constructor**

`events.rs` already constructs `McpToolCall` in a test (~line 286). Add the seven `None`/`Some`
fields to that literal so it compiles.

- [ ] **Step 5: Run**

Run: `cargo test -p mn-telemetry --lib`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-telemetry/src/events.rs
git commit -m "feat(mn-telemetry): additive retrieval-quality fields on McpToolCall"
```

### Task 3.2: `telemetry_search_daily` migration

**Files:**
- Create: `crates/mn-store/migrations/0010_telemetry_search_daily.sql`

- [ ] **Step 1: Write the migration**

```sql
-- 0010 — telemetry_search_daily: dimensional rollup of search retrieval quality.
--
-- Preserves the retrieval-quality signal from mcp_tool_call (tool=search) events
-- past the raw-retention window. Populated by the sweep job before raw rows are
-- deleted. Retained indefinitely (like telemetry_aggregate_daily).

CREATE TABLE telemetry_search_daily (
    day              date NOT NULL,
    corpus_model     text NOT NULL DEFAULT '',
    attribution      text NOT NULL DEFAULT '',
    reranker         text NOT NULL DEFAULT '',
    top_source       text NOT NULL DEFAULT '',
    confidence_bucket text NOT NULL DEFAULT '',
    count            bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (day, corpus_model, attribution, reranker, top_source, confidence_bucket)
);

CREATE INDEX idx_telemetry_search_daily_day ON telemetry_search_daily (day);
```

- [ ] **Step 2: Verify it parses (offline)**

Run: `sqlx migrate info --source crates/mn-store/migrations`
Expected: lists `0010_telemetry_search_daily` as pending (no DB needed to list).

- [ ] **Step 3: Commit**

```bash
git add crates/mn-store/migrations/0010_telemetry_search_daily.sql
git commit -m "feat(mn-store): telemetry_search_daily dimensional rollup table"
```

### Task 3.3: Sweep populates the dimensional table

**Files:**
- Modify: `crates/mn-server/src/jobs/telemetry_sweep.rs` (`sweep_once`)
- Test: `crates/mn-server/src/jobs/telemetry_sweep.rs` (integration-gated)

- [ ] **Step 1: Add the second aggregate step inside the same transaction**

In `sweep_once`, **before** the `DELETE`, after the existing `telemetry_aggregate_daily` upsert,
add a dimensional rollup for expired search events. It reads dimensions out of the JSONB
`fields` payload (the `McpToolCall` fields are stored verbatim under `fields`):

```rust
    // Dimensional rollup: preserve retrieval-quality dimensions for expired
    // search events past raw retention. Missing fields coalesce to '' so the
    // primary key is always satisfiable.
    sqlx::query(
        "WITH expired_search AS (
             SELECT received_at::date AS day,
                    COALESCE(fields->>'corpus_model', '') AS corpus_model,
                    COALESCE(fields->>'top_attribution', '') AS attribution,
                    COALESCE(fields->>'reranker_used', '') AS reranker,
                    COALESCE(fields->>'top_source', '') AS top_source,
                    COALESCE(fields->>'top_confidence', '') AS confidence_bucket,
                    COUNT(*)::bigint AS c
             FROM telemetry_event_raw
             WHERE received_at < now() - make_interval(days => $1::int)
               AND event_type = 'mcp_tool_call'
               AND fields->>'tool_name' = 'search'
             GROUP BY 1, 2, 3, 4, 5, 6
         )
         INSERT INTO telemetry_search_daily (day, corpus_model, attribution, reranker, top_source, confidence_bucket, count)
         SELECT day, corpus_model, attribution, reranker, top_source, confidence_bucket, c FROM expired_search
         ON CONFLICT (day, corpus_model, attribution, reranker, top_source, confidence_bucket)
         DO UPDATE SET count = telemetry_search_daily.count + EXCLUDED.count",
    )
    .bind(retention_days)
    .execute(&mut *tx)
    .await?;
```

> `fields->>'tool_name'` — the `McpToolName` serializes `snake_case`, so search rows have
> `fields->>'tool_name' = 'search'`. Confirm against a real inserted row in the integration
> test below.

- [ ] **Step 2: Add an integration test (CI / Docker)**

Add behind the integration feature (mirroring existing sweep tests). It inserts a search event,
runs `sweep_once` with `retention_days = 0` (everything expires), and asserts a
`telemetry_search_daily` row exists with the expected dimensions:

```rust
#[cfg(feature = "integration")]
#[sqlx::test(migrations = "../mn-store/migrations")]
async fn sweep_rolls_search_dimensions(pool: PgPool) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO telemetry_event_raw (id, received_at, event_type, component, version, fields) \
         VALUES (gen_random_uuid(), now() - interval '1 day', 'mcp_tool_call', 'mcp', '0.1.0', $1)",
    )
    .bind(serde_json::json!({
        "tool_name": "search", "corpus_model": "voyage-code-3@1",
        "top_attribution": "foundation", "reranker_used": "bge-reranker-base",
        "top_source": "Compact Docs", "top_confidence": "high"
    }))
    .execute(&pool).await?;

    super::sweep_once(&pool, 0).await?;

    let count: i64 = sqlx::query_scalar(
        "SELECT count FROM telemetry_search_daily \
         WHERE corpus_model = 'voyage-code-3@1' AND attribution = 'foundation' \
           AND reranker = 'bge-reranker-base' AND top_source = 'Compact Docs' AND confidence_bucket = 'high'",
    ).fetch_one(&pool).await?;
    assert_eq!(count, 1);
    Ok(())
}
```

> Match the exact `#[sqlx::test]`/testcontainers harness the existing sweep tests use; adapt the
> attribute/signature to the crate's established pattern.

- [ ] **Step 3: Compile check (non-DB)**

Run: `cargo build -p mn-server`
Expected: PASS. (The DB test runs in CI per the integration-tests-CI-only note.)

- [ ] **Step 4: Commit**

```bash
git add crates/mn-server/src/jobs/telemetry_sweep.rs
git commit -m "feat(mn-server): sweep preserves search retrieval-quality dimensions"
```

### Task 3.4: Retention 7 → 90

**Files:**
- Modify: `crates/mn-server/src/config.rs` (default line ~176; env parse `map_or` ~243-246)
- Modify: `crates/mn-server/src/jobs/telemetry_sweep.rs` (`DEFAULT_RETENTION_DAYS` ~16)
- Test: `crates/mn-server/src/config.rs`

- [ ] **Step 1: Failing test for the new default**

Add to `config.rs` tests (or wherever defaults are tested):

```rust
#[test]
fn default_retention_is_ninety_days() {
    // With the env var unset, the default must be 90 (was 7; FR-110 deviation, recorded).
    std::env::remove_var("MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS");
    let cfg = ServerConfig::from_env().expect("config");
    assert_eq!(cfg.telemetry_raw_retention_days, 90);
}
```

> If `from_env` requires other env vars (e.g. `DATABASE_URL`), set them in the test or use the
> crate's existing config-test harness. If a `Default` impl exists, assert on that instead.

- [ ] **Step 2: Run; expect failure**

Run: `cargo test -p mn-server --lib default_retention_is_ninety_days`
Expected: FAIL — default is 7.

- [ ] **Step 3: Change the defaults**

- In the struct default block (line ~176): `telemetry_raw_retention_days: 90,`
- In the env parse (line ~243): change `.map_or(7, |v| v.clamp(1, 365))` to `.map_or(90, |v| v.clamp(1, 365))`
- In `telemetry_sweep.rs`: `pub const DEFAULT_RETENTION_DAYS: i64 = 90;`
- Update the doc comment on the config field (line ~46-50) and the migration header note to say
  "default 90".

- [ ] **Step 4: Run**

Run: `cargo test -p mn-server --lib default_retention_is_ninety_days`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-server/src/config.rs crates/mn-server/src/jobs/telemetry_sweep.rs
git commit -m "feat(mn-server): raise telemetry raw retention 7 -> 90 days"
```

### Task 3.5: Canary coverage for the new fields

**Files:**
- Modify: `crates/mn-telemetry/tests/canary_suite.rs`
- Test: same

The new fields are enums/buckets/counts/corpus-catalog names — structurally incapable of holding
query text or chunk content. Lock that with a test that serializes an `McpToolCall` whose search
fields are populated, alongside a canary-bearing query string in scope, and asserts no canary
appears in the serialized event.

- [ ] **Step 1: Add the test**

```rust
#[test]
fn mcp_tool_call_search_fields_do_not_leak_canary() {
    use mn_telemetry::events::{McpToolName, Outcome};
    // A canary query string is "in scope" but must NOT reach any event field.
    let _user_query = CANARY_STRINGS[0].value; // QueryText canary
    let event = Event::new(
        Component::Mcp, "0.1.0",
        EventPayload::McpToolCall {
            tool_name: McpToolName::Search, latency_ms: 5, result_count: 1,
            model_state: ModelState::Ready, rerank_on: true, outcome: Outcome::Ok,
            corpus_model: Some("voyage-code-3@1".into()),
            reranker_used: Some("bge-reranker-base".into()),
            top_confidence: Some("high".into()),
            top_attribution: Some("foundation".into()),
            top_source: Some("Compact Docs".into()),
            filtered_by_confidence: Some(0),
            deduplicated_count: Some(0),
        },
    );
    let wire = serde_json::to_string(&event).expect("serialize");
    canary::assert_no_canary_in(&wire);
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p mn-telemetry --test canary_suite`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/mn-telemetry/tests/canary_suite.rs
git commit -m "test(mn-telemetry): canary coverage for new McpToolCall search fields"
```

### Task 3.6: Workspace-wide gate

- [ ] **Step 1: Run the full pre-push check**

Run: `VOYAGE_API_KEY= cargo fmt --check && VOYAGE_API_KEY= cargo clippy --workspace --all-targets --all-features -- -D warnings && VOYAGE_API_KEY= cargo test --workspace`
Expected: PASS (DB integration tests are skipped locally; they run in CI per the integration-tests-CI-only note). Two `mn-cli` `auth_integration` loopback tests are known-failing in this sandbox — ignore those specific failures.

- [ ] **Step 2: Final commit**

```bash
git add -A && git commit -m "chore: workspace fmt/clippy after MCP response-format work"
```

---

## Self-Review

**Spec coverage:**
- Decision 1 (concise summary + trimmed-JSON fence + structuredContent): Tasks 2.1–2.5, 2.8. ✓
- Decision 2 (all tool failures → isError; JSON-RPC only for protocol faults): Task 2.8 (`ToolFailure`, `cloud_failure`, `passthrough_failure`, unknown-tool stays JSON-RPC). ✓
- Decision 3 (advertise outputSchema + conformance tests): Tasks 2.6, 2.7, 2.9. ✓
- Decision 4 (enrichment): Tasks 1.1–1.4. ✓
- Decision 5 (telemetry capture): Tasks 3.1, 3.3, 2.8 (emit site). ✓
- Decision 6 (extend rollup + retention 7→90): Tasks 3.2, 3.3, 3.4. ✓
- Canary safety: Task 3.5. ✓
- mcp-tools.json contract: Task 2.7 Step 5. ✓
- openapi contract: Task 1.4. ✓

**Placeholder scan:** `<terms>` appears only inside example `next_actions` arguments — these are
intentional caller-fill values surfaced to the agent, not plan placeholders. The few
"confirm against real serialization" notes are verification instructions, each with a concrete
fallback, not deferred work.

**Type consistency:** `SearchTelemetry` fields are produced in `project_search` (2.3), consumed
in `run_search_dispatch`/`dispatch_tool` (2.8), and mapped to the `McpToolCall` fields (3.1) by
identical names (`corpus_model`, `reranker_used`, `top_confidence`(bucket)→`top_confidence`,
`top_attribution`, `top_source`, `filtered_by_confidence`, `deduplicated_count`, `result_count`).
`ToolOutcome`/`ToolFailure`/`NextAction`/`ErrorKind` are defined once (2.2) and reused unchanged.
`structured_content`/`output_schema` field names match across 2.1, 2.2, 2.6, 2.9.

**Known cross-phase ordering:** Task 2.8 Step 5 references the `McpToolCall` fields added in 3.1.
If executing strictly in order, do Task 3.1 before 2.8 Step 5 (3.1 is additive and independent).
Noted inline.

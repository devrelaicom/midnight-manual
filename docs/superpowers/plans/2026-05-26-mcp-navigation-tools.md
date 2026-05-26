# MCP navigation tools implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the chunk + document navigation surface from the CLI (PR #51) to the MCP server, replacing the broken `get_chunk_siblings` tool with five new tools (`get_chunk_next`, `get_chunk_prev`, `get_document`, `get_document_full`, `get_document_chunks`) and surfacing the cloud's 412 `too_many_chunks` body as a typed JSON-RPC error.

**Architecture:** All work is in `mn-mcp` plus one enum bump in `mn-telemetry`. Pass-through pattern: each new tool wraps a cloud route 1:1, returning the cloud's JSON verbatim. The typed 412 envelope mirrors the existing `embedding_model_mismatch` precedent (`data.kind`, `data.next_tool`). Removal of `get_chunk_siblings` happens in a single final commit to keep intermediate states green.

**Tech Stack:** Rust 1.91 stable, tokio 1.x, reqwest 0.12, wiremock 0.6, serde_json 1.x, axum (only via dep types), `mn-telemetry` for the closed enum.

**Spec:** [`docs/superpowers/specs/2026-05-26-mcp-navigation-tools-design.md`](../specs/2026-05-26-mcp-navigation-tools-design.md)

**Hard rules for any subagent:** This is a Rust workspace. Only use `cargo`, `git`, `rg`, `grep`, `find`, `ls`, `cat`, `head`, `tail`, `sed`, `awk`, and the harness `Read`/`Edit`/`Write`/`Bash` tools. **Never** invoke `npx`, `npm`, `pnpm`, `yarn`, `pip`, `pipx`, `brew install`, `curl | sh`, or any other package installer or shell-pipe-installer. There is no JavaScript or Python in this workspace. Two prior subagents triggered supply-chain alerts by running `npx file@0.2.2` and `npx run@1.5.0`; treat that as a hard line — work that breaks this rule gets discarded.

**Per-task verification:** Run `cargo test -p mn-mcp` after each task. Run `cargo clippy -p mn-mcp --all-targets -- -D warnings` before each commit. `just check` (full workspace fmt + clippy + test) at the end.

---

## Task 1: Add `CloudError::TooManyChunks` variant and parser

**Files:**
- Modify: `crates/mn-mcp/src/cloud_client.rs`

- [ ] **Step 1: Write the failing unit tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/mn-mcp/src/cloud_client.rs`:

```rust
#[test]
fn parse_too_many_chunks_extracts_count_and_cap() {
    let body = serde_json::json!({
        "error": "too_many_chunks",
        "chunk_count": 1240,
        "cap": 500,
        "hint": "Use GET /v1/documents/abc/chunks?from=K&limit=L (default L=20)",
    })
    .to_string();
    let err = parse_too_many_chunks(body.as_bytes()).expect("typed too_many_chunks");
    match err {
        CloudError::TooManyChunks { chunk_count, cap, hint } => {
            assert_eq!(chunk_count, 1240);
            assert_eq!(cap, 500);
            assert!(hint.contains("/chunks?from="));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn parse_too_many_chunks_returns_none_for_unrelated_body() {
    let body = serde_json::json!({ "error": "something_else", "chunk_count": 10 }).to_string();
    assert!(parse_too_many_chunks(body.as_bytes()).is_none());
}

#[test]
fn parse_too_many_chunks_returns_none_for_missing_fields() {
    let body = serde_json::json!({ "error": "too_many_chunks", "cap": 500 }).to_string();
    assert!(parse_too_many_chunks(body.as_bytes()).is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mn-mcp --lib cloud_client::tests::parse_too_many_chunks
```

Expected: FAIL with `parse_too_many_chunks` not found / `TooManyChunks` variant not in scope.

- [ ] **Step 3: Add the variant and parser**

In `crates/mn-mcp/src/cloud_client.rs`, add a new variant to the `CloudError` enum (after `EmbeddingModelMismatch`, before `Status`):

```rust
    /// 412 from `/v1/documents/:id/full` — document exceeds the chunk cap.
    /// Surfaced specially so the MCP layer can emit a typed JSON-RPC error
    /// pointing the caller at `get_document_chunks`.
    #[error("document too many chunks: {chunk_count} (cap {cap})")]
    TooManyChunks {
        /// Reported ready-chunk count for the document.
        chunk_count: u32,
        /// Server's configured cap (currently 500).
        cap: u32,
        /// Operator-facing hint from the cloud (path to the windowing endpoint).
        hint: String,
    },
```

And add the parser at module level (next to `parse_mismatch`):

```rust
/// Parse the cloud's `{ "error": "too_many_chunks", "chunk_count": N, "cap": K, "hint": "..." }`
/// body (from `412 Precondition Failed` on `/v1/documents/:id/full`) into
/// [`CloudError::TooManyChunks`]. Returns `None` if the body shape doesn't
/// match — caller falls back to [`CloudError::Status`].
fn parse_too_many_chunks(body: &[u8]) -> Option<CloudError> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    if v.get("error")?.as_str()? != "too_many_chunks" {
        return None;
    }
    let chunk_count = u32::try_from(v.get("chunk_count")?.as_u64()?).ok()?;
    let cap = u32::try_from(v.get("cap")?.as_u64()?).ok()?;
    let hint = v
        .get("hint")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some(CloudError::TooManyChunks { chunk_count, cap, hint })
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mn-mcp --lib cloud_client::tests::parse_too_many_chunks
```

Expected: 3 passed.

- [ ] **Step 5: Verify no clippy warnings**

```bash
cargo clippy -p mn-mcp --all-targets -- -D warnings
```

Expected: clean. (The parser is unused outside tests but the test usage counts.)

- [ ] **Step 6: Commit**

```bash
git add crates/mn-mcp/src/cloud_client.rs
git commit -m "$(cat <<'EOF'
feat(mn-mcp): add CloudError::TooManyChunks variant for 412 from document-full

Adds a typed cloud error and parser for the document-too-large body
returned by GET /v1/documents/:id/full when the document exceeds the
500-chunk cap. The MCP layer will translate this into a typed JSON-RPC
error pointing agents at get_document_chunks.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `cloud_client.get_chunk_next` and `get_chunk_prev`

**Files:**
- Modify: `crates/mn-mcp/src/cloud_client.rs`
- Modify: `crates/mn-mcp/tests/cloud_client.rs`

- [ ] **Step 1: Write the failing wiremock tests**

Append to `crates/mn-mcp/tests/cloud_client.rs`:

```rust
#[tokio::test]
async fn get_chunk_next_sends_count_query() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000000a";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .and(wiremock::matchers::query_param("count", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "chunks": [{"id": "x", "chunk_index": 4}],
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_chunk_next(id, 7).await.unwrap();
    assert_eq!(v["chunks"][0]["chunk_index"], 4);
}

#[tokio::test]
async fn get_chunk_prev_sends_count_query() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000000b";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/prev")))
        .and(wiremock::matchers::query_param("count", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "chunks": [{"id": "y", "chunk_index": 0}],
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_chunk_prev(id, 3).await.unwrap();
    assert_eq!(v["chunks"][0]["chunk_index"], 0);
}

#[tokio::test]
async fn get_chunk_next_maps_404_to_not_found() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000000c";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.get_chunk_next(id, 5).await.unwrap_err();
    assert!(matches!(err, CloudError::NotFound(_)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mn-mcp --test cloud_client get_chunk_next get_chunk_prev
```

Expected: FAIL with no method `get_chunk_next`/`get_chunk_prev` on `CloudClient`.

- [ ] **Step 3: Implement the methods**

In `crates/mn-mcp/src/cloud_client.rs`, add inside `impl CloudClient` (immediately after `get_chunk`):

```rust
    /// `GET /v1/chunks/:id/next?count=N`.
    pub async fn get_chunk_next(&self, id: &str, count: u32) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/chunks/{id}/next?count={count}");
        self.get_json(&path).await
    }

    /// `GET /v1/chunks/:id/prev?count=N`.
    pub async fn get_chunk_prev(&self, id: &str, count: u32) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/chunks/{id}/prev?count={count}");
        self.get_json(&path).await
    }
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mn-mcp --test cloud_client get_chunk_next get_chunk_prev
```

Expected: 3 passed.

- [ ] **Step 5: Clippy clean**

```bash
cargo clippy -p mn-mcp --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/mn-mcp/src/cloud_client.rs crates/mn-mcp/tests/cloud_client.rs
git commit -m "$(cat <<'EOF'
feat(mn-mcp): add CloudClient::get_chunk_{next,prev} methods

Wraps GET /v1/chunks/:id/{next,prev}?count=N for chunk-anchored
navigation in chunk_index order. Pass-through serde_json::Value;
404 maps to CloudError::NotFound via the shared get_json helper.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add `cloud_client.get_document` and `get_document_full`

**Files:**
- Modify: `crates/mn-mcp/src/cloud_client.rs`
- Modify: `crates/mn-mcp/tests/cloud_client.rs`

`get_document` is a plain id-only GET — uses the shared `get_json` helper. `get_document_full` is special: it must detect HTTP 412 and translate the body into `CloudError::TooManyChunks` via the parser from Task 1.

- [ ] **Step 1: Write the failing wiremock tests**

Append to `crates/mn-mcp/tests/cloud_client.rs`:

```rust
#[tokio::test]
async fn get_document_round_trips() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000000d";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": id,
            "source_path": "welcome.md",
            "chunk_ids": ["a", "b"],
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_document(id).await.unwrap();
    assert_eq!(v["source_path"], "welcome.md");
    assert_eq!(v["chunk_ids"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn get_document_full_returns_body_on_200() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000000e";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/full")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": id,
            "chunks": [{"chunk_id": "a", "chunk_index": 0, "content": "hi"}],
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_document_full(id).await.unwrap();
    assert_eq!(v["chunks"][0]["content"], "hi");
}

#[tokio::test]
async fn get_document_full_maps_412_to_too_many_chunks() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000000f";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/full")))
        .respond_with(ResponseTemplate::new(412).set_body_json(serde_json::json!({
            "error": "too_many_chunks",
            "chunk_count": 1240,
            "cap": 500,
            "hint": "Use GET /v1/documents/abc/chunks?from=K&limit=L (default L=20)",
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.get_document_full(id).await.unwrap_err();
    match err {
        CloudError::TooManyChunks { chunk_count, cap, .. } => {
            assert_eq!(chunk_count, 1240);
            assert_eq!(cap, 500);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[tokio::test]
async fn get_document_full_falls_back_to_status_on_unrelated_412() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000001a";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/full")))
        .respond_with(ResponseTemplate::new(412).set_body_json(serde_json::json!({
            "error": "something_else",
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.get_document_full(id).await.unwrap_err();
    assert!(matches!(err, CloudError::Status { status: 412, .. }));
}

#[tokio::test]
async fn get_document_404_maps_to_not_found() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000001b";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}")))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.get_document(id).await.unwrap_err();
    assert!(matches!(err, CloudError::NotFound(_)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mn-mcp --test cloud_client get_document
```

Expected: FAIL with no method `get_document`/`get_document_full`.

- [ ] **Step 3: Implement the methods**

In `crates/mn-mcp/src/cloud_client.rs`, add inside `impl CloudClient` (after `get_chunk_prev` from Task 2):

```rust
    /// `GET /v1/documents/:id`.
    pub async fn get_document(&self, id: &str) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/documents/{id}");
        self.get_json(&path).await
    }

    /// `GET /v1/documents/:id/full`. Detects `412 Precondition Failed` and
    /// translates the cloud's `too_many_chunks` body into
    /// [`CloudError::TooManyChunks`]. Other non-2xx statuses fall through to
    /// the standard [`CloudError::NotFound`] / [`CloudError::Status`] mapping.
    pub async fn get_document_full(&self, id: &str) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/documents/{id}/full");
        let url = self
            .base
            .join(&path)
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        let mut rb = self.http.get(url);
        if let Some(b) = &self.bearer {
            rb = rb.bearer_auth(b);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<serde_json::Value>()
                .await
                .map_err(|e| CloudError::Decode(e.to_string()));
        }
        let body_bytes = resp.bytes().await.unwrap_or_default();
        if status == reqwest::StatusCode::PRECONDITION_FAILED {
            if let Some(typed) = parse_too_many_chunks(&body_bytes) {
                return Err(typed);
            }
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(CloudError::NotFound(String::from_utf8_lossy(&body_bytes).into_owned()));
        }
        Err(CloudError::Status {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&body_bytes).into_owned(),
        })
    }
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mn-mcp --test cloud_client get_document
```

Expected: 5 passed.

- [ ] **Step 5: Clippy clean**

```bash
cargo clippy -p mn-mcp --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/mn-mcp/src/cloud_client.rs crates/mn-mcp/tests/cloud_client.rs
git commit -m "$(cat <<'EOF'
feat(mn-mcp): add CloudClient::get_document{,_full} methods

Wraps GET /v1/documents/:id (overview) and GET /v1/documents/:id/full
(complete document, capped at 500 chunks). The full variant detects
HTTP 412 and translates the cloud's too_many_chunks body into the
typed CloudError::TooManyChunks variant added in the previous commit;
other non-2xx statuses use the standard NotFound / Status mapping.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add `cloud_client.get_document_chunks`

**Files:**
- Modify: `crates/mn-mcp/src/cloud_client.rs`
- Modify: `crates/mn-mcp/tests/cloud_client.rs`

- [ ] **Step 1: Write the failing wiremock tests**

Append to `crates/mn-mcp/tests/cloud_client.rs`:

```rust
#[tokio::test]
async fn get_document_chunks_sends_from_and_limit() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000001c";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .and(wiremock::matchers::query_param("from", "5"))
        .and(wiremock::matchers::query_param("limit", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "chunks": [],
            "from": 5,
            "limit": 20,
            "total_chunks": 5,
        })))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let v = client.get_document_chunks(id, 5, 20).await.unwrap();
    assert_eq!(v["from"], 5);
    assert_eq!(v["total_chunks"], 5);
}

#[tokio::test]
async fn get_document_chunks_404_maps_to_not_found() {
    let server = MockServer::start().await;
    let id = "00000000-0000-0000-0000-00000000001d";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;
    let client = CloudClient::new(&server.uri(), None).unwrap();
    let err = client.get_document_chunks(id, 0, 20).await.unwrap_err();
    assert!(matches!(err, CloudError::NotFound(_)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mn-mcp --test cloud_client get_document_chunks
```

Expected: FAIL with no method `get_document_chunks`.

- [ ] **Step 3: Implement the method**

In `crates/mn-mcp/src/cloud_client.rs`, add inside `impl CloudClient` (after `get_document_full`):

```rust
    /// `GET /v1/documents/:id/chunks?from=K&limit=N`.
    pub async fn get_document_chunks(
        &self,
        id: &str,
        from: u32,
        limit: u32,
    ) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/documents/{id}/chunks?from={from}&limit={limit}");
        self.get_json(&path).await
    }
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mn-mcp --test cloud_client get_document_chunks
```

Expected: 2 passed.

- [ ] **Step 5: Clippy clean**

```bash
cargo clippy -p mn-mcp --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/mn-mcp/src/cloud_client.rs crates/mn-mcp/tests/cloud_client.rs
git commit -m "$(cat <<'EOF'
feat(mn-mcp): add CloudClient::get_document_chunks method

Wraps GET /v1/documents/:id/chunks?from=K&limit=N for position-windowed
chunk reads. Defaults handled at the tools layer in a later task.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Add `PassthroughError::TooManyChunks` + `PassthroughKind::{Document, DocumentFull}`

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`
- Modify: `crates/mn-mcp/src/server.rs` (stub match arm; replaced in Task 10)
- Modify: `crates/mn-mcp/tests/tools_dispatch.rs`

- [ ] **Step 1: Write the failing dispatch tests**

Append to `crates/mn-mcp/tests/tools_dispatch.rs`:

```rust
#[tokio::test]
async fn run_passthrough_id_hits_document_endpoint() {
    let server = MockServer::start().await;
    let id = "11111111-1111-1111-1111-111111111100";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::Document)
        .await
        .unwrap();
    assert_eq!(v["id"], id);
}

#[tokio::test]
async fn run_passthrough_id_hits_document_full_endpoint() {
    let server = MockServer::start().await;
    let id = "11111111-1111-1111-1111-111111111101";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/full")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id, "chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::DocumentFull)
        .await
        .unwrap();
    assert_eq!(v["chunks"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn run_passthrough_id_maps_document_full_412() {
    let server = MockServer::start().await;
    let id = "11111111-1111-1111-1111-111111111102";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/full")))
        .respond_with(ResponseTemplate::new(412).set_body_json(json!({
            "error": "too_many_chunks",
            "chunk_count": 1240,
            "cap": 500,
            "hint": "Use GET /v1/documents/.../chunks?from=K&limit=L (default L=20)",
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::DocumentFull)
        .await
        .unwrap_err();
    match err {
        PassthroughError::TooManyChunks { chunk_count, cap, .. } => {
            assert_eq!(chunk_count, 1240);
            assert_eq!(cap, 500);
        }
        other => panic!("expected TooManyChunks, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mn-mcp --test tools_dispatch document
```

Expected: FAIL with `PassthroughKind::Document` and `PassthroughError::TooManyChunks` not found.

- [ ] **Step 3: Add variants and extend the match arm**

In `crates/mn-mcp/src/tools.rs`, edit the `PassthroughKind` enum:

```rust
/// Which cloud endpoint a pass-through tool should hit.
#[derive(Debug, Clone, Copy)]
pub enum PassthroughKind {
    /// `/v1/chunks/:id`
    Chunk,
    /// `/v1/chunks/:id/siblings` (DEPRECATED — removed in a later task).
    Siblings,
    /// `/v1/chunks/:id/parents`
    Parents,
    /// `/v1/documents/:id`
    Document,
    /// `/v1/documents/:id/full` (may return [`PassthroughError::TooManyChunks`]).
    DocumentFull,
}
```

Edit the `PassthroughError` enum to add a `TooManyChunks` variant:

```rust
/// Errors for the chunk pass-through tools.
#[derive(Debug)]
pub enum PassthroughError {
    /// `id` arg missing or malformed.
    InvalidInput(String),
    /// Cloud returned 404.
    NotFound(String),
    /// Cloud returned `412 too_many_chunks` (document-full only).
    TooManyChunks {
        /// Reported ready-chunk count for the document.
        chunk_count: u32,
        /// Server's configured cap.
        cap: u32,
        /// Operator-facing hint from the cloud.
        hint: String,
    },
    /// Cloud / transport / decode failure.
    Cloud(String),
}
```

Update `run_passthrough_id` to dispatch the two new kinds and translate the new `CloudError::TooManyChunks`:

```rust
pub async fn run_passthrough_id(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
    kind: PassthroughKind,
) -> Result<serde_json::Value, PassthroughError> {
    let id_str = args
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PassthroughError::InvalidInput("`id` (string) is required".to_owned()))?;
    Uuid::parse_str(id_str)
        .map_err(|e| PassthroughError::InvalidInput(format!("`id` is not a valid UUID: {e}")))?;
    let r = match kind {
        PassthroughKind::Chunk => cloud.get_chunk(id_str).await,
        PassthroughKind::Siblings => cloud.get_chunk_siblings(id_str).await,
        PassthroughKind::Parents => cloud.get_chunk_parents(id_str).await,
        PassthroughKind::Document => cloud.get_document(id_str).await,
        PassthroughKind::DocumentFull => cloud.get_document_full(id_str).await,
    };
    r.map_err(|e| match e {
        CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
        CloudError::TooManyChunks { chunk_count, cap, hint } => {
            PassthroughError::TooManyChunks { chunk_count, cap, hint }
        }
        other => PassthroughError::Cloud(other.to_string()),
    })
}
```

- [ ] **Step 4: Add a temporary stub arm in `server.rs::run_passthrough_dispatch`**

The existing match in `crates/mn-mcp/src/server.rs::run_passthrough_dispatch` becomes non-exhaustive once `PassthroughError::TooManyChunks` exists. Add a stub arm — Task 10 replaces it with the typed JSON-RPC error response. In the existing `run_passthrough_dispatch` function, insert a new arm between `NotFound` and `Cloud`:

```rust
        Err(tools::PassthroughError::TooManyChunks { chunk_count, cap, .. }) => {
            // Replaced with too_many_chunks_response in Task 10.
            Err(Response::err(
                id.clone(),
                ErrorCode::ToolFailed,
                format!("document has {chunk_count} chunks (cap {cap})"),
            ))
        }
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p mn-mcp --test tools_dispatch document
```

Expected: 3 passed.

- [ ] **Step 6: Clippy clean**

```bash
cargo clippy -p mn-mcp --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/mn-mcp/src/tools.rs crates/mn-mcp/src/server.rs crates/mn-mcp/tests/tools_dispatch.rs
git commit -m "$(cat <<'EOF'
feat(mn-mcp): add Document/DocumentFull PassthroughKinds + TooManyChunks error

Extends the pass-through dispatch helper to cover GET /v1/documents/:id
and /full. The /full path can surface CloudError::TooManyChunks (from
the previous commit), which the helper translates into a new
PassthroughError::TooManyChunks for the server layer to render as a
typed JSON-RPC error.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Add `ChunkNavDirection` enum + `run_chunk_nav` helper

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`
- Modify: `crates/mn-mcp/tests/tools_dispatch.rs`

- [ ] **Step 1: Write the failing dispatch tests**

Append to `crates/mn-mcp/tests/tools_dispatch.rs`. First, expand the imports at the top of the file:

```rust
use mn_mcp::tools::{
    run_chunk_nav, run_passthrough_id, ChunkNavDirection, PassthroughError, PassthroughKind,
};
```

Then append:

```rust
#[tokio::test]
async fn run_chunk_nav_next_uses_count_query_param() {
    let server = MockServer::start().await;
    let id = "22222222-2222-2222-2222-222222222200";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .and(wiremock::matchers::query_param("count", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_chunk_nav(
        &json!({"id": id, "count": 7}),
        &client,
        ChunkNavDirection::Next,
    )
    .await
    .unwrap();
    assert!(v["chunks"].is_array());
}

#[tokio::test]
async fn run_chunk_nav_defaults_count_to_five() {
    let server = MockServer::start().await;
    let id = "22222222-2222-2222-2222-222222222201";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/next")))
        .and(wiremock::matchers::query_param("count", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let _ = run_chunk_nav(&json!({"id": id}), &client, ChunkNavDirection::Next)
        .await
        .unwrap();
}

#[tokio::test]
async fn run_chunk_nav_prev_hits_prev_endpoint() {
    let server = MockServer::start().await;
    let id = "22222222-2222-2222-2222-222222222202";
    Mock::given(method("GET"))
        .and(path(format!("/v1/chunks/{id}/prev")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunks": []})))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let _ = run_chunk_nav(&json!({"id": id}), &client, ChunkNavDirection::Prev)
        .await
        .unwrap();
}

#[tokio::test]
async fn run_chunk_nav_rejects_count_zero() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_nav(
        &json!({"id": "22222222-2222-2222-2222-222222222203", "count": 0}),
        &client,
        ChunkNavDirection::Next,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_chunk_nav_rejects_count_over_max() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_nav(
        &json!({"id": "22222222-2222-2222-2222-222222222204", "count": 101}),
        &client,
        ChunkNavDirection::Next,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_chunk_nav_rejects_non_integer_count() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_nav(
        &json!({"id": "22222222-2222-2222-2222-222222222205", "count": "five"}),
        &client,
        ChunkNavDirection::Next,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_chunk_nav_rejects_invalid_uuid() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_chunk_nav(&json!({"id": "not-a-uuid"}), &client, ChunkNavDirection::Next)
        .await
        .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mn-mcp --test tools_dispatch run_chunk_nav
```

Expected: FAIL with `run_chunk_nav` and `ChunkNavDirection` not found.

- [ ] **Step 3: Add the enum and helper**

In `crates/mn-mcp/src/tools.rs`, near the top of the "pass-through tools" section, add:

```rust
/// Direction for `run_chunk_nav` — selects `/next` or `/prev`.
#[derive(Debug, Clone, Copy)]
pub enum ChunkNavDirection {
    /// `/v1/chunks/:id/next`
    Next,
    /// `/v1/chunks/:id/prev`
    Prev,
}

const CHUNK_NAV_DEFAULT_COUNT: u32 = 5;
const CHUNK_NAV_MAX_COUNT: u32 = 100;

/// Dispatch `get_chunk_next` / `get_chunk_prev`. Parses `{id, count?}` and
/// rejects out-of-range or non-integer `count` as `InvalidInput` before the
/// wire call.
///
/// # Errors
///
/// See [`PassthroughError`].
pub async fn run_chunk_nav(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
    dir: ChunkNavDirection,
) -> Result<serde_json::Value, PassthroughError> {
    let obj = args
        .as_object()
        .ok_or_else(|| PassthroughError::InvalidInput("arguments must be a JSON object".to_owned()))?;
    let id_str = obj
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PassthroughError::InvalidInput("`id` (string) is required".to_owned()))?;
    Uuid::parse_str(id_str)
        .map_err(|e| PassthroughError::InvalidInput(format!("`id` is not a valid UUID: {e}")))?;

    let count = match obj.get("count") {
        None => CHUNK_NAV_DEFAULT_COUNT,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(PassthroughError::InvalidInput("`count` must be an integer".to_owned()));
            };
            if !(1..=i64::from(CHUNK_NAV_MAX_COUNT)).contains(&n) {
                return Err(PassthroughError::InvalidInput(format!(
                    "`count` must be 1..={CHUNK_NAV_MAX_COUNT}"
                )));
            }
            u32::try_from(n).expect("validated above")
        }
    };

    let r = match dir {
        ChunkNavDirection::Next => cloud.get_chunk_next(id_str, count).await,
        ChunkNavDirection::Prev => cloud.get_chunk_prev(id_str, count).await,
    };
    r.map_err(|e| match e {
        CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
        other => PassthroughError::Cloud(other.to_string()),
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mn-mcp --test tools_dispatch run_chunk_nav
```

Expected: 7 passed.

- [ ] **Step 5: Clippy clean**

```bash
cargo clippy -p mn-mcp --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/mn-mcp/src/tools.rs crates/mn-mcp/tests/tools_dispatch.rs
git commit -m "$(cat <<'EOF'
feat(mn-mcp): add run_chunk_nav + ChunkNavDirection helper

Tool-side dispatcher for get_chunk_next / get_chunk_prev. Parses
{id, count?} with the same id-uuid validation as the existing
run_passthrough_id, and rejects out-of-range or non-integer count
values as InvalidInput before the cloud call.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Add `run_document_chunks` helper

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`
- Modify: `crates/mn-mcp/tests/tools_dispatch.rs`

- [ ] **Step 1: Write the failing dispatch tests**

Add `run_document_chunks` to the imports in `crates/mn-mcp/tests/tools_dispatch.rs`:

```rust
use mn_mcp::tools::{
    run_chunk_nav, run_document_chunks, run_passthrough_id, ChunkNavDirection,
    PassthroughError, PassthroughKind,
};
```

Append the tests:

```rust
#[tokio::test]
async fn run_document_chunks_sends_from_and_limit() {
    let server = MockServer::start().await;
    let id = "33333333-3333-3333-3333-333333333300";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .and(wiremock::matchers::query_param("from", "3"))
        .and(wiremock::matchers::query_param("limit", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunks": [], "from": 3, "limit": 7, "total_chunks": 0,
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let v = run_document_chunks(&json!({"id": id, "from": 3, "limit": 7}), &client)
        .await
        .unwrap();
    assert_eq!(v["from"], 3);
    assert_eq!(v["limit"], 7);
}

#[tokio::test]
async fn run_document_chunks_defaults_from_zero_limit_twenty() {
    let server = MockServer::start().await;
    let id = "33333333-3333-3333-3333-333333333301";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .and(wiremock::matchers::query_param("from", "0"))
        .and(wiremock::matchers::query_param("limit", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunks": [], "from": 0, "limit": 20, "total_chunks": 0,
        })))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let _ = run_document_chunks(&json!({"id": id}), &client).await.unwrap();
}

#[tokio::test]
async fn run_document_chunks_rejects_negative_from() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_document_chunks(
        &json!({"id": "33333333-3333-3333-3333-333333333302", "from": -1}),
        &client,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_document_chunks_rejects_limit_zero() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_document_chunks(
        &json!({"id": "33333333-3333-3333-3333-333333333303", "limit": 0}),
        &client,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_document_chunks_rejects_limit_over_max() {
    let server = MockServer::start().await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_document_chunks(
        &json!({"id": "33333333-3333-3333-3333-333333333304", "limit": 101}),
        &client,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PassthroughError::InvalidInput(_)));
}

#[tokio::test]
async fn run_document_chunks_404_maps_to_not_found() {
    let server = MockServer::start().await;
    let id = "33333333-3333-3333-3333-333333333305";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/chunks")))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_document_chunks(&json!({"id": id}), &client).await.unwrap_err();
    assert!(matches!(err, PassthroughError::NotFound(_)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mn-mcp --test tools_dispatch run_document_chunks
```

Expected: FAIL with `run_document_chunks` not found.

- [ ] **Step 3: Add the helper**

In `crates/mn-mcp/src/tools.rs`, after `run_chunk_nav`:

```rust
const DOCUMENT_CHUNKS_DEFAULT_FROM: u32 = 0;
const DOCUMENT_CHUNKS_DEFAULT_LIMIT: u32 = 20;
const DOCUMENT_CHUNKS_MAX_LIMIT: u32 = 100;

/// Dispatch `get_document_chunks`. Parses `{id, from?, limit?}`. `from`
/// must be `>= 0`; `limit` must be in `[1, 100]`. Out-of-range or wrong-type
/// values are rejected as `InvalidInput` before the wire call.
///
/// # Errors
///
/// See [`PassthroughError`].
pub async fn run_document_chunks(
    args: &serde_json::Value,
    cloud: &Arc<CloudClient>,
) -> Result<serde_json::Value, PassthroughError> {
    let obj = args
        .as_object()
        .ok_or_else(|| PassthroughError::InvalidInput("arguments must be a JSON object".to_owned()))?;
    let id_str = obj
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PassthroughError::InvalidInput("`id` (string) is required".to_owned()))?;
    Uuid::parse_str(id_str)
        .map_err(|e| PassthroughError::InvalidInput(format!("`id` is not a valid UUID: {e}")))?;

    let from = match obj.get("from") {
        None => DOCUMENT_CHUNKS_DEFAULT_FROM,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(PassthroughError::InvalidInput("`from` must be an integer".to_owned()));
            };
            if n < 0 {
                return Err(PassthroughError::InvalidInput("`from` must be >= 0".to_owned()));
            }
            u32::try_from(n).map_err(|_| {
                PassthroughError::InvalidInput("`from` exceeds 32-bit range".to_owned())
            })?
        }
    };

    let limit = match obj.get("limit") {
        None => DOCUMENT_CHUNKS_DEFAULT_LIMIT,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return Err(PassthroughError::InvalidInput("`limit` must be an integer".to_owned()));
            };
            if !(1..=i64::from(DOCUMENT_CHUNKS_MAX_LIMIT)).contains(&n) {
                return Err(PassthroughError::InvalidInput(format!(
                    "`limit` must be 1..={DOCUMENT_CHUNKS_MAX_LIMIT}"
                )));
            }
            u32::try_from(n).expect("validated above")
        }
    };

    cloud
        .get_document_chunks(id_str, from, limit)
        .await
        .map_err(|e| match e {
            CloudError::NotFound(msg) => PassthroughError::NotFound(msg),
            other => PassthroughError::Cloud(other.to_string()),
        })
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mn-mcp --test tools_dispatch run_document_chunks
```

Expected: 6 passed.

- [ ] **Step 5: Clippy clean**

```bash
cargo clippy -p mn-mcp --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/mn-mcp/src/tools.rs crates/mn-mcp/tests/tools_dispatch.rs
git commit -m "$(cat <<'EOF'
feat(mn-mcp): add run_document_chunks helper

Tool-side dispatcher for get_document_chunks. Parses {id, from?, limit?}
with the from >= 0 / limit [1, 100] validation policy and forwards to
CloudClient::get_document_chunks.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Add 5 new entries to tool list; refresh `get_chunk` description

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`

- [ ] **Step 1: Update the failing tool-count test**

In `crates/mn-mcp/src/tools.rs`, the existing test `tool_list_has_all_seven_tools` (around line 663) needs to be made to fail by raising the expected count. Replace it with:

```rust
#[test]
fn tool_list_has_all_twelve_tools_pre_siblings_removal() {
    // After the 5 new navigation tools are added and before
    // `get_chunk_siblings` is removed, the manifest carries 12 tools.
    let m = list();
    let names: Vec<_> = m.tools.iter().map(|t| t.name).collect();
    for expected in [
        "search",
        "get_chunk",
        "get_chunk_next",
        "get_chunk_prev",
        "get_chunk_siblings",
        "get_chunk_parents",
        "get_document",
        "get_document_full",
        "get_document_chunks",
        "list_sources",
        "pull_models",
        "status",
    ] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }
    assert_eq!(names.len(), 12, "expected 12 tools, got {}", names.len());
}

#[test]
fn new_navigation_tools_have_object_schemas() {
    let m = list();
    for name in [
        "get_chunk_next",
        "get_chunk_prev",
        "get_document",
        "get_document_full",
        "get_document_chunks",
    ] {
        let tool = m
            .tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("missing tool: {name}"));
        assert_eq!(tool.input_schema["type"], "object", "{name} schema must be object-typed");
        assert_eq!(
            tool.input_schema["additionalProperties"], false,
            "{name} schema must reject additional properties"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mn-mcp --lib tools::tests::tool_list
cargo test -p mn-mcp --lib tools::tests::new_navigation_tools
```

Expected: FAIL — count is 7 / missing tools.

- [ ] **Step 3: Refresh `get_chunk` description and add new entries**

In `crates/mn-mcp/src/tools.rs`, edit the `list()` function. First, replace the `get_chunk` entry's description:

```rust
            ToolDescription {
                name: "get_chunk",
                description:
                    "Fetch one chunk by id. Returns the chunk row (id, content, chunk_index, total_chunks, content_hash, embedding_model_id, heading_path, symbol_path, start_byte, end_byte, token_count, status, created_at, document_id, source_version_id, node_id) plus a small `document` sub-object (id, source_path, published_url, source_url, language, kind, provenance) and a `source` sub-object (slug). For the chunk's parent chain call get_chunk_parents; for adjacent chunks call get_chunk_next/get_chunk_prev.",
                input_schema: id_only_schema(),
            },
```

Add five new `ToolDescription` entries between `get_chunk` and `get_chunk_siblings` (for `get_chunk_next`/`get_chunk_prev`), and after `get_chunk_parents` (for the three document tools). The final ordering inside `tools: vec![...]` is:

```rust
        tools: vec![
            // ... search (unchanged) ...
            // ... get_chunk (description refreshed above) ...
            ToolDescription {
                name: "get_chunk_next",
                description:
                    "Fetch up to `count` chunks immediately following the given chunk in chunk_index order, scoped to the same document. Returns `{chunks: ChunkWithContext[]}` sorted ascending. Returns `{chunks: []}` (not 404) when called on the last chunk. `embed_failed` chunks are skipped, so the returned chunk_index sequence may have gaps. count defaults to 5 and must be in [1, 100]; out-of-range values are rejected as InvalidParams before the call reaches the cloud.",
                input_schema: chunk_nav_schema(),
            },
            ToolDescription {
                name: "get_chunk_prev",
                description:
                    "Fetch up to `count` chunks immediately preceding the given chunk in chunk_index order, scoped to the same document. Returns `{chunks: ChunkWithContext[]}` sorted ascending (reading order). Returns `{chunks: []}` (not 404) when called on the first chunk. `embed_failed` chunks are skipped, so the returned chunk_index sequence may have gaps. count defaults to 5 and must be in [1, 100]; out-of-range values are rejected as InvalidParams before the call reaches the cloud.",
                input_schema: chunk_nav_schema(),
            },
            // ... get_chunk_siblings (unchanged for now) ...
            // ... get_chunk_parents (unchanged) ...
            ToolDescription {
                name: "get_document",
                description:
                    "Document overview: metadata (id, source_version_id, node_id, source_path, published_url, source_url, language, kind, content_hash, char_count, token_count, source_modified_at, created_at, frontmatter, provenance, package_id), the source `{slug}`, and an ordered `chunk_ids` array of every ready chunk. No chunk bodies. Use get_document_full for inline bodies or get_document_chunks for a windowed slice.",
                input_schema: id_only_schema(),
            },
            ToolDescription {
                name: "get_document_full",
                description:
                    "Complete document: every overview field except chunk_ids, plus a `chunks` array with each chunk's `{chunk_id, chunk_index, content, heading_path, token_count}` inline (no per-chunk document/source sub-objects). Capped at 500 ready chunks. For documents over the cap the call fails with a structured `too_many_chunks` error (see error.data.next_tool); fall back to get_document_chunks.",
                input_schema: id_only_schema(),
            },
            ToolDescription {
                name: "get_document_chunks",
                description:
                    "Position-windowed chunk slice of a document. Returns `{chunks: ChunkBody[], from, limit, total_chunks}`. from defaults to 0 (must be >= 0); limit defaults to 20 and must be in [1, 100]. Out-of-range values are rejected as InvalidParams before the call reaches the cloud. `from` past the end returns `chunks: []` with accurate `total_chunks` (not 404). Use to page through documents larger than get_document_full's 500-chunk cap or to read a known offset.",
                input_schema: document_chunks_schema(),
            },
            // ... list_sources, pull_models, status (unchanged) ...
        ],
```

Add the two new schema helpers near `id_only_schema()`:

```rust
fn chunk_nav_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 5 },
        },
        "additionalProperties": false,
    })
}

fn document_chunks_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "from": { "type": "integer", "minimum": 0, "default": 0 },
            "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
        },
        "additionalProperties": false,
    })
}
```

Refresh the module-level doc comment at the top of `crates/mn-mcp/src/tools.rs` (lines 1-11):

```rust
//! MCP tool registry and per-tool handlers.
//!
//! Twelve tools (until `get_chunk_siblings` is removed at the end of this
//! plan), three categories:
//!
//! - `status` / `pull_models` — local-only; talk to the embedder/reranker
//!   model cache. No cloud round-trip.
//! - `search` — embed locally, post to the cloud `/v1/search`, optionally
//!   rerank with the local cross-encoder.
//! - All other tools (`get_chunk` / `get_chunk_next` / `get_chunk_prev` /
//!   `get_chunk_siblings` / `get_chunk_parents` / `get_document` /
//!   `get_document_full` / `get_document_chunks` / `list_sources`) —
//!   pass-through to the cloud's read endpoints, returning the response
//!   JSON verbatim.
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mn-mcp --lib tools::tests
```

Expected: all tests pass, including the new two.

- [ ] **Step 5: Clippy clean**

```bash
cargo clippy -p mn-mcp --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/mn-mcp/src/tools.rs
git commit -m "$(cat <<'EOF'
feat(mn-mcp): list five new navigation tools; refresh get_chunk description

Adds get_chunk_next, get_chunk_prev, get_document, get_document_full,
and get_document_chunks to the tools/list manifest with the JSON
schemas matching the spec. Refreshes the get_chunk description to
match the augmented response shape (document + source sub-objects).
Dispatcher wiring lands in a later task.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Extend `McpToolName` and `tool_name_for_event`

**Files:**
- Modify: `crates/mn-telemetry/src/events.rs`
- Modify: `crates/mn-mcp/src/server.rs`

These two changes must land together — adding `McpToolName` variants without referencing them in `tool_name_for_event` would emit `dead_code` warnings under `-D warnings`.

- [ ] **Step 1: Read current state of the enum**

Skim `crates/mn-telemetry/src/events.rs` around line 167 (the `McpToolName` enum) to confirm the existing shape.

- [ ] **Step 2: Add the five new variants**

In `crates/mn-telemetry/src/events.rs`, edit `McpToolName`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpToolName {
    /// `search` tool.
    Search,
    /// `get_chunk` tool.
    GetChunk,
    /// `get_chunk_next` tool.
    GetChunkNext,
    /// `get_chunk_prev` tool.
    GetChunkPrev,
    /// `get_chunk_siblings` tool (slated for removal at the end of this plan).
    GetChunkSiblings,
    /// `get_chunk_parents` tool.
    GetChunkParents,
    /// `get_document` tool.
    GetDocument,
    /// `get_document_full` tool.
    GetDocumentFull,
    /// `get_document_chunks` tool.
    GetDocumentChunks,
    /// `list_sources` tool.
    ListSources,
    /// `pull_models` tool.
    PullModels,
    /// `status` tool.
    Status,
}
```

- [ ] **Step 3: Extend `tool_name_for_event` in server.rs**

In `crates/mn-mcp/src/server.rs`, replace the `tool_name_for_event` function:

```rust
fn tool_name_for_event(name: &str) -> Option<McpToolName> {
    match name {
        "search" => Some(McpToolName::Search),
        "get_chunk" => Some(McpToolName::GetChunk),
        "get_chunk_next" => Some(McpToolName::GetChunkNext),
        "get_chunk_prev" => Some(McpToolName::GetChunkPrev),
        "get_chunk_siblings" => Some(McpToolName::GetChunkSiblings),
        "get_chunk_parents" => Some(McpToolName::GetChunkParents),
        "get_document" => Some(McpToolName::GetDocument),
        "get_document_full" => Some(McpToolName::GetDocumentFull),
        "get_document_chunks" => Some(McpToolName::GetDocumentChunks),
        "list_sources" => Some(McpToolName::ListSources),
        "pull_models" => Some(McpToolName::PullModels),
        "status" => Some(McpToolName::Status),
        _ => None,
    }
}
```

- [ ] **Step 4: Run a workspace build to verify nothing is broken**

```bash
cargo build -p mn-mcp -p mn-telemetry
```

Expected: clean build.

- [ ] **Step 5: Run the affected test suites**

```bash
cargo test -p mn-mcp -p mn-telemetry
```

Expected: all green.

- [ ] **Step 6: Clippy clean**

```bash
cargo clippy -p mn-mcp -p mn-telemetry --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/mn-telemetry/src/events.rs crates/mn-mcp/src/server.rs
git commit -m "$(cat <<'EOF'
feat(mn-telemetry): add five McpToolName variants for navigation tools

Adds GetChunkNext, GetChunkPrev, GetDocument, GetDocumentFull, and
GetDocumentChunks. Wired into tool_name_for_event in mn-mcp the same
commit so the new variants are referenced (avoids dead-code warning
under -D warnings). GetChunkSiblings is retained for now and removed
in a later task.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Wire dispatch for the 5 new tools; add `too_many_chunks_response` builder

**Files:**
- Modify: `crates/mn-mcp/src/server.rs`

- [ ] **Step 1: Add the typed 412 response builder**

In `crates/mn-mcp/src/server.rs`, immediately after `mismatch_response` (at the bottom of the file), add:

```rust
/// Build a JSON-RPC error response for the cloud's 412 `too_many_chunks`
/// body, putting the count + cap + hint in the `data` field so an AI client
/// can render a structured remediation (next_tool = "get_document_chunks").
fn too_many_chunks_response(id: RequestId, chunk_count: u32, cap: u32, hint: &str) -> Response {
    let data = serde_json::json!({
        "kind": "too_many_chunks",
        "chunk_count": chunk_count,
        "cap": cap,
        "hint": hint,
        "next_tool": "get_document_chunks",
    });
    Response {
        jsonrpc: JSONRPC,
        id,
        result: None,
        error: Some(JsonRpcError {
            code: ErrorCode::ToolFailed as i32,
            message: format!("document has {chunk_count} chunks (cap {cap})"),
            data: Some(data),
        }),
    }
}
```

- [ ] **Step 2: Add new dispatch helpers**

In `crates/mn-mcp/src/server.rs`, after `run_passthrough_dispatch`, add:

```rust
async fn run_chunk_nav_dispatch(
    id: &RequestId,
    params: &ToolCallParams,
    state: &ServerState,
    dir: tools::ChunkNavDirection,
) -> Result<String, Response> {
    match tools::run_chunk_nav(&params.arguments, &state.cloud, dir).await {
        Ok(v) => Ok(v.to_string()),
        Err(tools::PassthroughError::InvalidInput(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::InvalidParams, msg))
        }
        Err(tools::PassthroughError::NotFound(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, format!("not found: {msg}")))
        }
        Err(tools::PassthroughError::TooManyChunks { .. }) => {
            // Not reachable for next/prev (no 412 on /next or /prev), but
            // exhaustively matched so the compiler catches additions.
            Err(Response::err(
                id.clone(),
                ErrorCode::ToolFailed,
                "unexpected too_many_chunks on /next or /prev".to_owned(),
            ))
        }
        Err(tools::PassthroughError::Cloud(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, msg))
        }
    }
}

async fn run_document_chunks_dispatch(
    id: &RequestId,
    params: &ToolCallParams,
    state: &ServerState,
) -> Result<String, Response> {
    match tools::run_document_chunks(&params.arguments, &state.cloud).await {
        Ok(v) => Ok(v.to_string()),
        Err(tools::PassthroughError::InvalidInput(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::InvalidParams, msg))
        }
        Err(tools::PassthroughError::NotFound(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, format!("not found: {msg}")))
        }
        Err(tools::PassthroughError::TooManyChunks { .. }) => {
            Err(Response::err(
                id.clone(),
                ErrorCode::ToolFailed,
                "unexpected too_many_chunks on /chunks window".to_owned(),
            ))
        }
        Err(tools::PassthroughError::Cloud(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, msg))
        }
    }
}
```

- [ ] **Step 3: Replace the stub TooManyChunks arm with the typed response**

Task 5 added a stub `TooManyChunks` arm in `run_passthrough_dispatch` that just produced a plain `ToolFailed` error. Replace it now with the typed `too_many_chunks_response` call. Replace the entire `run_passthrough_dispatch` function with:

```rust
async fn run_passthrough_dispatch(
    id: &RequestId,
    params: &ToolCallParams,
    state: &ServerState,
    kind: tools::PassthroughKind,
) -> Result<String, Response> {
    match tools::run_passthrough_id(&params.arguments, &state.cloud, kind).await {
        Ok(v) => Ok(v.to_string()),
        Err(tools::PassthroughError::InvalidInput(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::InvalidParams, msg))
        }
        Err(tools::PassthroughError::NotFound(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, format!("not found: {msg}")))
        }
        Err(tools::PassthroughError::TooManyChunks { chunk_count, cap, hint }) => {
            Err(too_many_chunks_response(id.clone(), chunk_count, cap, &hint))
        }
        Err(tools::PassthroughError::Cloud(msg)) => {
            Err(Response::err(id.clone(), ErrorCode::ToolFailed, msg))
        }
    }
}
```

- [ ] **Step 4: Add dispatch arms for the five new tools**

In `crates/mn-mcp/src/server.rs`, in `dispatch_tool_inner`, add arms for each new tool. Insert these after the existing `get_chunk_parents` arm and before `list_sources`:

```rust
        "get_chunk_next" => {
            run_chunk_nav_dispatch(&id, &params, state, tools::ChunkNavDirection::Next).await
        }
        "get_chunk_prev" => {
            run_chunk_nav_dispatch(&id, &params, state, tools::ChunkNavDirection::Prev).await
        }
        "get_document" => {
            run_passthrough_dispatch(&id, &params, state, tools::PassthroughKind::Document).await
        }
        "get_document_full" => {
            run_passthrough_dispatch(&id, &params, state, tools::PassthroughKind::DocumentFull)
                .await
        }
        "get_document_chunks" => run_document_chunks_dispatch(&id, &params, state).await,
```

- [ ] **Step 5: Run the workspace test suite**

```bash
cargo test -p mn-mcp
```

Expected: all green.

- [ ] **Step 6: Clippy clean**

```bash
cargo clippy -p mn-mcp --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/mn-mcp/src/server.rs
git commit -m "$(cat <<'EOF'
feat(mn-mcp): wire dispatch for five navigation tools + typed 412

Adds dispatch arms for get_chunk_next, get_chunk_prev, get_document,
get_document_full, and get_document_chunks. The TooManyChunks error
returned by get_document_full (via the tools-layer PassthroughError)
is translated into a typed JSON-RPC error envelope with
data.next_tool = "get_document_chunks", mirroring the existing
embedding_model_mismatch precedent.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Server-loop end-to-end test for 412 → typed JSON-RPC error

**Files:**
- Modify: `crates/mn-mcp/tests/server_loop.rs`

The existing `tools_list_returns_two_phase_5b_tools` test is content-light. We'll leave it alone (it's an additive assertion) and add two new tests: an explicit count + name list for the current 12-tool manifest, and an end-to-end 412 flow.

The 412 end-to-end test exercises the dispatcher rather than `mn_mcp::run` (which requires stdin/stdout). It builds a `ToolCallParams` and a wiremock cloud, then asserts the response shape. The dispatcher entry point used by the existing tests is internal to the crate, so to test the full path from `tools/call` through to `too_many_chunks_response` we drive `tools::run_passthrough_id` + manually invoke the dispatch flow that lives in `server.rs`. The simplest exercise is to call `tools::run_passthrough_id`, observe `PassthroughError::TooManyChunks`, and assert it has the right fields — the `too_many_chunks_response` builder itself is exercised indirectly via the existing dispatch helpers.

For a real end-to-end test we'd need to expose a public dispatch entry point. That's a refactor we don't want here. Instead, write a focused test on `tools::run_passthrough_id` returning the right typed error (covered already in Task 5) and add a server-loop tools/list count assertion.

- [ ] **Step 1: Write the tools/list count test**

Append to `crates/mn-mcp/tests/server_loop.rs`:

```rust
#[tokio::test]
async fn tools_list_contains_all_twelve_pre_siblings_removal() {
    // Until `get_chunk_siblings` is removed at the end of the plan, the
    // manifest carries 12 tools. After removal this drops to 11 and the
    // test is updated.
    let list = mn_mcp::tools::list();
    let names: Vec<_> = list.tools.iter().map(|t| t.name).collect();
    for expected in [
        "search",
        "get_chunk",
        "get_chunk_next",
        "get_chunk_prev",
        "get_chunk_siblings",
        "get_chunk_parents",
        "get_document",
        "get_document_full",
        "get_document_chunks",
        "list_sources",
        "pull_models",
        "status",
    ] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }
    assert_eq!(names.len(), 12);
}

#[tokio::test]
async fn new_navigation_tool_schemas_are_well_formed() {
    let list = mn_mcp::tools::list();
    for name in [
        "get_chunk_next",
        "get_chunk_prev",
        "get_document",
        "get_document_full",
        "get_document_chunks",
    ] {
        let t = list
            .tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("missing tool: {name}"));
        assert_eq!(t.input_schema["type"], "object");
        assert!(t.input_schema["required"].as_array().is_some());
        assert!(t.input_schema["properties"]["id"]["format"] == "uuid");
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

```bash
cargo test -p mn-mcp --test server_loop
```

Expected: all green (the new tests pass because Tasks 8 + 10 already set up the manifest).

- [ ] **Step 3: Clippy clean**

```bash
cargo clippy -p mn-mcp --all-targets -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add crates/mn-mcp/tests/server_loop.rs
git commit -m "$(cat <<'EOF'
test(mn-mcp): assert the 12-tool manifest + schema shape for new tools

Adds server-loop-level coverage that locks in the post-add /
pre-removal tools/list shape: 12 tools, and the five new entries have
well-formed object schemas with id (uuid) required. The count
assertion will need updating when get_chunk_siblings is removed in
the final task.

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Remove `get_chunk_siblings` everywhere — single atomic commit

This is the cleanup task. All siblings touch-points are removed in one commit to keep intermediate states green. There are eight touch-points; address them in this order so each edit is local.

**Files:**
- Modify: `crates/mn-mcp/src/cloud_client.rs`
- Modify: `crates/mn-mcp/tests/cloud_client.rs`
- Modify: `crates/mn-mcp/src/tools.rs`
- Modify: `crates/mn-mcp/tests/tools_dispatch.rs`
- Modify: `crates/mn-mcp/src/server.rs`
- Modify: `crates/mn-mcp/tests/server_loop.rs`
- Modify: `crates/mn-telemetry/src/events.rs`

- [ ] **Step 1: Remove the cloud_client method**

In `crates/mn-mcp/src/cloud_client.rs`, delete the `get_chunk_siblings` method (the entire `pub async fn get_chunk_siblings` block in `impl CloudClient`).

- [ ] **Step 2: Remove the cloud_client test**

In `crates/mn-mcp/tests/cloud_client.rs`, delete the entire `get_chunk_siblings_round_trips` test function (lines ~183-198 in the current file).

- [ ] **Step 3: Remove `PassthroughKind::Siblings` and its match arm**

In `crates/mn-mcp/src/tools.rs`:

- Delete the `Siblings` variant from `PassthroughKind`.
- Delete the `PassthroughKind::Siblings => cloud.get_chunk_siblings(id_str).await,` arm from `run_passthrough_id`.

- [ ] **Step 4: Remove the `get_chunk_siblings` entry from `list()`**

In `crates/mn-mcp/src/tools.rs`, delete the `ToolDescription` block for `get_chunk_siblings` in the `list()` function.

- [ ] **Step 5: Repoint the dispatch test that used `PassthroughKind::Siblings`**

In `crates/mn-mcp/tests/tools_dispatch.rs`, the test `run_passthrough_id_maps_404` uses `PassthroughKind::Siblings` against `/v1/chunks/{id}/siblings`. Repoint to `PassthroughKind::DocumentFull` against `/v1/documents/{id}/full`:

```rust
#[tokio::test]
async fn run_passthrough_id_maps_404() {
    let server = MockServer::start().await;
    let id = "22222222-2222-2222-2222-222222222222";
    Mock::given(method("GET"))
        .and(path(format!("/v1/documents/{id}/full")))
        .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
        .mount(&server)
        .await;
    let client = Arc::new(CloudClient::new(&server.uri(), None).unwrap());
    let err = run_passthrough_id(&json!({"id": id}), &client, PassthroughKind::DocumentFull)
        .await
        .unwrap_err();
    assert!(matches!(err, PassthroughError::NotFound(_)));
}
```

- [ ] **Step 6: Update the in-file tool list test (renamed from "twelve" to "eleven")**

In `crates/mn-mcp/src/tools.rs`, rename `tool_list_has_all_twelve_tools_pre_siblings_removal` (added in Task 8) to `tool_list_has_all_eleven_tools`. Drop the `get_chunk_siblings` line from the expected list and change the count assertion:

```rust
#[test]
fn tool_list_has_all_eleven_tools() {
    let m = list();
    let names: Vec<_> = m.tools.iter().map(|t| t.name).collect();
    for expected in [
        "search",
        "get_chunk",
        "get_chunk_next",
        "get_chunk_prev",
        "get_chunk_parents",
        "get_document",
        "get_document_full",
        "get_document_chunks",
        "list_sources",
        "pull_models",
        "status",
    ] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }
    assert_eq!(names.len(), 11, "expected 11 tools, got {}", names.len());
}
```

- [ ] **Step 7: Update the module-level doc comment**

In `crates/mn-mcp/src/tools.rs` at the top, replace the 12-tool note with the 11-tool note:

```rust
//! MCP tool registry and per-tool handlers.
//!
//! Eleven tools, three categories:
//!
//! - `status` / `pull_models` — local-only; talk to the embedder/reranker
//!   model cache. No cloud round-trip.
//! - `search` — embed locally, post to the cloud `/v1/search`, optionally
//!   rerank with the local cross-encoder.
//! - All other tools (`get_chunk` / `get_chunk_next` / `get_chunk_prev` /
//!   `get_chunk_parents` / `get_document` / `get_document_full` /
//!   `get_document_chunks` / `list_sources`) — pass-through to the cloud's
//!   read endpoints, returning the response JSON verbatim.
```

- [ ] **Step 8: Remove the server.rs dispatch arm and tool_name_for_event arm**

In `crates/mn-mcp/src/server.rs`:

- In `dispatch_tool_inner`, delete the `"get_chunk_siblings" => run_passthrough_dispatch(...)` arm.
- In `tool_name_for_event`, delete the `"get_chunk_siblings" => Some(McpToolName::GetChunkSiblings),` arm.

- [ ] **Step 9: Update the server-loop tools/list assertions**

In `crates/mn-mcp/tests/server_loop.rs`, in `tools_list_contains_all_twelve_pre_siblings_removal`:

- Rename to `tools_list_contains_all_eleven`.
- Drop the `"get_chunk_siblings"` line from the expected list.
- Change `assert_eq!(names.len(), 12)` to `assert_eq!(names.len(), 11)`.

- [ ] **Step 10: Remove the telemetry enum variant**

In `crates/mn-telemetry/src/events.rs`, delete the `GetChunkSiblings` variant from `McpToolName`:

```rust
// Delete these two lines:
    /// `get_chunk_siblings` tool (slated for removal at the end of this plan).
    GetChunkSiblings,
```

- [ ] **Step 11: Run the workspace test suite**

```bash
cargo test -p mn-mcp -p mn-telemetry
```

Expected: all green.

- [ ] **Step 12: Run the full workspace check**

```bash
just check
```

Expected: fmt clean, clippy clean (`-D warnings`), all tests pass.

If the pre-existing flake `tests/auth_integration.rs::local_listener_ignores_non_oauth_paths` fails, ignore it — it's a known intermittent unrelated to this work. Anything else failing is a regression.

- [ ] **Step 13: Commit**

```bash
git add crates/mn-mcp/src/cloud_client.rs \
        crates/mn-mcp/tests/cloud_client.rs \
        crates/mn-mcp/src/tools.rs \
        crates/mn-mcp/tests/tools_dispatch.rs \
        crates/mn-mcp/src/server.rs \
        crates/mn-mcp/tests/server_loop.rs \
        crates/mn-telemetry/src/events.rs
git commit -m "$(cat <<'EOF'
feat(mn-mcp): remove get_chunk_siblings tool (replaced by document nav)

The cloud route GET /v1/chunks/:id/siblings was removed in PR #51; the
matching MCP tool would 404 in production once shipped. Remove it
everywhere: cloud_client method, PassthroughKind variant, tool list
entry, server dispatch arm, telemetry McpToolName variant, and the
associated tests. Repoint the 404-mapping dispatch test at
PassthroughKind::DocumentFull (the same code path).

Functional replacement: agents that want a document's entire chunk
set can call get_document_full (capped at 500 chunks); agents that
want a windowed slice can call get_document_chunks (paged).

Net change: 7 tools → 11 (after the additions in earlier tasks).

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

## Self-review checklist (after all 12 tasks complete)

- [ ] `cargo test --workspace` (no `--features integration` needed — none of these touch Postgres).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] `cargo fmt --check`.
- [ ] `git log --oneline -13` shows 12 feature commits in order.
- [ ] No remaining references to `get_chunk_siblings` / `GetChunkSiblings` / `PassthroughKind::Siblings` anywhere under `crates/mn-mcp/` or `crates/mn-telemetry/`:
  ```bash
  rg 'siblings|Siblings' crates/mn-mcp crates/mn-telemetry
  ```
  Expected: no hits. (The CLI hand-off mentions an `cli` reference in older specs/plans under `docs/` — those are immutable history and do not count.)
- [ ] `cargo run --release -p mn-cli -- mcp serve` starts; from another terminal an `initialize` + `tools/list` JSON-RPC roundtrip lists exactly 11 tools. (Optional smoke test — covered by `tests/server_loop.rs::tools_list_contains_all_eleven`.)

---

## Spec coverage check

| Spec section | Implemented in task(s) |
|---|---|
| §1 — Tool surface (11 tools final) | Tasks 8 + 12 |
| §2.1 — `get_chunk` description refresh | Task 8 |
| §2.2 — `get_chunk_next` description + schema | Task 8 |
| §2.3 — `get_chunk_prev` description + schema | Task 8 |
| §2.4 — `get_document` description + schema | Task 8 |
| §2.5 — `get_document_full` description + schema | Task 8 |
| §2.6 — `get_document_chunks` description + schema | Task 8 |
| §3 — Response shapes (pass-through) | Tasks 2-4 (cloud), 5-7 (tools) |
| §4.1 — `CloudError::TooManyChunks` + parser + cloud_client methods | Tasks 1-4 |
| §4.2 — `PassthroughError::TooManyChunks` + `PassthroughKind::{Document, DocumentFull}` + `ChunkNavDirection` + `run_chunk_nav` + `run_document_chunks` | Tasks 5-7 |
| §4.3 — `tool_name_for_event` updates + dispatch arms + dispatch helpers + `too_many_chunks_response` builder | Tasks 9-10 |
| §5 — Error mapping | Tasks 5, 7, 10 (typed 412 in dispatch) |
| §6 — Telemetry enum changes | Tasks 9 + 12 |
| §7.1 — Updates to existing tests (count, repoint siblings 404) | Tasks 8, 12 |
| §7.2 — New unit + dispatch tests | Tasks 1, 5-7 |
| §7.3 — End-to-end server-loop tools/list test | Task 11 |
| §8 — File deltas (all touch-points) | Spread across all tasks |
| §9 — Open follow-ups (out of scope) | n/a — intentionally deferred |

All spec items covered.

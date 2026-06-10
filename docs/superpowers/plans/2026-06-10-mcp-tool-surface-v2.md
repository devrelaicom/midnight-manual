# MCP Tool Surface v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the approved design in `docs/superpowers/specs/2026-06-10-mcp-tool-surface-v2-design.md`: 13 rewritten MCP tools (search split, batch get_chunks, merged get_document, pull_models removed), `suggested_next_actions` with prose descriptions, tool annotations, five cloud API additions, a reworked `status` shared with a new `mnm status` CLI command, and a rewritten bundled search skill.

**Architecture:** Three layers, built bottom-up: (1) cloud endpoints in `mn-server`/`mn-store` first, (2) `mn-mcp` client + render + tool surface on top, (3) skill content, CLI command, and contracts last. Every tool result keeps the PR #79 shape: one text block (summary + ```json fence) + full-fidelity `structuredContent`.

**Tech Stack:** Rust stable, axum, sqlx/Postgres, reqwest, serde_json, clap v4, wiremock (tests).

**Branch:** `mcp-response-format` (extends open PR #79). Commit after every task.

**Environment notes (sandbox):**
- DB-backed integration tests do NOT run locally (no Docker) — they're written here, gated behind `--features integration`, and verified in CI.
- The sandbox exports `VOYAGE_API_KEY`; run mn-mcp tests as `VOYAGE_API_KEY= cargo test -p mn-mcp` to avoid the BYOK-path failures.
- Before the final push run the full CI surface: `cargo fmt --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`.

**File map (created / modified):**

| Layer | Files |
|---|---|
| mn-store | `crates/mn-store/src/entities/chunk.rs` (batch fetch), `source.rs` (paged list), `document.rs` (skeleton overview, delete full), `node.rs` (parent chain + document_id) |
| mn-server | NEW `crates/mn-server/src/pagination.rs`, NEW `routes/me.rs`; `routes/chunks.rs`, `routes/sources.rs`, `routes/facets.rs`, `routes/documents.rs`, `app.rs`, `routes/mod.rs` |
| mn-mcp | `src/cloud_client.rs`, `src/render.rs`, `src/tools.rs`, `src/server.rs`, `src/schemas.rs`, `src/protocol.rs`, NEW `src/status.rs` |
| mn-skills | `src/install.rs` (+`detected`), `src/lib.rs` (+`installed_anywhere`), `assets/midnight-advanced-search/**` (content rewrite) |
| mn-telemetry | `src/events.rs` (new enum variants) |
| mn-cli | NEW `src/commands/status.rs`; `src/cli.rs`, `src/commands/mod.rs`, `src/commands/mcp.rs`, `src/shared.rs`, `src/commands/doctor.rs` |
| contracts | `specs/001-rag-platform/contracts/openapi.yaml`, `mcp-tools.json` |

---

## Phase 1 — Cloud API (mn-server / mn-store)

### Task 1: Batch chunk fetch — store query + `GET /v1/chunks?ids=`

**Files:**
- Modify: `crates/mn-store/src/entities/chunk.rs` (add `get_many_with_context` below `get_with_context`, ~line 419)
- Modify: `crates/mn-server/src/routes/chunks.rs` (new route + handler)
- Test: `crates/mn-server/tests/` integration (CI-only) + unit test in `routes/chunks.rs`

- [ ] **Step 1: Add the store function** (`crates/mn-store/src/entities/chunk.rs`, after `get_with_context`):

```rust
/// Fetch many chunks (each with document + source context) in one query.
/// Rows come back in arbitrary DB order; callers re-order by input order.
/// Ids that don't exist (or are `embed_failed`) are simply absent.
///
/// # Errors
///
/// Returns `StoreError::Database` on any SQL failure.
pub async fn get_many_with_context(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<ChunkWithContext>> {
    let rows = sqlx::query_as::<_, ChunkWithContextRow>(
        "SELECT \
            c.id, c.source_version_id, c.document_id, c.node_id, c.chunk_index, c.total_chunks, \
            c.content, c.content_hash, c.embedding_model_id, c.heading_path, c.symbol_path, \
            c.start_byte, c.end_byte, c.token_count, c.status, c.created_at, \
            d.source_path AS d_source_path, d.published_url AS d_published_url, \
            d.source_url AS d_source_url, d.language AS d_language, d.kind AS d_kind, \
            d.provenance AS d_provenance, \
            s.slug AS s_slug, s.display_name AS s_display_name \
         FROM chunk c \
         JOIN document d ON c.document_id = d.id \
         JOIN source_version sv ON c.source_version_id = sv.id \
         JOIN source s ON sv.source_id = s.id \
         WHERE c.id = ANY($1) AND c.status <> 'embed_failed'",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}
```

- [ ] **Step 2: Add the route + handler** in `crates/mn-server/src/routes/chunks.rs`. Add to `router()`:

```rust
        .route("/v1/chunks", get(get_chunks_batch))
```

Add below the existing handlers:

```rust
/// Hard cap on ids per batch request (matches the MCP `get_chunks` cap).
const BATCH_IDS_CAP: usize = 20;

#[derive(Debug, Deserialize)]
struct BatchQuery {
    /// Comma-separated chunk UUIDs.
    ids: String,
}

async fn get_chunks_batch(
    Query(q): Query<BatchQuery>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let mut ids: Vec<Uuid> = Vec::new();
    for part in q.ids.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match part.parse::<Uuid>() {
            Ok(u) => ids.push(u),
            Err(_) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "invalid_ids",
                        "message": format!("`{part}` is not a valid UUID"),
                    })),
                )
                    .into_response()
            }
        }
    }
    if ids.is_empty() || ids.len() > BATCH_IDS_CAP {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_ids",
                "message": format!("ids must contain 1..={BATCH_IDS_CAP} UUIDs"),
            })),
        )
            .into_response();
    }
    match chunk::get_many_with_context(&state.pool, &ids).await {
        Ok(found) => {
            // Re-order to input order; collect ids that came back empty.
            let by_id: std::collections::HashMap<Uuid, _> =
                found.into_iter().map(|c| (c.chunk.id, c)).collect();
            let mut by_id = by_id;
            let mut chunks = Vec::with_capacity(ids.len());
            let mut missing = Vec::new();
            for id in &ids {
                match by_id.remove(id) {
                    Some(c) => chunks.push(c),
                    None => missing.push(*id),
                }
            }
            Json(serde_json::json!({ "chunks": chunks, "missing": missing })).into_response()
        }
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "get_chunks_batch failed");
            error::service_unavailable("batch chunk lookup failed", rid)
        }
    }
}
```

(Duplicate input ids resolve to the first occurrence and are then reported `missing` for the repeat — acceptable; do not special-case.)

- [ ] **Step 3: Unit-test the pure parts.** In `routes/chunks.rs` add a `#[cfg(test)] mod tests` exercising id parsing/cap via the handler-independent logic — extract parsing into a testable function first:

```rust
/// Parse + validate the comma-separated id list. Exposed for unit tests.
fn parse_batch_ids(raw: &str) -> std::result::Result<Vec<Uuid>, String> {
    let mut ids = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        ids.push(part.parse::<Uuid>().map_err(|_| format!("`{part}` is not a valid UUID"))?);
    }
    if ids.is_empty() || ids.len() > BATCH_IDS_CAP {
        return Err(format!("ids must contain 1..={BATCH_IDS_CAP} UUIDs"));
    }
    Ok(ids)
}
```

…and have the handler call it (replacing the inline loop; on `Err(msg)` return the 400 with that message). Tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_batch_ids_accepts_valid_list() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let ids = parse_batch_ids(&format!("{a}, {b}")).unwrap();
        assert_eq!(ids, vec![a, b]);
    }

    #[test]
    fn parse_batch_ids_rejects_garbage_empty_and_overflow() {
        assert!(parse_batch_ids("not-a-uuid").is_err());
        assert!(parse_batch_ids("").is_err());
        let many = vec![Uuid::new_v4().to_string(); 21].join(",");
        assert!(parse_batch_ids(&many).is_err());
    }
}
```

- [ ] **Step 4: Integration test (CI-only).** In the existing mn-server integration test file that covers chunk routes (find with `grep -rl "v1/chunks" crates/mn-server/tests/`), add a test seeding 3 chunks, requesting `/v1/chunks?ids=<c2>,<c1>,<unknown>` and asserting: 200; `chunks[0].id == c2`, `chunks[1].id == c1` (input order preserved); `missing == [unknown]`. Mark `#[cfg(feature = "integration")]` consistent with neighbors.

- [ ] **Step 5: Verify + commit**

Run: `cargo test -p mn-server --lib && cargo test -p mn-store --lib`
Expected: PASS (integration test compiles via `cargo check -p mn-server --features integration --tests`).

```bash
git add crates/mn-store/src/entities/chunk.rs crates/mn-server/src/routes/chunks.rs crates/mn-server/tests/
git commit -m "feat(mn-server): batch chunk fetch GET /v1/chunks?ids= (order-preserving, missing[] reporting)"
```

### Task 2: Sources keyset pagination + filters

**Files:**
- Create: `crates/mn-server/src/pagination.rs`
- Modify: `crates/mn-server/src/lib.rs` or `main.rs` module tree (add `mod pagination;` where other modules are declared), `crates/mn-server/Cargo.toml` (+`base64`), root `Cargo.toml` workspace deps if base64 absent
- Modify: `crates/mn-store/src/entities/source.rs`, `crates/mn-server/src/routes/sources.rs`

- [ ] **Step 1: Add base64 dep.** Check root `Cargo.toml` `[workspace.dependencies]` for `base64`; if absent add `base64 = "0.22"` there and `base64 = { workspace = true }` to `crates/mn-server/Cargo.toml`.

- [ ] **Step 2: Cursor helpers with tests** — `crates/mn-server/src/pagination.rs` (NEW):

```rust
//! Opaque keyset-cursor encoding shared by the paginated list endpoints.
//! A cursor is the base64url (no pad) encoding of the last-seen sort key.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

/// Encode a sort-key value as an opaque cursor token.
#[must_use]
pub fn encode_cursor(last_key: &str) -> String {
    URL_SAFE_NO_PAD.encode(last_key)
}

/// Decode a cursor token back to the sort-key value. `None` on any malformed input.
#[must_use]
pub fn decode_cursor(cursor: &str) -> Option<String> {
    URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(decode_cursor(&encode_cursor("compact-docs")).as_deref(), Some("compact-docs"));
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(decode_cursor("!!!not-base64!!!"), None);
    }
}
```

Register `pub mod pagination;` alongside the other module declarations in the mn-server crate root.

- [ ] **Step 3: Store paged query** — `crates/mn-store/src/entities/source.rs`, below `list_active` (keep `list_active`; the facets overview still uses it):

```rust
/// Filterable, keyset-paginated source listing. Page key is `slug` (unique,
/// matches the existing ORDER BY). Returns one page plus the total row count
/// for the same filter set (ignoring the cursor).
#[derive(Debug, Default)]
pub struct SourcePageQuery {
    /// Resume after this slug (exclusive). `None` = first page.
    pub after_slug: Option<String>,
    /// Page size (validated by the route: 1..=100).
    pub limit: i64,
    /// Only sources created strictly after this instant.
    pub created_after: Option<OffsetDateTime>,
    /// Only sources created strictly before this instant.
    pub created_before: Option<OffsetDateTime>,
    /// Only sources of this kind (wire string, e.g. "docs_site").
    pub kind: Option<String>,
    /// Include retired sources (default false = active only).
    pub include_retired: bool,
}

/// One page of sources plus pagination facts.
#[derive(Debug)]
pub struct SourcePage {
    /// The page rows, ordered by slug.
    pub sources: Vec<Source>,
    /// Total rows matching the filters (cursor ignored).
    pub total: i64,
    /// Slug to resume from when more rows exist.
    pub next_after_slug: Option<String>,
}

/// Run a [`SourcePageQuery`].
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn list_paged(pool: &PgPool, q: &SourcePageQuery) -> Result<SourcePage> {
    fn push_filters<'a>(
        b: &mut sqlx::QueryBuilder<'a, sqlx::Postgres>,
        q: &'a SourcePageQuery,
    ) {
        b.push(" WHERE 1=1");
        if !q.include_retired {
            b.push(" AND retired_at IS NULL");
        }
        if let Some(t) = q.created_after {
            b.push(" AND created_at > ").push_bind(t);
        }
        if let Some(t) = q.created_before {
            b.push(" AND created_at < ").push_bind(t);
        }
        if let Some(k) = &q.kind {
            b.push(" AND kind = ").push_bind(k);
        }
    }

    let mut count = sqlx::QueryBuilder::new("SELECT count(*) FROM source");
    push_filters(&mut count, q);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;

    let mut page = sqlx::QueryBuilder::new(
        "SELECT id, slug, display_name, kind, origin_url, retention_count, created_at, retired_at \
         FROM source",
    );
    push_filters(&mut page, q);
    if let Some(after) = &q.after_slug {
        page.push(" AND slug > ").push_bind(after);
    }
    page.push(" ORDER BY slug LIMIT ").push_bind(q.limit + 1);
    let rows: Vec<SourceRow> = page.build_query_as().fetch_all(pool).await?;

    let has_more = rows.len() > usize::try_from(q.limit).unwrap_or(usize::MAX);
    let mut sources: Vec<Source> = rows
        .into_iter()
        .take(usize::try_from(q.limit).unwrap_or(usize::MAX))
        .map(TryInto::try_into)
        .collect::<Result<_>>()?;
    let next_after_slug = if has_more {
        sources.last().map(|s| s.slug.clone())
    } else {
        None
    };
    // `sources` is already capped at `limit`; binding `next_after_slug` before
    // returning keeps the borrow checker happy.
    let _ = &mut sources;
    Ok(SourcePage { sources, total, next_after_slug })
}
```

(`SourceRow` must derive `sqlx::FromRow` already — it does. `build_query_as` needs that.)

- [ ] **Step 4: Route rework** — replace `list_sources` in `crates/mn-server/src/routes/sources.rs`:

```rust
use crate::pagination::{decode_cursor, encode_cursor};
use axum::extract::Query;
use serde::Deserialize;

const SOURCES_DEFAULT_LIMIT: i64 = 20;
const SOURCES_MAX_LIMIT: i64 = 100;

#[derive(Debug, Deserialize)]
struct SourcesQuery {
    cursor: Option<String>,
    limit: Option<i64>,
    /// RFC3339 timestamp.
    created_after: Option<String>,
    /// RFC3339 timestamp.
    created_before: Option<String>,
    kind: Option<String>,
    #[serde(default)]
    retired: bool,
}

fn bad_request(message: String) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": "invalid_query", "message": message })),
    )
        .into_response()
}

fn parse_rfc3339(name: &str, v: Option<&str>) -> std::result::Result<Option<time::OffsetDateTime>, String> {
    v.map(|s| {
        time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
            .map_err(|_| format!("`{name}` must be an RFC3339 timestamp"))
    })
    .transpose()
}

async fn list_sources(
    Query(q): Query<SourcesQuery>,
    State(state): State<AppState>,
    Extension(req_id): Extension<RequestId>,
) -> Response {
    let rid = req_id.as_str();
    let limit = q.limit.unwrap_or(SOURCES_DEFAULT_LIMIT);
    if !(1..=SOURCES_MAX_LIMIT).contains(&limit) {
        return bad_request(format!("limit must be in 1..={SOURCES_MAX_LIMIT}"));
    }
    let after_slug = match q.cursor.as_deref() {
        None => None,
        Some(c) => match decode_cursor(c) {
            Some(s) => Some(s),
            None => return bad_request("cursor is malformed".to_owned()),
        },
    };
    let created_after = match parse_rfc3339("created_after", q.created_after.as_deref()) {
        Ok(v) => v,
        Err(m) => return bad_request(m),
    };
    let created_before = match parse_rfc3339("created_before", q.created_before.as_deref()) {
        Ok(v) => v,
        Err(m) => return bad_request(m),
    };
    if let Some(k) = q.kind.as_deref() {
        if !matches!(k, "docs_site" | "code_repo" | "standalone" | "mixed") {
            return bad_request(format!("unknown kind `{k}`"));
        }
    }
    let page_q = source::SourcePageQuery {
        after_slug,
        limit,
        created_after,
        created_before,
        kind: q.kind,
        include_retired: q.retired,
    };
    match source::list_paged(&state.pool, &page_q).await {
        Ok(page) => Json(serde_json::json!({
            "sources": page.sources,
            "total": page.total,
            "next_cursor": page.next_after_slug.as_deref().map(encode_cursor),
        }))
        .into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, op = "list_sources", error = %e, "store error");
            error::service_unavailable("list_sources failed", rid)
        }
    }
}
```

**This changes the `/v1/sources` response from a bare array to an object** — Task 10 updates the MCP client; any mn-cli `sources list` rendering that deserializes the bare array must be updated in the same commit (`grep -rn "v1/sources" crates/mn-cli/src/` and fix `commands/sources.rs` list path to read `.sources`).

- [ ] **Step 5: Integration test (CI-only).** In the mn-server integration suite covering sources: seed 3 sources with distinct slugs/kinds/created_at (one retired); assert (a) default call returns object with `sources` ordered by slug, `total`, `next_cursor: null`; (b) `?limit=1` walks all pages via `next_cursor` with no overlaps/gaps; (c) `?kind=docs_site` filters; (d) `?retired=true` includes the retired row; (e) `?limit=0` → 400.

- [ ] **Step 6: Verify + commit**

Run: `cargo test -p mn-server --lib && cargo test -p mn-store --lib && cargo test -p mn-cli`
Expected: PASS.

```bash
git add crates/mn-server crates/mn-store crates/mn-cli Cargo.toml Cargo.lock
git commit -m "feat(mn-server): keyset pagination + created_at/kind/retired filters on GET /v1/sources"
```

### Task 3: Facets drill-down

**Files:**
- Modify: `crates/mn-server/src/routes/facets.rs`

- [ ] **Step 1: Add query params + dispatch.** `get_facets` gains `Query<FacetsQuery>`:

```rust
#[derive(Debug, Deserialize)]
struct FacetsQuery {
    /// When present: return a paginated value list for this one facet.
    facet: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}
```

In the handler, before the cache check: `if let Some(f) = q.facet.as_deref() { return facet_values_page(&state, rid, f, q.cursor.as_deref(), q.limit).await; }`. The overview path (no `facet`) keeps the TTL cache unchanged.

- [ ] **Step 2: Drill-down implementation** (same file):

```rust
const DRILL_DEFAULT_LIMIT: i64 = 50;
const DRILL_MAX_LIMIT: i64 = 200;

/// Per-facet (sql_page, sql_count) for the drillable open-set facets. The page
/// query takes ($1 = after_value, $2 = limit+1) and yields a `v` text column.
fn drill_queries(facet: &str) -> Option<(&'static str, &'static str)> {
    match facet {
        "source_slug" => Some((
            "SELECT s.slug AS v FROM source s WHERE s.retired_at IS NULL AND s.slug > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(*) FROM source s WHERE s.retired_at IS NULL",
        )),
        "language" => Some((
            "SELECT DISTINCT d.language AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             WHERE sv.is_active = true AND d.language IS NOT NULL AND d.language > $1 \
             ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT d.language) FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             WHERE sv.is_active = true AND d.language IS NOT NULL",
        )),
        "tags" => Some((
            "SELECT DISTINCT tag AS v FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements_text(COALESCE(d.provenance->'tags','[]'::jsonb)) AS tag \
             WHERE sv.is_active = true AND tag > $1 ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT tag) FROM document d \
             JOIN source_version sv ON sv.id = d.source_version_id \
             CROSS JOIN LATERAL jsonb_array_elements_text(COALESCE(d.provenance->'tags','[]'::jsonb)) AS tag \
             WHERE sv.is_active = true",
        )),
        "package" => Some((
            "SELECT DISTINCT p.name AS v FROM package p \
             JOIN source_version sv ON sv.id = p.source_version_id \
             WHERE sv.is_active = true AND p.name > $1 ORDER BY v LIMIT $2",
            "SELECT count(DISTINCT p.name) FROM package p \
             JOIN source_version sv ON sv.id = p.source_version_id \
             WHERE sv.is_active = true",
        )),
        _ => None,
    }
}

async fn facet_values_page(
    state: &AppState,
    rid: &str,
    facet: &str,
    cursor: Option<&str>,
    limit: Option<i64>,
) -> Response {
    use sqlx::Row as _;
    let limit = limit.unwrap_or(DRILL_DEFAULT_LIMIT);
    if !(1..=DRILL_MAX_LIMIT).contains(&limit) {
        return bad_request(format!("limit must be in 1..={DRILL_MAX_LIMIT}"));
    }
    let Some((page_sql, count_sql)) = drill_queries(facet) else {
        return bad_request(format!(
            "facet `{facet}` is not drillable; drillable facets: source_slug, language, tags, package \
             (closed-enum facets list all values in the overview)"
        ));
    };
    let after = match cursor {
        None => String::new(), // "" sorts before all non-empty text
        Some(c) => match crate::pagination::decode_cursor(c) {
            Some(s) => s,
            None => return bad_request("cursor is malformed".to_owned()),
        },
    };
    let total: i64 = match sqlx::query_scalar(count_sql).fetch_one(&state.pool).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "facet count failed");
            return error::service_unavailable("facet value count failed", rid);
        }
    };
    let rows = match sqlx::query(page_sql)
        .bind(&after)
        .bind(limit + 1)
        .fetch_all(&state.pool)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "facet page failed");
            return error::service_unavailable("facet value page failed", rid);
        }
    };
    let mut values: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("v").ok())
        .collect();
    let has_more = values.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    values.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next_cursor = if has_more {
        values.last().map(|v| crate::pagination::encode_cursor(v))
    } else {
        None
    };
    Json(json!({ "facet": facet, "values": values, "total": total, "next_cursor": next_cursor }))
        .into_response()
}
```

Copy the `bad_request` helper from Task 2 Step 4 into this module too (or move it into `crate::error` as `pub fn bad_request(message: impl Into<String>) -> Response` and use it from both — preferred; do that and update Task 1/2 call sites).

- [ ] **Step 3: Trim the overview samples.** In `corpus_values`, change `VALUE_CAP` from `200` to `10` and rename it `SAMPLE_CAP`. For `tags` and `package`, replace the capped `total` (currently "cap+1 or more") with the exact `count(DISTINCT …)` queries from `drill_queries` (run alongside; still cached 60s). The `language`/`source_slug` blocks stay full (low cardinality), but cap their `values` arrays at `SAMPLE_CAP` as well, setting `truncated`/`total` accordingly.

- [ ] **Step 4: Integration test (CI-only).** Seed documents with 15 distinct tags. Assert: overview `tags.values.len() == 10`, `truncated == true`, `total == 15`; `?facet=tags&limit=10` returns 10 values + `next_cursor`; second page returns remaining 5 with `next_cursor: null`; `?facet=kind` → 400 (closed enum); `?facet=bogus` → 400.

- [ ] **Step 5: Verify + commit**

Run: `cargo test -p mn-server --lib`
Expected: PASS.

```bash
git add crates/mn-server/src
git commit -m "feat(mn-server): facets drill-down (?facet=&cursor=&limit=) + 10-value overview samples with exact totals"
```

### Task 4: `GET /v1/me`

**Files:**
- Create: `crates/mn-server/src/routes/me.rs`
- Modify: `crates/mn-server/src/routes/mod.rs` (+`pub mod me;`), `crates/mn-server/src/app.rs` (+`.merge(crate::routes::me::router())`)

- [ ] **Step 1: Write the route:**

```rust
//! `GET /v1/me` — auth + limit introspection for clients (MCP `status`,
//! `mnm status`). Anonymous calls succeed and report `authenticated: false`.
//!
//! Callers carry TWO independent limit systems and this endpoint reports both:
//! the request rate limit (req/s token bucket, `ratelimit.rs`) and the
//! embedding token budget (rolling hourly/daily windows, `tokenlimit.rs`,
//! charged by `POST /v1/embeddings`).

use axum::extract::{Extension, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use time::OffsetDateTime;

use crate::app::AppState;
use crate::middleware::bearer::AuthContext;
use crate::middleware::rate_limit::RateLimitContext;
use crate::ratelimit::Decision;
use crate::tokenlimit::TokenTier;

/// Mount the introspection route.
#[must_use]
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/me", get(me))
}

const fn token_tier_str(t: TokenTier) -> &'static str {
    match t {
        TokenTier::Anonymous => "anonymous",
        TokenTier::ReadUplift => "read_uplift",
        TokenTier::Admin => "admin",
    }
}

async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthContext>>,
    rl: Option<Extension<RateLimitContext>>,
) -> Response {
    let auth = auth.map(|Extension(a)| a);
    let (auth_type, identity, permission_level) = match &auth {
        None => ("anonymous", None, "read"),
        Some(a) => {
            let t = match a.tier {
                mn_auth::Tier::Admin => "admin",
                mn_auth::Tier::ReadUplift => "github_oauth",
            };
            let p = if a.can_admin() {
                "admin"
            } else if a.can_write() {
                "write"
            } else {
                "read"
            };
            (t, Some(a.sub.clone()), p)
        }
    };
    // Request rate limit: peek the caller's bucket without spending a token
    // (cost 0). The RateLimitContext extension exists whenever the limiter is
    // enabled.
    let rate_limit = rl.and_then(|Extension(ctx)| {
        state.rate_limiter.as_ref().map(|limiter| {
            let (remaining, reset_secs) = match limiter.charge(&ctx.key, ctx.limit, 0) {
                Decision::Allowed { remaining, reset_secs } => (remaining, reset_secs),
                Decision::Rejected { retry_after_secs } => (0, retry_after_secs),
            };
            json!({
                "tier": ctx.tier.as_str(),
                "limit": ctx.limit,
                "remaining": remaining,
                "reset_secs": reset_secs,
            })
        })
    });
    // Embedding token budget: same resolve + non-consuming snapshot the
    // embeddings route uses (embeddings.rs:196-249).
    let client_ip =
        crate::middleware::rate_limit::client_ip(&headers, &state.cfg.rate_limit_client_ip_header);
    let (subject, token_tier, limits) = state.token_limiter.resolve(&client_ip, auth.as_ref());
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let usage = state.token_limiter.snapshot_for(&subject, limits, now);
    let window = |w: crate::tokenlimit::WindowInfo| {
        json!({ "limit": w.limit, "remaining": w.remaining, "reset_at_secs": w.reset_at_secs })
    };
    let token_limits = json!({
        "tier": token_tier_str(token_tier),
        "hourly": window(usage.hour),
        "daily": window(usage.day),
    });
    Json(json!({
        "authenticated": auth.is_some(),
        "auth_type": auth_type,
        "identity": identity,
        "permission_level": permission_level,
        "rate_limit": rate_limit,
        "token_limits": token_limits,
        "server_version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}
```

(`rate_limit` is a single object — one request bucket per caller — while `token_limits` carries the two embedding windows. `server_version` rides along here so the MCP `status` tool gets the cloud version without a second endpoint.)

- [ ] **Step 2: Check visibility.** `RateLimitContext`, `ratelimit::Tier::as_str`, `tokenlimit::{TokenTier, WindowInfo}`, `TokenUsageLimiter::{resolve, snapshot_for}` are all `pub` already; `middleware::rate_limit::client_ip` is `pub(crate)` (rate_limit.rs:~44) — that's sufficient from another module in the same crate. If `mn_auth::Tier` import differs, mirror the import used in `ratelimit.rs`.

- [ ] **Step 3: Integration test (CI-only):** anonymous GET `/v1/me` → `authenticated:false, auth_type:"anonymous", permission_level:"read", server_version` non-empty, `token_limits.tier == "anonymous"` with `hourly.limit > 0` and `daily.limit > 0`; with an admin JWT (reuse the suite's existing JWT mint helper) → `auth_type:"admin"`, `identity` = sub, `rate_limit.limit > 0`, `token_limits.tier == "admin"`; `rate_limit.remaining` unchanged across two consecutive `/v1/me` calls plus one `/v1/sources` call in between decrements it (proves peek doesn't spend); and `token_limits.hourly.remaining` drops after a `POST /v1/embeddings` call but NOT after `/v1/me` itself (proves `snapshot_for` doesn't charge).

- [ ] **Step 4: Verify + commit**

Run: `cargo test -p mn-server --lib`
Expected: PASS.

```bash
git add crates/mn-server/src
git commit -m "feat(mn-server): GET /v1/me auth + rate-limit introspection"
```

### Task 5: Parents enrichment (document_id + source)

**Files:**
- Modify: `crates/mn-store/src/entities/node.rs`, `crates/mn-server/src/routes/chunks.rs` (`get_parents`)

- [ ] **Step 1: Enrich the store query.** In `node.rs`, add a response-shaped struct + new query (keep `parent_chain` if other callers exist — check `grep -rn "parent_chain" crates/`; if only the route uses it, replace it):

```rust
/// One ancestor in a chunk's parent chain, with the document id attached when
/// the node is a document node (group/root nodes have `document_id: None`).
#[derive(Debug, Clone, Serialize)]
pub struct ParentNode {
    /// Node id (structural hierarchy id — NOT a document id).
    pub id: Uuid,
    /// Owning source version.
    pub source_version_id: Uuid,
    /// Parent node id (`None` for the root).
    pub parent_node_id: Option<Uuid>,
    /// `document` / `group` / `root`.
    pub kind: String,
    /// Display name (file or folder name; `root` for the root).
    pub name: String,
    /// Sibling order.
    pub order_index: i32,
    /// The fetchable document id, present only on `kind == "document"` nodes.
    pub document_id: Option<Uuid>,
}

/// [`parent_chain`] + a LEFT JOIN to `document` so document-kind nodes carry
/// their fetchable document id. Ordered immediate parent → root.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure.
pub async fn parent_chain_with_documents(pool: &PgPool, node_id: Uuid) -> Result<Vec<ParentNode>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        source_version_id: Uuid,
        parent_node_id: Option<Uuid>,
        kind: String,
        name: String,
        order_index: i32,
        document_id: Option<Uuid>,
    }
    let rows = sqlx::query_as::<_, Row>(
        "WITH RECURSIVE chain AS ( \
             SELECT id, source_version_id, parent_node_id, kind, name, order_index, 0 AS depth \
             FROM node WHERE id = $1 \
             UNION ALL \
             SELECT n.id, n.source_version_id, n.parent_node_id, n.kind, n.name, n.order_index, c.depth + 1 \
             FROM node n JOIN chain c ON n.id = c.parent_node_id \
         ) \
         SELECT chain.id, chain.source_version_id, chain.parent_node_id, chain.kind, chain.name, \
                chain.order_index, d.id AS document_id \
         FROM chain LEFT JOIN document d ON d.node_id = chain.id \
         WHERE chain.depth > 0 ORDER BY chain.depth",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ParentNode {
            id: r.id,
            source_version_id: r.source_version_id,
            parent_node_id: r.parent_node_id,
            kind: r.kind,
            name: r.name,
            order_index: r.order_index,
            document_id: r.document_id,
        })
        .collect())
}
```

(If `Node.kind` is an enum in the existing `NodeRow`, mirror its decoding instead of raw `String` — match whatever `parent_chain` does today.)

- [ ] **Step 2: Reshape the route response.** In `get_parents` (`routes/chunks.rs`), the handler already fetched `parent_chunk` (which carries `.source`). Replace the tail:

```rust
    match node::parent_chain_with_documents(&state.pool, parent_chunk.chunk.node_id).await {
        Ok(chain) => Json(serde_json::json!({
            "parents": chain,
            "source": parent_chunk.source,
        }))
        .into_response(),
        Err(e) => {
            tracing::warn!(request_id = rid, error = %e, "parent_chain failed");
            error::service_unavailable("parent-chain lookup failed", rid)
        }
    }
```

**Wire change:** response goes from bare array → `{parents, source}`. The MCP projector is updated in Task 14 (its current `project_parents` wraps the array itself — it must stop wrapping). Check mn-cli `commands/chunks` for a parents subcommand and update its deserialization in this commit if present.

- [ ] **Step 3: Integration test (CI-only):** seed a doc at `guides/intro.md`; GET parents of one of its chunks; assert first parent has `kind:"document"` and non-null `document_id` equal to the seeded document's id; the last has `kind:"root"` and `document_id: null`; top-level `source.slug` matches.

- [ ] **Step 4: Verify + commit**

Run: `cargo test -p mn-server --lib && cargo test -p mn-store --lib && cargo test -p mn-cli`

```bash
git add crates/mn-store crates/mn-server crates/mn-cli
git commit -m "feat(mn-server): parents response carries document_id per node + top-level source"
```

### Task 6: Document overview skeleton; delete `/full`

**Files:**
- Modify: `crates/mn-store/src/entities/document.rs`, `crates/mn-server/src/routes/documents.rs`

- [ ] **Step 1: Replace `chunk_ids` with a skeleton.** In `document.rs`:

```rust
/// Per-chunk skeleton entry in a document overview: position + cost, no body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkSkeleton {
    /// Chunk id (feed to `GET /v1/chunks?ids=`).
    pub id: Uuid,
    /// Position within the document.
    pub chunk_index: i32,
    /// Token count of the chunk body.
    pub token_count: i32,
}
```

In `DocumentOverview`, replace `pub chunk_ids: Vec<Uuid>` with `pub chunks: Vec<ChunkSkeleton>`. In `get_overview`, replace the chunk-id select with:

```rust
    let chunks = sqlx::query_as::<_, ChunkSkeleton>(
        "SELECT id, chunk_index, token_count FROM chunk \
         WHERE document_id = $1 AND status <> 'embed_failed' ORDER BY chunk_index",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
```

(`ChunkSkeleton` needs `#[derive(sqlx::FromRow)]` too. Mirror whatever status predicate the current chunk_ids query uses — keep it identical.)

- [ ] **Step 2: Delete the full-document path.** Remove from `document.rs`: `DocumentFull`, `FullResult`, `get_full`. Remove from `routes/documents.rs`: the `/v1/documents/:id/full` route line, `get_document_full` handler, `DOCUMENT_FULL_CHUNK_CAP`, `effective_cap`. Keep `ChunkBody`, `DocumentChunkWindow`, `list_chunks_window`, and the `/chunks` route untouched.

- [ ] **Step 3: Sweep callers.** `grep -rn "get_full\|DocumentFull\|FullResult\|documents/.*full\|DOCUMENT_FULL_CHUNK_CAP" crates/ specs/` — expect hits in mn-mcp (handled in Tasks 8/13; leave compile-broken only if you're doing Tasks 6→8 in one sitting, otherwise stub `CloudClient::get_document_full` removal forward by doing Task 8's deletion steps for it now so the workspace compiles). mn-cli `commands/documents` has a `full` subcommand — remove it (pre-1.0; the `chunks` subcommand remains the body-reader).

- [ ] **Step 4: Update affected tests:** any mn-server/mn-store/mn-cli tests referencing `chunk_ids`, `/full`, or 412 too-many-chunks get updated/deleted. Integration test: overview returns `chunks[0] == {id, chunk_index: 0, token_count > 0}`.

- [ ] **Step 5: Verify + commit**

Run: `cargo test -p mn-server --lib && cargo test -p mn-store --lib && cargo test -p mn-cli && VOYAGE_API_KEY= cargo test -p mn-mcp`
Expected: PASS (mn-mcp may already be touched by Step 3; that's fine — keep the workspace green).

```bash
git add crates/ && git commit -m "feat!(mn-server): document overview returns chunk skeletons; remove /full endpoint + 500-chunk cap"
```

### Task 7: openapi.yaml for Phase 1

**Files:**
- Modify: `specs/001-rag-platform/contracts/openapi.yaml`

- [ ] **Step 1:** Document, matching existing style (hand-written YAML, `$ref` components): `GET /chunks` (ids query param, `{chunks, missing}` 200, 400), `/sources` query params + new `{sources, total, next_cursor}` response object, `/facets` `facet`/`cursor`/`limit` params + drill-down response, `GET /me`, `/chunks/{id}/parents` new `{parents, source}` shape with `document_id`, `/documents/{id}` `chunks` skeleton array, and DELETE the `/documents/{id}/full` path + `DocumentFull` schema. Add `ChunkSkeleton`, `SourcePage`, `Me`, `FacetValuesPage`, `ParentNode` component schemas.

- [ ] **Step 2: Commit**

```bash
git add specs/001-rag-platform/contracts/openapi.yaml
git commit -m "docs(contracts): openapi for batch chunks, paged sources, facets drill-down, /v1/me, parents + overview reshape"
```

---

## Phase 2 — mn-mcp foundations

### Task 8: CloudClient methods

**Files:**
- Modify: `crates/mn-mcp/src/cloud_client.rs`

- [ ] **Step 1: Remove the dead path.** Delete `get_document_full`, `parse_too_many_chunks`, and `CloudError::TooManyChunks` (compile errors lead to the server.rs/render.rs call sites — delete those arms now too: the `TooManyChunks` mapping in `cloud_failure` and `ErrorKind::TooManyChunks` in render.rs, including its `code()`/`retryable()` arms).

- [ ] **Step 2: Add new methods** (same style as `get_chunk`/`list_sources`):

```rust
    /// `GET /v1/chunks?ids=a,b,c` — batch fetch, input order preserved server-side.
    pub async fn get_chunks(&self, ids: &[String]) -> Result<serde_json::Value, CloudError> {
        let path = format!("/v1/chunks?ids={}", ids.join(","));
        self.get_json(&path).await
    }

    /// `GET /v1/sources` with pagination/filter params. Pairs are appended as
    /// query params verbatim (values must already be URL-safe: cursors are
    /// base64url, kinds are enum tokens, timestamps RFC3339 — `:` and `+` are
    /// percent-encoded here to be safe).
    pub async fn list_sources(
        &self,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, CloudError> {
        let mut url = self
            .base
            .join("/v1/sources")
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
        self.get_json_url(url).await
    }

    /// `GET /v1/facets` — overview when `params` is empty, drill-down otherwise.
    pub async fn get_facets(
        &self,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, CloudError> {
        let mut url = self
            .base
            .join("/v1/facets")
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
        self.get_json_url(url).await
    }

    /// `GET /v1/me` — auth/rate-limit introspection.
    pub async fn get_me(&self) -> Result<serde_json::Value, CloudError> {
        self.get_json("/v1/me").await
    }

    /// `GET /readyz` — returns the HTTP status code (no body parsing).
    pub async fn readyz(&self) -> Result<u16, CloudError> {
        let url = self
            .base
            .join("/readyz")
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| CloudError::Transport(e.to_string()))?;
        Ok(resp.status().as_u16())
    }
```

The existing `list_sources(&self)` (no args) is replaced by the params version; `get_facets()` likewise. `get_json_url` is a small refactor of the existing `get_json` taking a pre-built `Url` (extract the body of `get_json` after the join into `get_json_url(&self, url: Url)`; `get_json` calls it).

- [ ] **Step 3: Tests.** cloud_client has wiremock-style or unit tests? (`grep -rn "mod tests" crates/mn-mcp/src/cloud_client.rs`). Add/extend: `get_chunks` builds `?ids=a,b`; `list_sources(&[("cursor","abc".into()),("limit","20".into())])` produces both query params (assert via `Url` construction or a wiremock echo in `crates/mn-mcp/tests/`, matching the existing test idiom found in the file).

- [ ] **Step 4: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`
Expected: render.rs/server.rs may still reference removed tools — if so finish the mechanical deletions they point at (they are completed properly in Tasks 13/17). Workspace must compile at commit time.

```bash
git add crates/mn-mcp && git commit -m "feat(mn-mcp): cloud client batch/paged/introspection methods; drop document-full path"
```

### Task 9: render.rs core — `suggested_next_actions` with descriptions

**Files:**
- Modify: `crates/mn-mcp/src/render.rs`, `crates/mn-mcp/src/schemas.rs`

- [ ] **Step 1: New `NextAction` shape:**

```rust
/// A suggested follow-up surfaced to the agent. `tool: None` describes a user
/// action (e.g. "ask the user to restart the harness") rather than a tool call.
#[derive(Debug, Clone)]
pub struct NextAction {
    /// What this action achieves, as a human-written sentence.
    pub description: String,
    /// Tool name to call next (`None` for user actions).
    pub tool: Option<&'static str>,
    /// Arguments object for that call (`None` for user actions).
    pub arguments: Option<Value>,
}

impl NextAction {
    /// Tool-call action.
    pub fn call(description: impl Into<String>, tool: &'static str, arguments: Value) -> Self {
        Self { description: description.into(), tool: Some(tool), arguments: Some(arguments) }
    }

    /// User action (no tool).
    pub fn user(description: impl Into<String>) -> Self {
        Self { description: description.into(), tool: None, arguments: None }
    }

    fn to_value(&self) -> Value {
        let mut o = json!({ "description": self.description });
        if let Some(t) = self.tool {
            o["tool"] = json!(t);
        }
        if let Some(a) = &self.arguments {
            o["arguments"] = a.clone();
        }
        o
    }
}
```

- [ ] **Step 2: Rename the wire key.** In `ToolOutcome::into_result` change `map.insert("next_actions", …)` → `"suggested_next_actions"`. In `ToolFailure::into_result` change the `structured` json key `"next_actions"` → `"suggested_next_actions"`. Rename `ToolOutcome.next_actions` field → `suggested_next_actions` (and the `ToolFailure` field) for consistency.

- [ ] **Step 3: Mechanically update every projector** to the new constructor, with real descriptions (final wording lands per-tool in Phase 3 tasks; for projectors NOT otherwise reworked in Phase 3 — `project_chunk_list`, `project_neighbors` — write them now):

```rust
// project_chunk_list ("after" branch):
NextAction::call("Continue reading past the last returned chunk", "get_chunk_next", json!({ "id": last }))
// ("before" branch):
NextAction::call("Continue reading before the first returned chunk", "get_chunk_prev", json!({ "id": first }))
// project_neighbors:
NextAction::call("Fetch the parent document's overview and chunk map", "get_document", json!({ "id": d }))
```

- [ ] **Step 4: Schema fragment.** In `schemas.rs` rename `next_actions_fragment` → `suggested_next_actions_fragment`:

```rust
fn suggested_next_actions_fragment() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "What this suggested action achieves. Actions are suggestions, not required next steps." },
                "tool": { "type": "string", "description": "Tool to call. Absent for actions the user (not the agent) must take." },
                "arguments": { "type": "object" }
            },
            "required": ["description"]
        }
    })
}
```

…and update `passthrough_object_schema` + `search_output_schema` to use the new key `suggested_next_actions`.

- [ ] **Step 5: Fix every render test** asserting `next_actions`/`tool` (e.g. `sc["next_actions"][0]["tool"]` → `sc["suggested_next_actions"][0]["tool"]`; `o.next_actions[0].tool` → `o.suggested_next_actions[0].tool == Some("get_chunk")`), plus `tests/tools_dispatch.rs` and `tests/result_shape.rs` occurrences (`grep -rn "next_actions" crates/mn-mcp/`).

- [ ] **Step 6: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`
Expected: PASS.

```bash
git add crates/mn-mcp && git commit -m "feat!(mn-mcp): suggested_next_actions with prose descriptions; user-action entries"
```

### Task 10: Snippet helper + chunk-list text upgrades

**Files:**
- Modify: `crates/mn-mcp/src/render.rs`

- [ ] **Step 1: Helper + test:**

```rust
/// First ~150 chars of a chunk body on a char boundary, ellipsised.
fn snippet(content: &str) -> String {
    const MAX: usize = 150;
    if content.chars().count() <= MAX {
        return content.to_owned();
    }
    let head: String = content.chars().take(MAX).collect();
    format!("{head}…")
}

/// Trimmed per-chunk entry for multi-chunk text fences.
fn chunk_brief(c: &Value) -> Value {
    json!({
        "id": c.get("id").cloned().unwrap_or(Value::Null),
        "source_path": c.pointer("/document/source_path").cloned().unwrap_or(Value::Null),
        "heading_path": c.get("heading_path").cloned().unwrap_or(json!([])),
        "snippet": c.get("content").and_then(Value::as_str).map(snippet),
    })
}
```

Test: `snippet` of a 200-char string is 151 chars ending in `…`; a 10-char string is unchanged; multibyte input doesn't panic (use a string of 200 `é`).

- [ ] **Step 2: Use in `project_chunk_list` and `project_neighbors`.** Replace their `trimmed` values: `project_chunk_list` → `json!({ "count": chunks_len, "chunks": env["chunks"].as_array().map(|a| a.iter().map(chunk_brief).collect::<Vec<_>>()).unwrap_or_default() })`; `project_neighbors` → prev/next counts plus `chunks` briefs for both sides and the anchor. Update the affected tests to assert a `snippet` key exists.

- [ ] **Step 3: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add crates/mn-mcp && git commit -m "feat(mn-mcp): per-chunk snippets in multi-chunk text fences"
```

---

## Phase 3 — Tool surface

### Task 11: Search split (`search` + `advanced_search`)

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`, `src/server.rs`, `src/render.rs`, `crates/mn-skills/src/lib.rs`

- [ ] **Step 1: Skill-installed probe** in `crates/mn-skills/src/lib.rs` (near `detect`):

```rust
/// `true` when the bundled skill's `SKILL.md` exists for ANY harness at user
/// scope. Used by the MCP search projector's low-result nudge.
#[must_use]
pub fn installed_anywhere(env: &impl SkillEnv) -> bool {
    let Ok(base) = base_dir(Scope::User, env) else {
        return false;
    };
    Harness::ALL
        .iter()
        .any(|h| h.skill_file(Scope::User, &base).exists())
}
```

(Match the actual signatures of `base_dir`/`skill_file` in `install.rs` — adjust `Scope::User` token to the real enum path. Unit test with a temp-dir `SkillEnv` fake: false on empty dir, true after writing the file at the claude-code path.)

- [ ] **Step 2: Two input schemas** in `tools.rs`. Replace `search_input_schema()`:

```rust
fn search_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "minLength": 1,
                "description": "What you want to find, as natural language or code terms." },
            "mode": { "type": "string", "enum": ["hybrid", "vector", "fts"], "default": "hybrid",
                "description": "hybrid (default) fuses keyword + semantic; fts is keyword-only (lowest latency); vector is semantic-only." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10,
                "description": "Max results returned." }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

fn advanced_search_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "queries": { "type": "array", "minItems": 1, "maxItems": 10,
                "items": { "type": "string", "minLength": 1 },
                "description": "1-10 query variants fused with RRF (HyDE, expansion, step-back). One query = one-element array. Rate-limit cost is one token per distinct query." },
            "mode": { "type": "string", "enum": ["hybrid", "vector", "fts"], "default": "hybrid",
                "description": "hybrid (default) fuses keyword + semantic; fts is keyword-only; vector is semantic-only." },
            "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10,
                "description": "Max results returned." },
            "rerank": { "type": "boolean", "default": true,
                "description": "Apply cross-encoder reranking against the first query. Disable for lowest latency." },
            "filters": <KEEP the existing filters schema object verbatim from the current search_input_schema>
        },
        "required": ["queries"],
        "additionalProperties": false
    })
}
```

(Move the existing `filters` schema literal into a `fn filters_schema() -> serde_json::Value` and reference it from `advanced_search_input_schema` — basic `search` does not include it.)

- [ ] **Step 3: Split the parser.** `parse_search_args` (tools.rs:767) currently handles the oneOf. Refactor into:

```rust
fn parse_basic_search_args(v: &serde_json::Value) -> Result<ParsedSearchArgs, String> {
    // accepts: query (required string), mode, limit. rerank fixed = true, filters = None.
}
fn parse_advanced_search_args(v: &serde_json::Value) -> Result<ParsedSearchArgs, String> {
    // accepts: queries (required array 1..=10), mode, limit, rerank, filters.
}
```

Both produce the existing `ParsedSearchArgs` (queries vec, mode, limit, rerank, filters) so `run_search`'s body is untouched. Implement by splitting the current function's branches; reject unknown keys the way the current parser does (if it doesn't, don't add that behavior). `run_search` gains no signature change — the dispatcher picks the parser. Give `run_search` a `parsed: ParsedSearchArgs` parameter instead of re-parsing internally if that's the current flow; follow the existing seam (the dispatch currently calls `run_search(&params.arguments, cfg, cloud)` — change to parse in the dispatcher and pass `ParsedSearchArgs`).

Unit tests (mirror the existing `parse_search_args` tests): basic rejects `queries`/`filters`/`rerank` keys (additionalProperties false at schema level; the parser should also ignore-or-reject consistently with current behavior — pick reject with a clear message); advanced rejects `query`, accepts 10 queries, rejects 11.

- [ ] **Step 4: Projector rework.** Change `project_search` signature:

```rust
pub struct SearchRenderOpts {
    /// Reranker model name when local rerank ran.
    pub reranker_used: Option<String>,
    /// `true` for advanced_search (keeps matched_queries; basic strips it).
    pub advanced: bool,
    /// Whether the midnight-advanced-search skill is installed locally.
    pub skill_installed: bool,
}
pub fn project_search(envelope: Value, opts: &SearchRenderOpts) -> ToolOutcome
```

Inside:
1. `total_candidates` = `envelope.pointer("/search_metadata/total_candidates").and_then(Value::as_u64)` (verify the field name against a real envelope — `grep -n "total_candidates" crates/mn-server/src/routes/search.rs`; if it's under a different key, use that one).
2. Summary (no corpus model, no "fetch with get_chunk" tail):
   `"{result_count} matches ({candidates} candidates). Top: {path} › {heading} [{attr} · {conf:.2}]."` — when `total_candidates` is absent omit the parenthetical. Zero results: `"0 matches ({candidates} candidates)."`.
3. **Nudge:** when `total_candidates.unwrap_or(0) < 5 && !opts.skill_installed`, append `"\nFew candidates matched — the midnight-advanced-search skill teaches query patterns that find more (run install_search_skill)."` to the summary AND push `NextAction::call("Install the midnight-advanced-search skill to learn higher-recall query patterns", "install_search_skill", json!({}))`.
4. **matched_queries strip (basic only):** when `!opts.advanced`, iterate `structured["results"]` and `map.remove("matched_queries")` from each result's `scores` object before constructing the outcome.
5. `suggested_next_actions` (top result `t`, `top5` = first 5 chunk_ids):

```rust
let mut actions = Vec::new();
if let Some(id) = t.get("chunk_id").and_then(Value::as_str) {
    actions.push(NextAction::call("Fetch the top-ranked chunk's full content", "get_chunks", json!({ "ids": [id] })));
    actions.push(NextAction::call("Read the chunks surrounding the top result for more context", "get_chunk_neighbors", json!({ "id": id })));
}
if top5.len() > 1 {
    actions.push(NextAction::call("Fetch the top 5 ranked chunks' content in one call", "get_chunks", json!({ "ids": top5 })));
}
if let Some(d) = t.get("document_id").and_then(Value::as_str) {
    actions.push(NextAction::call("Get the top result's parent document overview and chunk map", "get_document", json!({ "id": d })));
}
```

Telemetry construction is unchanged (`reranker_used` moves into opts).

- [ ] **Step 5: Dispatch.** In `server.rs`: `"search" | "advanced_search" => return Ok(run_search_dispatch(&params, state).await)`; `run_search_dispatch` picks the parser by `params.name`, computes `skill_installed: mn_skills::installed_anywhere(&<the real SkillEnv impl used by run_install_search_skill — reuse it>)`, and builds `SearchRenderOpts { reranker_used, advanced: params.name == "advanced_search", skill_installed }`. The `rerank_on` telemetry probe at server.rs:304 must treat both names (`params.name == "search" || params.name == "advanced_search"`), with basic search always `true`.

- [ ] **Step 6: Registration.** In `tools::list()` replace the `search` entry's schema/description and add `advanced_search` right after it (descriptions in Task 20's table — paste them now from there). Both use `crate::schemas::search_output_schema()`.

- [ ] **Step 7: Render tests** (update existing + add):
- summary no longer contains `corpus` or the model id (assert `!o.summary.contains("corpus")`).
- nudge fires: envelope with `total_candidates: 2`, opts `skill_installed: false` → summary contains `install_search_skill`, actions contain it; with `skill_installed: true` → absent; with `total_candidates: 50` → absent.
- basic strips `matched_queries` from `structured`, advanced keeps it.
- actions: `get_chunks` single + top-5 + neighbors + `get_document`, each with a non-empty `description`.

- [ ] **Step 8: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp && cargo test -p mn-skills`

```bash
git add crates/mn-mcp crates/mn-skills
git commit -m "feat!(mn-mcp): split search into basic search + advanced_search; low-candidate skill nudge"
```

### Task 12: `get_chunks` (replaces `get_chunk`)

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs` (registration), `src/server.rs` (dispatch), `src/render.rs` (projector)

- [ ] **Step 1: Registration.** Replace the `get_chunk` entry with:

```rust
ToolDescription {
    name: "get_chunks",
    description: "Fetch the full content of one or more chunks by id (up to 20 per call), typically ids returned by search. Use this to read the actual text behind search results.",
    input_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "ids": { "type": "array", "minItems": 1, "maxItems": 20,
                "items": { "type": "string", "format": "uuid" },
                "description": "Chunk ids to fetch. One id is a one-element array." }
        },
        "required": ["ids"],
        "additionalProperties": false
    }),
    output_schema: Some(crate::schemas::chunks_output_schema()),
    annotations: read_only(),  // field exists after Task 20; if doing tasks in order, add the annotations field in this task's commit only if Task 20 already landed — otherwise omit and let Task 20 add it
},
```

- [ ] **Step 2: Dispatch.** Remove `"get_chunk"` from the passthrough match arm; add:

```rust
"get_chunks" => {
    let ids: Vec<String> = match params.arguments.get("ids").and_then(serde_json::Value::as_array) {
        Some(a) if (1..=20).contains(&a.len()) => {
            match a.iter().map(|v| v.as_str().map(str::to_owned).ok_or(())).collect() {
                Ok(ids) => ids,
                Err(()) => {
                    return Ok(err(
                        ToolFailure::simple(ErrorKind::InvalidInput, "ids must be an array of UUID strings", "Pass chunk ids from a recent search result.").into_result(),
                        Outcome::InvalidInput,
                    ))
                }
            }
        }
        _ => {
            return Ok(err(
                ToolFailure::simple(ErrorKind::InvalidInput, "ids must contain 1..=20 UUID strings", "Pass chunk ids from a recent search result.").into_result(),
                Outcome::InvalidInput,
            ))
        }
    };
    match state.cloud.get_chunks(&ids).await {
        Ok(v) => ok(render::project_chunks(v).into_result(), None),
        Err(e) => err(cloud_failure(&e).into_result(), Outcome::Error),
    }
}
```

(Adapt the `ok`/`err`/`return` shape to the actual closure signatures in `dispatch_tool` — mirror the `list_sources` arm.)

- [ ] **Step 3: Projector.** Replace `project_chunk` with:

```rust
/// `get_chunks`: `{ chunks: [ChunkWithContext..], missing: [id..] }`.
/// Single chunk → FULL content in the text fence (legacy text-only clients
/// must receive the payload). Multiple → per-chunk snippets.
pub fn project_chunks(env: Value) -> ToolOutcome {
    let chunks = env.get("chunks").and_then(Value::as_array).cloned().unwrap_or_default();
    let missing: Vec<String> = env
        .get("missing")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let n = chunks.len();
    let missing_note = if missing.is_empty() {
        String::new()
    } else {
        format!(" ({} id(s) not found: {})", missing.len(), missing.join(", "))
    };
    let (summary, trimmed) = if n == 1 {
        let c = &chunks[0];
        let id = c.get("id").and_then(Value::as_str).unwrap_or("?");
        let path = c.pointer("/document/source_path").and_then(Value::as_str).unwrap_or("(unknown)");
        let heading = c.get("heading_path").and_then(Value::as_array)
            .map(|h| h.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" › "))
            .filter(|s| !s.is_empty());
        let where_ = heading.map_or_else(|| path.to_owned(), |h| format!("{path} › {h}"));
        (
            format!("Chunk {id} — {where_}.{missing_note}"),
            json!({
                "id": id,
                "source_path": path,
                "heading_path": c.get("heading_path").cloned().unwrap_or(json!([])),
                "content": c.get("content").cloned().unwrap_or(Value::Null),
            }),
        )
    } else {
        (
            format!("{n} chunks fetched.{missing_note}"),
            json!({ "count": n, "chunks": chunks.iter().map(chunk_brief).collect::<Vec<_>>() }),
        )
    };
    let mut actions = Vec::new();
    if let Some(first) = chunks.first() {
        if let Some(id) = first.get("id").and_then(Value::as_str) {
            actions.push(NextAction::call(
                "Read the chunks surrounding the first fetched chunk",
                "get_chunk_neighbors", json!({ "id": id }),
            ));
        }
        if let Some(d) = first.get("document_id").and_then(Value::as_str) {
            actions.push(NextAction::call(
                "Fetch the first chunk's parent document overview and chunk map",
                "get_document", json!({ "id": d }),
            ));
        }
    }
    ToolOutcome::new(summary, env, trimmed, actions)
}
```

Schemas.rs: add `pub fn chunks_output_schema() -> Value { passthrough_object_schema() }`, delete `chunk_output_schema`.

- [ ] **Step 4: Tests.** Render tests: single → fence contains the full `content` string; multi → `snippet` keys, no full `content`; missing ids surface in summary. Dispatch tests in `tests/tools_dispatch.rs`: `get_chunks` happy path (wiremock `/v1/chunks` returning 2 chunks), 0 ids → `INVALID_INPUT` isError envelope, 21 ids → same; `get_chunk` no longer in `tools/list`.

- [ ] **Step 5: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add crates/mn-mcp && git commit -m "feat!(mn-mcp): get_chunks batch tool replaces get_chunk (full content for 1, snippets for many)"
```

### Task 13: Document tools rework

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`, `src/server.rs`, `src/render.rs`, `src/schemas.rs`

- [ ] **Step 1: Registration.** Delete the old `get_document` AND `get_document_full` entries; add ONE `get_document` (description: "Fetch a document's metadata plus an ordered skeleton of its chunks (ids, positions, token counts — no bodies). Use to size up a document before reading it with get_document_chunks.") with the existing `{id}` input schema and `document_output_schema()`. `get_document_chunks` keeps its schema; new description: "Read a window of a document's chunk bodies by position. Use after get_document to read a document section by section."

- [ ] **Step 2: Dispatch.** The passthrough arm drops `"get_document_full"`. `get_document` continues hitting `GET /v1/documents/:id` via `CloudClient::get_document` (the cloud response now carries the skeleton from Task 6). Delete `run_passthrough_tool`'s full-document branch and the `TooManyChunks` failure construction (already removed in Task 8 — confirm with `grep -rn "TooManyChunks\|TOO_MANY_CHUNKS" crates/mn-mcp/`: zero hits after this step).

- [ ] **Step 3: Projectors.** Delete `project_document_full`. Rewrite `project_document_overview` → `project_document`:

```rust
/// `get_document`: metadata + chunk skeleton (`chunks: [{id, chunk_index, token_count}]`).
pub fn project_document(env: Value) -> ToolOutcome {
    let path = env.get("source_path").and_then(Value::as_str).unwrap_or("(unknown)");
    let name = env.pointer("/source/display_name").and_then(Value::as_str)
        .or_else(|| env.pointer("/source/slug").and_then(Value::as_str)).unwrap_or("");
    let id = env.get("id").and_then(Value::as_str).unwrap_or("?").to_owned();
    let skeleton = env.get("chunks").and_then(Value::as_array).cloned().unwrap_or_default();
    let n = skeleton.len();
    let tokens: i64 = skeleton.iter()
        .filter_map(|c| c.get("token_count").and_then(Value::as_i64)).sum();
    let summary = format!("{path} ({name}): {n} chunks, ~{tokens} tokens.");
    let trimmed = json!({
        "id": id, "source_path": path, "chunk_count": n, "total_tokens": tokens,
        // Skeletons are small; cap the fence at 50 entries (full set in structuredContent).
        "chunks": skeleton.iter().take(50).cloned().collect::<Vec<_>>(),
    });
    let actions = vec![NextAction::call(
        "Read the document's chunk bodies from the beginning",
        "get_document_chunks", json!({ "id": id, "from": 0 }),
    )];
    ToolOutcome::new(summary, env, trimmed, actions)
}
```

Rework `project_document_window`'s trimmed fence to per-chunk snippets and add the overview backlink:

```rust
    // trimmed:
    let briefs: Vec<Value> = env.get("chunks").and_then(Value::as_array)
        .map(|a| a.iter().map(|c| json!({
            "chunk_id": c.get("chunk_id").cloned().unwrap_or(Value::Null),
            "chunk_index": c.get("chunk_index").cloned().unwrap_or(Value::Null),
            "snippet": c.get("content").and_then(Value::as_str).map(snippet),
        })).collect()).unwrap_or_default();
    let trimmed = json!({ "source_path": path, "from": from, "to": to, "total_chunks": total, "chunks": briefs });
    // actions: keep the next-window action (new description: "Read the next
    // window of chunk bodies"), plus:
    actions.push(NextAction::call("Fetch the document overview and full chunk map", "get_document", json!({ "id": id })));
```

- [ ] **Step 4: Schemas + dispatch table.** `schemas.rs`: `document_output_schema` doc comment now covers `get_document` / `get_document_chunks` only. `server.rs` passthrough match: `"get_document"` projector → `project_document`; remove the `project_document_full` arm.

- [ ] **Step 5: Tests.** Update `project_document_overview`/`project_document_full` tests → new shapes: skeleton env (`chunks: [{id, chunk_index, token_count}]`) → summary contains token total; fence capped at 50 (feed 60 skeletons, assert 50 in trimmed, 60 in structured); window fence has snippets; tools_dispatch: `get_document_full` absent from tools/list, calling it → `unknown tool` JSON-RPC error.

- [ ] **Step 6: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add crates/mn-mcp && git commit -m "feat!(mn-mcp): merged get_document (metadata + skeleton); windowed chunks carry snippets"
```

### Task 14: `get_chunk_parents` projector

**Files:**
- Modify: `crates/mn-mcp/src/render.rs`

- [ ] **Step 1: Rewrite `project_parents`** (cloud now sends `{parents, source}` per Task 5 — no wrapping needed):

```rust
/// `get_chunk_parents`: `{ parents: [ParentNode..], source: {slug, display_name} }`,
/// ordered immediate parent → root.
pub fn project_parents(env: Value) -> ToolOutcome {
    let parents = env.get("parents").and_then(Value::as_array).cloned().unwrap_or_default();
    let n = parents.len();
    let source_name = env.pointer("/source/display_name").and_then(Value::as_str)
        .or_else(|| env.pointer("/source/slug").and_then(Value::as_str)).unwrap_or("(unknown)");
    let mut lines = Vec::with_capacity(n);
    for p in &parents {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let kind = p.get("kind").and_then(Value::as_str).unwrap_or("?");
        let id = p.get("id").and_then(Value::as_str).unwrap_or("?");
        lines.push(format!("  {name} ({kind}) — {id}"));
    }
    let summary = format!("{n} ancestor(s), root last — source: {source_name}\n{}", lines.join("\n"));
    let trimmed = json!({
        "count": n,
        "source": env.get("source").cloned().unwrap_or(Value::Null),
        "parents": parents.iter().map(|p| json!({
            "id": p.get("id").cloned().unwrap_or(Value::Null),
            "name": p.get("name").cloned().unwrap_or(Value::Null),
            "kind": p.get("kind").cloned().unwrap_or(Value::Null),
            "document_id": p.get("document_id").cloned().unwrap_or(Value::Null),
        })).collect::<Vec<_>>(),
    });
    // Only the document-kind node maps to a fetchable document.
    let actions = parents.iter()
        .find(|p| p.get("kind").and_then(Value::as_str) == Some("document"))
        .and_then(|p| p.get("document_id").and_then(Value::as_str))
        .map(|d| vec![NextAction::call(
            "Fetch the containing document's overview and chunk map",
            "get_document", json!({ "id": d }),
        )])
        .unwrap_or_default();
    ToolOutcome::new(summary, env, trimmed, actions)
}
```

Description (registration): "Show where a chunk sits in its source's structure: the chain of containing nodes (document, folders) up to the source root. Use to orient a chunk within its source and find its containing document."

- [ ] **Step 2: Tests.** Env with `[document node (document_id set), group, root]` + source → summary lists `name (kind) — id` per line + source display name; action targets the document_id; env with no document node → no actions.

- [ ] **Step 3: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add crates/mn-mcp && git commit -m "feat(mn-mcp): parents response names each node, carries source, links containing document"
```

### Task 15: `list_sources` tool — pagination + filters

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`, `src/server.rs`, `src/render.rs`

- [ ] **Step 1: Input schema** (registration; description: "List the sources that make up the corpus (paginated). Use to discover what material exists and to get source slugs for advanced_search filters."):

```rust
input_schema: serde_json::json!({
    "type": "object",
    "properties": {
        "cursor": { "type": "string", "description": "Opaque pagination token from a previous response's next_cursor." },
        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
        "created_after": { "type": "string", "format": "date-time", "description": "Only sources registered after this RFC3339 instant." },
        "created_before": { "type": "string", "format": "date-time", "description": "Only sources registered before this RFC3339 instant." },
        "kind": { "type": "string", "enum": ["docs_site", "code_repo", "standalone", "mixed"] },
        "retired": { "type": "boolean", "default": false, "description": "Include retired sources." }
    },
    "additionalProperties": false
}),
```

- [ ] **Step 2: Dispatch** builds the param list (only present keys) and calls `state.cloud.list_sources(&params_vec)`:

```rust
"list_sources" => {
    let mut p: Vec<(&str, String)> = Vec::new();
    for key in ["cursor", "limit", "created_after", "created_before", "kind", "retired"] {
        if let Some(v) = params.arguments.get(key) {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            p.push((key, s));
        }
    }
    match state.cloud.list_sources(&p).await {
        Ok(v) => ok(render::project_sources(v).into_result(), None),
        Err(e) => err(cloud_failure(&e).into_result(), Outcome::Error),
    }
}
```

- [ ] **Step 3: Projector** — env is now `{sources, total, next_cursor}`:

```rust
pub fn project_sources(env: Value) -> ToolOutcome {
    let sources = env.get("sources").and_then(Value::as_array).cloned().unwrap_or_default();
    let n = sources.len();
    let total = env.get("total").and_then(Value::as_i64).unwrap_or_else(|| n as i64);
    let next_cursor = env.get("next_cursor").and_then(Value::as_str).map(str::to_owned);
    let more = if next_cursor.is_some() { " More available — pass cursor." } else { "" };
    let summary = format!("Showing {n} of {total} sources.{more}");
    let brief: Vec<Value> = sources.iter().map(|s| json!({
        "id": s.get("id").cloned().unwrap_or(Value::Null),
        "display_name": s.get("display_name").cloned().unwrap_or(Value::Null),
        "kind": s.get("kind").cloned().unwrap_or(Value::Null),
        "slug": s.get("slug").cloned().unwrap_or(Value::Null),
    })).collect();
    let trimmed = json!({ "count": n, "total": total, "sources": brief });
    let mut actions = Vec::new();
    if let Some(c) = next_cursor {
        actions.push(NextAction::call("Fetch the next page of sources", "list_sources", json!({ "cursor": c })));
    }
    if let Some(slug) = sources.first().and_then(|s| s.get("slug")).and_then(Value::as_str) {
        actions.push(NextAction::call(
            format!("Restrict a search to the `{slug}` source (swap in your own query and slug)"),
            "advanced_search",
            json!({ "queries": ["<your query>"], "filters": { "source_slug": { "any_of": [slug] } } }),
        ));
    }
    ToolOutcome::new(summary, env, trimmed, actions)
}
```

(`slug` rides along in the brief — agents need it for the filter even though the design's minimum was id/display_name/kind. Confirm the filter key is `source_slug` against `mn_retrieval::facets::facets()` — `grep -n "source_slug" crates/mn-retrieval/src/facets.rs`.)

- [ ] **Step 4: Tests.** Projector: paged env → "Showing 2 of 43", next-page + concrete-filter actions (filter example uses the real first slug, not a placeholder slug); last page → no cursor action. Dispatch (wiremock): `list_sources {limit: 5, kind: "docs_site"}` → request URL contains both query params.

- [ ] **Step 5: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add crates/mn-mcp && git commit -m "feat(mn-mcp): list_sources pagination/filters + concrete filter example in suggested actions"
```

### Task 16: `facets` tool — drill-down

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`, `src/server.rs`, `src/render.rs`, `src/schemas.rs`

- [ ] **Step 1: Input schema** (description: "Discover the filter dimensions available to advanced_search and the values present in the corpus. Call without arguments for an overview; pass a facet name to page through all values of one dimension."):

```rust
input_schema: serde_json::json!({
    "type": "object",
    "properties": {
        "facet": { "type": "string", "enum": ["source_slug", "language", "tags", "package"],
            "description": "Drill into one open-set facet's full value list. Omit for the overview." },
        "cursor": { "type": "string", "description": "Opaque token from a previous drill-down response." },
        "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
    },
    "additionalProperties": false
}),
```

- [ ] **Step 2: Dispatch** mirrors Task 15 Step 2 (param keys `facet`/`cursor`/`limit` → `state.cloud.get_facets(&p)`), routing the result through `render::project_facets`.

- [ ] **Step 3: Projector** handles both shapes (drill-down has a top-level `"facet"` key):

```rust
pub fn project_facets(env: Value) -> ToolOutcome {
    if let Some(facet) = env.get("facet").and_then(Value::as_str).map(str::to_owned) {
        // Drill-down page.
        let values = env.get("values").and_then(Value::as_array).cloned().unwrap_or_default();
        let n = values.len();
        let total = env.get("total").and_then(Value::as_i64).unwrap_or(n as i64);
        let next_cursor = env.get("next_cursor").and_then(Value::as_str).map(str::to_owned);
        let summary = format!("{facet}: showing {n} of {total} values.");
        let trimmed = json!({ "facet": facet, "values": values, "total": total });
        let mut actions = Vec::new();
        if let Some(c) = next_cursor {
            actions.push(NextAction::call(
                format!("Fetch the next page of `{facet}` values"),
                "facets", json!({ "facet": facet, "cursor": c }),
            ));
        }
        if let Some(v) = values.first().and_then(Value::as_str) {
            actions.push(NextAction::call(
                format!("Search filtered to {facet}=`{v}` (swap in your own query and value)"),
                "advanced_search",
                json!({ "queries": ["<your query>"], "filters": { facet: { "any_of": [v] } } }),
            ));
        }
        return ToolOutcome::new(summary, env, trimmed, actions);
    }
    // Overview (existing shape: { modes, filters: [{key, type, values?, total?, truncated?}] }).
    let dims = env.get("filters").and_then(Value::as_array).cloned().unwrap_or_default();
    let keys: Vec<String> = dims.iter()
        .filter_map(|f| f.get("key").and_then(Value::as_str).map(str::to_owned)).collect();
    let summary = format!(
        "{} filter dimensions for advanced_search: {}. Open-set dimensions show samples — drill in with facets({{facet}}).",
        keys.len(), keys.join(", ")
    );
    let trimmed = json!({ "dimensions": dims.iter().map(|f| json!({
        "key": f.get("key").cloned().unwrap_or(Value::Null),
        "type": f.get("type").cloned().unwrap_or(Value::Null),
        "values": f.get("values").cloned().unwrap_or(Value::Null),
        "total": f.get("total").cloned().unwrap_or(Value::Null),
    })).collect::<Vec<_>>() });
    // Concrete example from real corpus data: first closed-enum value if any.
    let mut actions = Vec::new();
    if let Some((key, v)) = dims.iter().find_map(|f| {
        let key = f.get("key").and_then(Value::as_str)?;
        let v = f.get("values").and_then(Value::as_array)?.first()?.as_str()?;
        Some((key.to_owned(), v.to_owned()))
    }) {
        actions.push(NextAction::call(
            format!("Search filtered to {key}=`{v}` (swap in your own query and value)"),
            "advanced_search",
            json!({ "queries": ["<your query>"], "filters": { key: { "any_of": [v] } } }),
        ));
    }
    actions.push(NextAction::call(
        "Page through every value of an open-set facet (e.g. tags)",
        "facets", json!({ "facet": "tags" }),
    ));
    ToolOutcome::new(summary, env, trimmed, actions)
}
```

- [ ] **Step 4: Tests.** Overview env → summary names dimensions + example action uses a REAL value from the env; drill env (`{facet:"tags", values:[..], total, next_cursor}`) → "tags: showing X of Y", next-page action present.

- [ ] **Step 5: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add crates/mn-mcp && git commit -m "feat(mn-mcp): facets drill-down + concrete filter examples in suggested actions"
```

### Task 17: Remove `pull_models`

**Files:**
- Modify: `crates/mn-mcp/src/tools.rs`, `src/server.rs`, `src/render.rs`, `src/schemas.rs`

- [ ] **Step 1: Delete:** the `pull_models` registration entry; the `"pull_models"` dispatch arm; `run_pull_models` + `PullModelsOutput` (tools.rs:505-542 region) — keep `reranker::global` / `LOADED_MARKERS` (search's lazy load uses them); `project_pull_models`; `pull_models_output_schema`; the `pull_models` next-action in `project_status` (rewritten in Task 18 anyway). `tool_name_for_event`: drop the `"pull_models"` arm; keep `McpToolName::PullModels` in mn-telemetry (historical rows decode it) with a doc comment `/// `pull_models` tool (removed in v2; retained for historical event rows).`

- [ ] **Step 2: Sweep:** `grep -rn "pull_models\|PullModels" crates/ specs/` — remaining legitimate hits: mn-telemetry enum variant + its tests, mcp-tools.json (Task 24), SKILL.md assets (Task 22), mn-cli `commands/models.rs` (`mnm models pull` — KEEP, that's the CLI's own model command, unrelated to the MCP tool; verify it doesn't call mn-mcp's deleted fn — if it does, inline the old `reranker::global` call there).

- [ ] **Step 3: Tests:** drop pull_models dispatch/render tests; add a tools_dispatch assertion that `tools/list` has no `pull_models` and calling it returns the `unknown tool` JSON-RPC error.

- [ ] **Step 4: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp && cargo test -p mn-cli`

```bash
git add crates/ && git commit -m "feat!(mn-mcp): remove pull_models (lazy load + status cover it)"
```

### Task 18: `status` rework — shared assembler

**Files:**
- Create: `crates/mn-mcp/src/status.rs`
- Modify: `crates/mn-mcp/src/lib.rs` (`pub mod status;`), `src/tools.rs` (delete `run_status`/`StatusOutput`), `src/server.rs`, `src/render.rs`

- [ ] **Step 1: The assembler** (`crates/mn-mcp/src/status.rs`):

```rust
//! Shared status assembly for the MCP `status` tool and `mnm status`.
//! Probes run concurrently with a 3s budget each; a failed probe degrades
//! that section, never the whole report.

use std::time::Duration;

use serde::Serialize;

use crate::cloud_client::CloudClient;

/// Cloud reachability.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudState {
    /// `/readyz` returned 200.
    Reachable,
    /// `/readyz` returned non-200 (server up, dependencies not ready).
    Degraded,
    /// Transport failure.
    Unreachable,
}

/// VoyageAI key state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoyageState {
    /// Key present and accepted by the Voyage API.
    Valid,
    /// Key present but rejected (401/403).
    InvalidKey,
    /// Key present but the probe failed (network/timeout/5xx).
    Unreachable,
    /// No key configured — embedding goes through the server proxy.
    NotConfigured,
}

/// Full status report. One struct, two renderers (MCP projector, CLI).
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    /// This binary's mn-mcp version.
    pub mcp_version: &'static str,
    /// Cloud reachability.
    pub cloud: CloudState,
    /// Cloud server version (from `/v1/me`), when reachable.
    pub cloud_version: Option<String>,
    /// `true` when a bearer was presented and accepted.
    pub authenticated: bool,
    /// `anonymous` / `github_oauth` / `admin` (from `/v1/me`).
    pub auth_type: String,
    /// Identity string (GitHub login or admin user id), when authenticated.
    pub identity: Option<String>,
    /// `read` / `write` / `admin`.
    pub permission_level: String,
    /// Request rate-limit bucket state (from `/v1/me`), when reachable.
    pub rate_limit: Option<serde_json::Value>,
    /// Embedding token-budget windows (from `/v1/me`): `{tier, hourly, daily}`.
    pub token_limits: Option<serde_json::Value>,
    /// Voyage key state.
    pub voyage: VoyageState,
    /// Local reranker model name.
    pub reranker: &'static str,
    /// Whether the local reranker is loaded into memory.
    pub reranker_loaded: bool,
}

/// Probe Voyage with the given key. `GET /v1/files` is a cheap authenticated
/// endpoint: 200 → valid, 401/403 → invalid key, anything else → unreachable.
async fn probe_voyage(key: &str) -> VoyageState {
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(3)).build() {
        Ok(c) => c,
        Err(_) => return VoyageState::Unreachable,
    };
    match client
        .get("https://api.voyageai.com/v1/files")
        .bearer_auth(key)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => VoyageState::Valid,
        Ok(r) if r.status() == 401 || r.status() == 403 => VoyageState::InvalidKey,
        _ => VoyageState::Unreachable,
    }
}

/// Assemble the report. `voyage_key` is the resolved BYOK key (None = proxy mode).
pub async fn assemble(cloud: &CloudClient, voyage_key: Option<&str>) -> StatusReport {
    let readyz = tokio::time::timeout(Duration::from_secs(3), cloud.readyz());
    let me = tokio::time::timeout(Duration::from_secs(3), cloud.get_me());
    let voyage = async {
        match voyage_key {
            None => VoyageState::NotConfigured,
            Some(k) => probe_voyage(k).await,
        }
    };
    let (readyz, me, voyage) = tokio::join!(readyz, me, voyage);

    let cloud_state = match readyz {
        Ok(Ok(200)) => CloudState::Reachable,
        Ok(Ok(_)) => CloudState::Degraded,
        _ => CloudState::Unreachable,
    };
    let me = me.ok().and_then(Result::ok);
    let str_of = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(serde_json::Value::as_str).map(str::to_owned)
    };
    StatusReport {
        mcp_version: crate::VERSION,
        cloud: cloud_state,
        cloud_version: me.as_ref().and_then(|m| str_of(m, "server_version")),
        authenticated: me
            .as_ref()
            .and_then(|m| m.get("authenticated").and_then(serde_json::Value::as_bool))
            .unwrap_or(false),
        auth_type: me
            .as_ref()
            .and_then(|m| str_of(m, "auth_type"))
            .unwrap_or_else(|| "anonymous".to_owned()),
        identity: me.as_ref().and_then(|m| str_of(m, "identity")),
        permission_level: me
            .as_ref()
            .and_then(|m| str_of(m, "permission_level"))
            .unwrap_or_else(|| "read".to_owned()),
        rate_limit: me.as_ref().and_then(|m| m.get("rate_limit").cloned()).filter(|v| !v.is_null()),
        token_limits: me
            .as_ref()
            .and_then(|m| m.get("token_limits").cloned())
            .filter(|v| !v.is_null()),
        voyage,
        reranker: mn_embedding::RERANKER_MODEL_NAME,
        reranker_loaded: crate::tools::reranker_loaded(),
    }
}
```

(`reranker_loaded` must be `pub(crate)`-visible from status.rs — adjust its visibility in tools.rs. If the configurable-reranker selection helper from the Voyage workstream lands a stable resolver before this task, report its id instead of `RERANKER_MODEL_NAME`; otherwise this matches today's behavior.)

- [ ] **Step 2: Dispatch.** Replace the `"status"` arm:

```rust
"status" => {
    let voyage_key = {
        let cfg_env = mn_core::config::StdEnv;
        let (core_cfg, _) = mn_core::config::Config::discover(None, &cfg_env).unwrap_or_default();
        mn_core::config::resolve_voyage_api_key(None, &core_cfg.models, &cfg_env)
    };
    let report = crate::status::assemble(&state.cloud, voyage_key.as_deref()).await;
    let v = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    ok(render::project_status(v).into_result(), None)
}
```

(`resolve_voyage_api_key` signature: mirror the exact call already made in `run_search` at tools.rs:601.)

- [ ] **Step 3: Projector** — rewrite `project_status`:

```rust
/// `status` (StatusReport as JSON).
pub fn project_status(env: Value) -> ToolOutcome {
    let s = |k: &str| env.get(k).and_then(Value::as_str).unwrap_or("?").to_owned();
    let cloud = s("cloud");
    let cloud_ver = env.get("cloud_version").and_then(Value::as_str)
        .map(|v| format!(" (v{v})")).unwrap_or_default();
    let auth = if env.get("authenticated").and_then(Value::as_bool).unwrap_or(false) {
        format!("{} {} ({})", s("auth_type"),
            env.get("identity").and_then(Value::as_str).unwrap_or("?"), s("permission_level"))
    } else {
        "anonymous (read)".to_owned()
    };
    let rl = env.get("rate_limit").filter(|v| !v.is_null()).map(|r| format!(
        "; requests {}/{}",
        r.get("remaining").and_then(Value::as_u64).unwrap_or(0),
        r.get("limit").and_then(Value::as_u64).unwrap_or(0),
    )).unwrap_or_default();
    let tl = env.get("token_limits").filter(|v| !v.is_null()).map(|t| {
        let w = |k: &str, unit: &str| {
            format!(
                "{}/{} {unit}",
                t.pointer(&format!("/{k}/remaining")).and_then(Value::as_u64).unwrap_or(0),
                t.pointer(&format!("/{k}/limit")).and_then(Value::as_u64).unwrap_or(0),
            )
        };
        format!("; embed tokens {} · {}", w("hourly", "hr"), w("daily", "day"))
    }).unwrap_or_default();
    let reranker = format!("{} {}", s("reranker"),
        if env.get("reranker_loaded").and_then(Value::as_bool).unwrap_or(false) { "loaded" } else { "not loaded (loads on first reranked search)" });
    let summary = format!(
        "Cloud {cloud}{cloud_ver}; auth: {auth}{rl}{tl}; Voyage key {}; reranker {reranker}.",
        s("voyage").replace('_', " "),
    );
    let trimmed = json!({
        "cloud": env.get("cloud").cloned().unwrap_or(Value::Null),
        "authenticated": env.get("authenticated").cloned().unwrap_or(Value::Null),
        "auth_type": env.get("auth_type").cloned().unwrap_or(Value::Null),
        "voyage": env.get("voyage").cloned().unwrap_or(Value::Null),
        "rate_limit": env.get("rate_limit").cloned().unwrap_or(Value::Null),
        "token_limits": env.get("token_limits").cloned().unwrap_or(Value::Null),
    });
    let mut actions = Vec::new();
    if s("voyage") == "invalid_key" {
        actions.push(NextAction::user("Ask the user to check their VOYAGE_API_KEY — the Voyage API rejected it"));
    }
    if !env.get("authenticated").and_then(Value::as_bool).unwrap_or(false) {
        actions.push(NextAction::user("For higher rate limits, ask the user to run `mnm auth github`"));
    }
    ToolOutcome::new(summary, env, trimmed, actions)
}
```

Registration description: "Diagnose the retrieval setup: cloud reachability, authentication and rate-limit state, VoyageAI key validity, and reranker readiness. Call when searches fail, return errors, or before starting a long session."

- [ ] **Step 4: Tests.** Assembler unit tests with wiremock standing in for the cloud (readyz 200 + `/v1/me` body → Reachable + identity populated; readyz refused → Unreachable and report still assembles). Voyage probe NOT exercised against the real API: pass `voyage_key: None` → `NotConfigured` (probe_voyage stays untested-live; its match arms are covered by construction). Projector tests: authenticated env renders identity + rate limit; `invalid_key` env yields the user action.

- [ ] **Step 5: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add crates/mn-mcp && git commit -m "feat(mn-mcp): status reworked — cloud health, /v1/me auth + rate limits, Voyage key probe"
```

### Task 19: `install_search_skill` — detected list + reload actions

**Files:**
- Modify: `crates/mn-skills/src/install.rs`, `crates/mn-mcp/src/render.rs`

- [ ] **Step 1: `detected` field.** `InstallReport` gains `pub detected: Vec<String>` (doc: "Harness ids written to (forced or auto-detected)."). In `install()`: `detected: targets_ids` where `let targets_ids: Vec<String> = targets.iter().map(|h| h.id().to_owned()).collect();` computed before the loop (the loop consumes `targets` — clone ids first). Update mn-skills tests asserting the report shape.

- [ ] **Step 2: Projector** — rewrite `project_install`:

```rust
/// `install_search_skill` (InstallReport as JSON).
pub fn project_install(env: Value) -> ToolOutcome {
    let scope = env.get("scope").and_then(Value::as_str).unwrap_or("user");
    let skill = env.get("skill_name").and_then(Value::as_str).unwrap_or("midnight-advanced-search");
    let empty = vec![];
    let installed = env.get("installed").and_then(Value::as_array).unwrap_or(&empty);
    let names: Vec<&str> = installed.iter()
        .filter_map(|i| i.get("harness").and_then(Value::as_str)).collect();
    let summary = format!(
        "Installed/updated `{skill}` for {} (scope: {scope}). The skill is NOT active yet — \
         ask the user to restart their session or refresh their skills, then it will load automatically.",
        if names.is_empty() { "no harnesses".to_owned() } else { names.join(", ") },
    );
    let trimmed = json!({
        "skill_name": skill, "scope": scope,
        "detected": env.get("detected").cloned().unwrap_or(json!([])),
        "not_detected": env.get("not_detected").cloned().unwrap_or(json!([])),
        "actions": installed.iter().map(|i| json!({
            "harness": i.get("harness").cloned().unwrap_or(Value::Null),
            "action": i.get("action").cloned().unwrap_or(Value::Null),
        })).collect::<Vec<_>>(),
    });
    let actions = installed.iter().filter_map(|i| {
        let h = i.get("harness").and_then(Value::as_str)?;
        let step = i.get("reload_step").and_then(Value::as_str)?;
        Some(NextAction::user(format!("[{h}] Ask the user to: {step}")))
    }).collect();
    ToolOutcome::new(summary, env, trimmed, actions)
}
```

Registration description: "Install (or update) the midnight-advanced-search skill — a retrieval playbook teaching effective corpus search patterns — into the user's AI harness(es). Use when search results are poor or the user asks for better search guidance."

- [ ] **Step 3: Tests.** mn-skills: report has `detected == ["claude-code"]` when forced to claude-code. Render: summary contains the refresh instruction; one user-action per installed harness carrying its `reload_step`; trimmed has `detected` + `not_detected`.

- [ ] **Step 4: Verify + commit**

Run: `cargo test -p mn-skills && VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add crates/mn-skills crates/mn-mcp && git commit -m "feat(mn-mcp): install report gains detected list; reload steps become user actions"
```

### Task 20: Annotations + remaining descriptions + tools/list test

**Files:**
- Modify: `crates/mn-mcp/src/protocol.rs`, `src/tools.rs`, `tests/tools_dispatch.rs`

- [ ] **Step 1: Annotation types** (`protocol.rs`, near `ToolDescription`):

```rust
/// MCP tool annotations (behavior hints for clients).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ToolAnnotations {
    /// Tool does not modify its environment.
    #[serde(rename = "readOnlyHint")]
    pub read_only_hint: bool,
    /// Tool may perform destructive updates (only meaningful when not read-only).
    #[serde(rename = "destructiveHint", skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// Repeated identical calls have no additional effect.
    #[serde(rename = "idempotentHint", skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// Tool interacts with an open world of external entities.
    #[serde(rename = "openWorldHint")]
    pub open_world_hint: bool,
}

impl ToolAnnotations {
    /// Read-only, closed-world (every corpus/read tool).
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            read_only_hint: true,
            destructive_hint: None,
            idempotent_hint: None,
            open_world_hint: false,
        }
    }

    /// Local writer that only touches its own files, safely re-runnable.
    #[must_use]
    pub const fn idempotent_writer() -> Self {
        Self {
            read_only_hint: false,
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: false,
        }
    }
}
```

`ToolDescription` gains `pub annotations: ToolAnnotations` with `#[serde(rename = "annotations")]`.

- [ ] **Step 2: Apply.** Every entry in `tools::list()` gets `annotations: ToolAnnotations::read_only()` except `install_search_skill` → `ToolAnnotations::idempotent_writer()`. While here, set the final descriptions for the tools not already rewritten in Tasks 11–19:
- `get_chunk_next`: "Fetch chunks that immediately follow a given chunk in its document's reading order. Use to continue reading past the end of a chunk you already have."
- `get_chunk_prev`: "Fetch chunks that immediately precede a given chunk in its document's reading order. Use to read the context leading up to a chunk you already have."
- `get_chunk_neighbors`: "Fetch the chunks immediately before and after a given chunk in one call. Use when a search hit needs surrounding context to be understood."

Move every cap/default sentence still living in a description into the corresponding input-schema property `description` (the `count` properties already carry ranges — verify, then delete the duplicated prose from descriptions). **Audit rule: no description may mention a repo file path, an FR/D number, or enumerate response fields.** `grep -n "docs/\|D2[0-9]\|FR-" crates/mn-mcp/src/tools.rs` must return no hits inside description strings.

- [ ] **Step 3: tools/list test** (`tests/tools_dispatch.rs`):

```rust
#[test]
fn tools_list_has_13_tools_with_annotations() {
    let list = mn_mcp::tools::list();
    let names: Vec<&str> = list.tools.iter().map(|t| t.name).collect();
    assert_eq!(
        names,
        [
            "search", "advanced_search", "get_chunks", "get_chunk_next", "get_chunk_prev",
            "get_chunk_neighbors", "get_chunk_parents", "get_document", "get_document_chunks",
            "list_sources", "facets", "status", "install_search_skill",
        ]
    );
    for t in &list.tools {
        let v = serde_json::to_value(t).unwrap();
        assert!(v["annotations"]["readOnlyHint"].is_boolean(), "{} missing annotations", t.name);
        assert!(!t.description.contains("docs/"), "{} description references a repo path", t.name);
    }
    let install = list.tools.iter().find(|t| t.name == "install_search_skill").unwrap();
    let v = serde_json::to_value(install).unwrap();
    assert_eq!(v["annotations"]["readOnlyHint"], false);
    assert_eq!(v["annotations"]["idempotentHint"], true);
    assert_eq!(v["annotations"]["destructiveHint"], false);
}
```

(Adjust the registration order in `tools::list()` to match this canonical order.)

- [ ] **Step 4: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add crates/mn-mcp && git commit -m "feat(mn-mcp): tool annotations + final what/when descriptions across all 13 tools"
```

### Task 21: Telemetry names

**Files:**
- Modify: `crates/mn-telemetry/src/events.rs`, `crates/mn-mcp/src/server.rs`, `crates/mn-telemetry/tests/canary_suite.rs`

- [ ] **Step 1:** Add to `McpToolName`: `AdvancedSearch` (doc: `/// `advanced_search` tool.`) and `GetChunks` (`/// `get_chunks` tool.`). Keep `GetChunk`, `GetDocumentFull`, `PullModels` with `(removed in v2; retained for historical rows)` doc suffixes.

- [ ] **Step 2:** `tool_name_for_event` (server.rs): `"advanced_search" => Some(McpToolName::AdvancedSearch)`, `"get_chunks" => Some(McpToolName::GetChunks)`; remove the `"get_chunk"`, `"get_document_full"`, `"pull_models"` arms. Add `CliCommandName::Status` (`/// `mnm status``) to the CLI enum while in the file (used by Task 23).

- [ ] **Step 3:** Canary: extend the existing serialization canary (the test asserting `McpToolCall` emits only closed-vocabulary values) with events carrying the two new variants — assert their snake_case wire forms `"advanced_search"` / `"get_chunks"` and that no free-text fields appear (mirror the existing canary test bodies in `canary_suite.rs`).

- [ ] **Step 4: Verify + commit**

Run: `cargo test -p mn-telemetry && VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add crates/mn-telemetry crates/mn-mcp && git commit -m "feat(mn-telemetry): tool-name variants for advanced_search/get_chunks + mnm status"
```

---

## Phase 4 — Skill content, CLI, contracts, gate

### Task 22: Rewrite the bundled search skill

**Files:**
- Modify: `crates/mn-skills/assets/midnight-advanced-search/SKILL.md`, `references/filters-and-modes.md`, `references/advanced-techniques.md`, mn-skills frontmatter tests if any assert content

- [ ] **Step 1: Systematic rewrite.** Open all three files and apply, everywhere (the explorer located hits at SKILL.md lines 7, 26, 31, 33, 36, 38, 44, 50, 56, 58, 79, 90, 94, 96, 118, 121, 124, 128 — re-grep, the references files have more):
1. Every multi-query / filters / rerank / mode-tuning pattern now calls **`advanced_search`** with a required `queries` array (1–10 strings) — rewrite each example invocation accordingly. Plain quick lookups use **`search`** `{query, mode?, limit?}`.
2. `get_chunk` → `get_chunks` with `ids: [..]` (update every example; mention the 20-id cap and that search results feed it directly).
3. `get_document_full` → `get_document` (metadata + chunk skeleton; bodies come from `get_document_chunks`). Remove any 500-chunk-cap / TOO_MANY_CHUNKS recovery advice.
4. Remove every `pull_models` mention (model loads lazily; `status` reports readiness).
5. `list_sources`: document cursor pagination + `created_after`/`created_before`/`kind`/`retired` filters. `facets`: document overview vs `{facet, cursor}` drill-down.
6. Tool results: mention `suggested_next_actions` (descriptions + optional tool) as the follow-up protocol.

- [ ] **Step 2: Verification grep.** `grep -rn "get_chunk\b\|get_document_full\|pull_models\|chunk_ids" crates/mn-skills/assets/` → zero hits (`get_chunks`/`get_chunk_next` etc. are fine — the `\b` guards the bare name).

- [ ] **Step 3: Verify + commit**

Run: `cargo test -p mn-skills` (frontmatter/content tests)

```bash
git add crates/mn-skills && git commit -m "docs(mn-skills): rewrite advanced-search skill for the v2 tool surface"
```

### Task 23: `mnm status` CLI command

**Files:**
- Create: `crates/mn-cli/src/commands/status.rs`
- Modify: `crates/mn-cli/src/commands/mod.rs` (+`pub mod status;`), `src/cli.rs`, `src/shared.rs`, `src/commands/mcp.rs`, `src/commands/doctor.rs`

- [ ] **Step 1: Move the bearer helper.** Relocate `resolve_read_uplift_token()` from `commands/mcp.rs:70-77` to `shared.rs` as `pub fn resolve_read_uplift_token() -> Option<String>` (same body); `mcp.rs` calls `crate::shared::resolve_read_uplift_token()`.

- [ ] **Step 2: The command** (`commands/status.rs`):

```rust
//! `mnm status` — quick "can I search and who am I" check. Renders the same
//! `StatusReport` the MCP `status` tool returns.

use anyhow::Result;
use clap::Args as ClapArgs;
use mn_mcp::cloud_client::CloudClient;
use mn_mcp::status::{assemble, CloudState, StatusReport, VoyageState};

/// Arguments for `mnm status` (none beyond the globals).
#[derive(Debug, ClapArgs)]
pub struct Args {}

/// Run `mnm status`.
///
/// # Errors
///
/// Returns an error (non-zero exit) when the cloud is unreachable, so the
/// command is scriptable as a health probe.
pub async fn run(_args: Args, server: Option<&str>, json: bool) -> Result<()> {
    let url = crate::shared::resolve_server_url(server);
    let bearer = crate::shared::resolve_read_uplift_token();
    let cloud = CloudClient::new(&url, bearer)
        .map_err(|e| anyhow::anyhow!("cloud client init failed: {e}"))?;
    let voyage_key = {
        let cfg_env = mn_core::config::StdEnv;
        let (core_cfg, _) = mn_core::config::Config::discover(None, &cfg_env).unwrap_or_default();
        mn_core::config::resolve_voyage_api_key(None, &core_cfg.models, &cfg_env)
    };
    let report = assemble(&cloud, voyage_key.as_deref()).await;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report, &url);
    }
    if matches!(report.cloud, CloudState::Unreachable) {
        anyhow::bail!("cloud server unreachable at {url}");
    }
    Ok(())
}

fn print_human(r: &StatusReport, url: &str) {
    println!("mnm status");
    println!("  cloud:        {} ({url})", match r.cloud {
        CloudState::Reachable => "reachable",
        CloudState::Degraded => "degraded",
        CloudState::Unreachable => "UNREACHABLE",
    });
    if let Some(v) = &r.cloud_version {
        println!("  server:       v{v}");
    }
    if r.authenticated {
        println!(
            "  auth:         {} as {} ({})",
            r.auth_type,
            r.identity.as_deref().unwrap_or("?"),
            r.permission_level,
        );
    } else {
        println!("  auth:         anonymous (read) — run `mnm auth github` for higher limits");
    }
    if let Some(rl) = &r.rate_limit {
        println!(
            "  requests:     {}/{} remaining ({} tier, resets in {}s)",
            rl.get("remaining").and_then(serde_json::Value::as_u64).unwrap_or(0),
            rl.get("limit").and_then(serde_json::Value::as_u64).unwrap_or(0),
            rl.get("tier").and_then(serde_json::Value::as_str).unwrap_or("?"),
            rl.get("reset_secs").and_then(serde_json::Value::as_u64).unwrap_or(0),
        );
    }
    if let Some(tl) = &r.token_limits {
        let w = |k: &str| {
            (
                tl.pointer(&format!("/{k}/remaining")).and_then(serde_json::Value::as_u64).unwrap_or(0),
                tl.pointer(&format!("/{k}/limit")).and_then(serde_json::Value::as_u64).unwrap_or(0),
            )
        };
        let (hr_rem, hr_lim) = w("hourly");
        let (day_rem, day_lim) = w("daily");
        println!("  embed tokens: {hr_rem}/{hr_lim} this hour, {day_rem}/{day_lim} today");
    }
    println!("  voyage key:   {}", match r.voyage {
        VoyageState::Valid => "valid",
        VoyageState::InvalidKey => "INVALID — check VOYAGE_API_KEY",
        VoyageState::Unreachable => "unreachable (could not verify)",
        VoyageState::NotConfigured => "not configured (server-proxy embedding)",
    });
    println!(
        "  reranker:     {} ({})",
        r.reranker,
        if r.reranker_loaded { "loaded" } else { "loads on first reranked search" },
    );
}
```

- [ ] **Step 3: Wire it.** `cli.rs`: add `/// Connectivity, auth, and model readiness check.` `Status(commands::status::Args),` to `Command` (place after `Doctor`); dispatch `Command::Status(args) => commands::status::run(args, cli.server.as_deref(), cli.json).await,`; telemetry map `Command::Status(_) => CliCommandName::Status,`. `commands/mod.rs`: `pub mod status;`. `doctor.rs` `print_human`: after the version line add `println!("  (for connectivity/auth checks, run `mnm status`)");`.

- [ ] **Step 4: Test** (wiremock, in `crates/mn-cli/tests/` following `sources_admin_integration.rs` style): stand up a mock serving `/readyz` → 200 and `/v1/me` → an authenticated body; call `mn_mcp::status::assemble` against it via a `CloudClient` pointed at the mock and assert the report fields (this re-verifies the shared path from the CLI crate's perspective); plus a unit test that `print_human` doesn't panic on a default-ish report (call it with a hand-built `StatusReport`).

- [ ] **Step 5: Verify + commit**

Run: `cargo test -p mn-cli` (the 2 sandbox-failing `auth_integration` loopback tests are pre-existing — ignore exactly those)

```bash
git add crates/mn-cli && git commit -m "feat(mn-cli): mnm status — shared StatusReport renderer with scriptable exit code"
```

### Task 24: Contracts + conformance

**Files:**
- Modify: `specs/001-rag-platform/contracts/mcp-tools.json`, `crates/mn-mcp/tests/result_shape.rs`, any contract test comparing tools.rs ↔ mcp-tools.json

- [ ] **Step 1:** Regenerate `mcp-tools.json` to exactly mirror `tools::list()`: 13 tools, new names/descriptions/input schemas/output schemas/annotations. (Check for a contract test: `grep -rn "mcp-tools.json" crates/` — if one parses the JSON and compares against `tools::list()`, run it to drive the JSON content; otherwise hand-sync.)

- [ ] **Step 2:** `result_shape.rs`: conformance cases for every renamed/new projector — `project_chunks` (single + multi), `project_document`, `project_parents` (new env shape), `project_sources` (paged env), `project_facets` (both shapes), `project_status` (StatusReport env), `project_install` (with `detected`), `project_search` (both flavors) — each validated against its advertised output schema. Delete cases for removed tools.

- [ ] **Step 3: Verify + commit**

Run: `VOYAGE_API_KEY= cargo test -p mn-mcp`

```bash
git add specs/001-rag-platform/contracts/mcp-tools.json crates/mn-mcp
git commit -m "docs(contracts): mcp-tools.json for the v2 surface + outputSchema conformance coverage"
```

### Task 25: Full gate + PR update

- [ ] **Step 1: Full CI surface locally:**

Run:
```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
VOYAGE_API_KEY= cargo test --workspace
cargo check -p mn-server --features integration --tests
```
Expected: clean (modulo the 2 known mn-cli auth_integration sandbox failures). Fix anything else before proceeding; run `cargo fmt` last if files changed.

- [ ] **Step 2: Sweep for leftovers:** `grep -rn "next_actions\b" crates/ --include="*.rs"` → only `suggested_next_actions`; `grep -rn "get_chunk\"\|get_document_full\|pull_models" crates/mn-mcp/src/` → no live registrations.

- [ ] **Step 3: Update PR #79 description** (`gh pr edit 79 --body …`): append a "Phase 4 — tool surface v2" section summarizing this work, linking `docs/superpowers/specs/2026-06-10-mcp-tool-surface-v2-design.md` and this plan, and refresh the test-plan checkboxes (CI integration tests for the new endpoints).

- [ ] **Step 4: Commit + push**

```bash
git add -A && git commit -m "chore: workspace fmt + contract sync for MCP tool surface v2" || true
git push
```

---

## Self-review checklist (done at write time)

- **Spec coverage:** §1 surface → Tasks 11–20; §2 response rules → Tasks 9–19; §2.3 nudge → Task 11; §2.4/2.5 → Task 11 (strip + schema descriptions); §3 cloud → Tasks 1–6 (+ overview-skeleton, a §3 omission in the spec, covered by Task 6); §4 status → Task 18; §5 install → Task 19; §6 CLI → Task 23; §7 skill → Task 22; §8 telemetry → Task 21; §9 testing → distributed per task + Task 24/25; §10 deletions → Tasks 6, 8, 13, 17.
- **Known judgment calls encoded:** `/v1/me` reports BOTH limit systems — `rate_limit` (one request bucket per caller) and `token_limits` (embedding budget, rolling hourly + daily windows via `TokenUsageLimiter::snapshot_for`); `slug` added to the list_sources brief (agents need it for filters); `mnm models pull` (CLI) is unrelated to the removed MCP `pull_models` and stays.
- **Type consistency:** `NextAction::call/user` (Task 9) used by all later tasks; `chunk_brief`/`snippet` (Task 10) used by Tasks 12–13; `SearchRenderOpts` (Task 11) matches Task 11's dispatch; `ToolAnnotations::read_only/idempotent_writer` (Task 20) matches Task 12's note; `StatusReport` fields (Task 18) match Task 23's renderer.

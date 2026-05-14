# midnight-manual

A Rust-based RAG platform for the [Midnight Network](https://midnight.network).
Three deliverables from one Cargo workspace:

- **CLI** — `midnight-manual` / `mnm` for developers and admins.
- **Local MCP server** — `mnm mcp serve` over stdio, exposing seven retrieval
  tools to AI clients (Claude Code, Cursor, etc.).
- **Cloud server** — `midnight-manual-server` on Fly.io, hosting the corpus
  and the search API.

See [`CONSTITUTION.md`](CONSTITUTION.md) for non-negotiable principles and
[`specs/001-rag-platform/`](specs/001-rag-platform/) for the v1 spec, plan,
data model, contracts, and quickstart.

## Telemetry & Privacy

Telemetry is **opt-out**, **never** carries user query content, chunk content,
bearer tokens, filesystem paths, or environment-variable values, and is
gated by a CI canary suite that fails any build leaking forbidden strings
(FR-107..114, Constitution VII).

### What is collected

Six event types, each a fixed shape of coarse-grained scalars / enums:

| Event type        | Fields                                                                                       | Where           |
| ----------------- | -------------------------------------------------------------------------------------------- | --------------- |
| `mcp_tool_call`   | `tool_name`, `latency_ms`, `result_count`, `model_state`, `rerank_on`, `outcome`             | MCP server      |
| `cli_command`     | `command`, `duration_ms`, `outcome`                                                          | CLI             |
| `ingest_complete` | `documents_added/updated/skipped`, `duration_ms`, `outcome`                                  | CLI (admin)     |
| `pull_models`     | `embedder_downloaded`, `reranker_downloaded`, `duration_ms`, `outcome`                       | CLI / MCP       |
| `mcp_startup`     | `startup_ms`, `model_state`                                                                  | MCP server      |
| `mcp_shutdown`    | `uptime_s`, `tools_served`                                                                   | MCP server      |

Every emission carries `component` (`cli` / `mcp` / `server`), the crate
`version`, and an optional `request_id` for log correlation. Adding a
field requires a coordinated bump on both the client schema and the
server-side validator (FR-109); unknown fields are dropped at the wire
boundary with a structured warning.

### What is NOT collected (forbidden set)

- Verbatim text of any user query.
- Verbatim text of any returned chunk.
- Bearer tokens, JWTs, API keys, signing secrets.
- Filesystem paths from your machine.
- Resolved environment-variable values.
- IP addresses on event rows (IPs are used transiently for rate-limiting and
  never persisted with events).
- Email addresses or user identifiers on event rows.

### Where it goes

Events `POST` to `/v1/telemetry/events` on the configured cloud server (default
`https://manual.midnight.network`). The endpoint is anonymous (no bearer
required, any supplied bearer is ignored per FR-116), returns `202 Accepted`,
and is the only auth-free POST in the surface. Client-side, events are
batched in memory and flushed every 30 seconds OR every 100 events, whichever
comes first. Failed flushes retry with jittered exponential backoff up to
three attempts within a ten-second wall-clock budget; 4xx responses drop the
batch immediately, since re-sending a malformed batch is futile.

### How to opt out

Three equivalent mechanisms; any one of them disables telemetry:

1. **Environment**: set `MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1` (truthy values:
   `1`, `true`, `yes`, `on`).
2. **Config**: `telemetry.enabled = false` in `~/.config/midnight-manual/config.toml`.
3. **Runtime**: `mnm telemetry disable` writes a persistent marker at
   `$XDG_CONFIG_HOME/midnight-manual/telemetry-disabled` (or
   `$HOME/.config/midnight-manual/telemetry-disabled`). Every CLI / MCP
   invocation reads it at startup. Reverse with `mnm telemetry enable`.
   Inspect with `mnm telemetry status`.

When disabled, the client never opens a connection to `/v1/telemetry`, any
in-memory queued events are discarded (FR-108), and the dropped-by-optout
counter is exposed via the local client API.

### Retention

- `telemetry_event_raw`: rolling **7 days** (configurable via
  `MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS`, clamped to `[1, 365]`).
  The sweep job rolls expired rows into `telemetry_aggregate_daily` and
  deletes them inside a single transaction — counters always reflect the
  rows that were actually removed (SC-065).
- `telemetry_aggregate_daily`: kept indefinitely; numeric counters only.
  Exposed at `GET /metrics` as Prometheus counter rows
  (`midnight_manual_telemetry_events_total{event_type,component}`) and a
  same-shape `midnight_manual_telemetry_events_today` gauge.

### Privacy canary

A CI test feeds a set of canary strings — query stand-ins, fake bearer
tokens, fake paths, fake env values — through every code path that handles
user-controllable content. Post-run, every captured log file and every row
of `telemetry_event_raw` is grepped for the canary set. Any match fails the
build (FR-112 / SC-061). The canary infrastructure lives in
[`crates/mn-telemetry/src/canary.rs`](crates/mn-telemetry/src/canary.rs)
and expands to cover every endpoint and tool as call sites land.

## Quick links

- Spec: [`specs/001-rag-platform/spec.md`](specs/001-rag-platform/spec.md)
- Constitution: [`CONSTITUTION.md`](CONSTITUTION.md)
- Architecture: [`specs/001-rag-platform/plan.md`](specs/001-rag-platform/plan.md)
- Project guide for AI assistants: [`CLAUDE.md`](CLAUDE.md)

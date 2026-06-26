---
id: privacy
title: Privacy & telemetry
sidebar_label: Privacy
description: What midnight-manual collects, what it never touches, how to opt out, and how the canary test enforces those promises.
---

# Privacy & telemetry

Telemetry is **opt-out**, carries no query content, chunk content, bearer tokens, filesystem paths, or environment values, and is enforced by a CI canary suite that fails any build leaking a forbidden string.

## What is collected

Seven event types, each a fixed shape of coarse scalars and closed enums:

| Event | Fields |
|---|---|
| `mcp_tool_call` | `tool_name`, `latency_ms`, `result_count`, `model_state`, `rerank_on`, `outcome` |
| `rerank` | placement, model, whether applied, and a closed-set degrade reason |
| `cli_command` | `command`, `duration_ms`, `outcome` |
| `ingest_complete` | `documents_added`, `documents_updated`, `documents_skipped`, `duration_ms`, `outcome` |
| `pull_models` | `embedder_downloaded`, `reranker_downloaded`, `duration_ms`, `outcome` |
| `mcp_startup` | `startup_ms`, `model_state` |
| `mcp_shutdown` | `uptime_s`, `tools_served` |

Every emission also carries the `component` (`cli`/`mcp`/`server`), the crate `version`, and an optional `request_id` for log correlation.

Events are sent to the Gauge telemetry service (default endpoint `https://gauge-telemetry.fly.dev`). Override the endpoint via the `MIDNIGHT_MANUAL_GAUGE_ENDPOINT` environment variable or `[telemetry].endpoint` in your config file; this is independent of the `--server`/corpus URL.

## What is NOT collected

- Verbatim text of any query or returned chunk.
- Bearer tokens, JWTs, API keys, or signing secrets.
- Filesystem paths or resolved environment-variable values.
- IP addresses on event rows (used transiently for rate-limiting, never persisted with events).
- Email addresses or user identifiers on event rows.

Searching runs against the hosted corpus, which records only counts, never your query text or the passages you read.

## How to opt out

Any one of these is enough:

1. Set the environment variable `MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1`.
2. Set `telemetry.enabled = false` in your config file.
3. Run `mnm telemetry disable`. It writes a persistent marker read at every startup. Reverse it with `mnm telemetry enable`, or check the current state with `mnm telemetry status`.

When disabled, no events are written and nothing is sent.

Two additional kill switches apply regardless of the above:

- `GAUGE_TELEMETRY_DISABLE=1` acts as a global kill switch at the Gauge layer.
- Telemetry is auto-disabled when the `CI` environment variable is set.

See [Skills and telemetry](/docs/cli/skills-telemetry) for the full `mnm telemetry` command reference.

## Delivery model

When telemetry is enabled, events are written at emit time to a crash-safe on-disk queue, then delivered out-of-band:

- The CLI spawns a detached flush process on exit.
- The MCP server uses a background flusher (approximately every 30 seconds) and drains the queue at shutdown.

## The canary

The promises above are enforced by a continuous test suite in `crates/mnm-telemetry/src/canary.rs`. Each event type is checked at the per-event level by asserting it carries no forbidden substrings from the canary set. Integration tests inject local query probes and assert they do not appear in logs or responses. **Any match fails the build.**

The canary set is sourced from the Gauge `FORBIDDEN_SUBSTRINGS` constant, which includes `"@"` as a probe prefix; any string built as `"@" + some_token` is caught by the canary test, which fails the build before it ships.

## Query text and the corpus

Telemetry is the easy half of the privacy story: it carries no content at all. The harder half is what happens to your query text when it is turned into a search vector.

The indexed corpus is built from **public Midnight repositories**: the docs site and open-source code. Nothing private is in it, and nothing you search reveals anything to other users. For full details on where query text travels during embedding, see [Embeddings](/docs/reference/embeddings).

For MCP-specific details on what the search tools accept and return, see [Searching with the MCP server](/docs/mcp/searching).

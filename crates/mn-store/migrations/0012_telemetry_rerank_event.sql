-- 0012 — register the `rerank` event_type (spec 2026-06-11 §6).
--
-- The Voyage rerank decision event (placement / model / applied / reason /
-- billed_tokens) is emitted by the CLI and MCP clients and ingested through
-- POST /v1/telemetry/events. Migration 0005's CHECK constraint (and the
-- server's ALLOWED_EVENT_TYPES allow-list, bumped in lockstep) only listed the
-- original six event types, so every `rerank` row was rejected at insertion.
--
-- 0005 is applied and immutable (editing it would change its checksum and
-- crash-loop mn-server on deploy), so the fix is a NEW migration that drops the
-- old inline CHECK and re-adds it with `rerank` included. The inline unnamed
-- CHECK from 0005 is auto-named `telemetry_event_raw_event_type_check` by
-- PostgreSQL.

ALTER TABLE telemetry_event_raw
    DROP CONSTRAINT telemetry_event_raw_event_type_check;

ALTER TABLE telemetry_event_raw
    ADD CONSTRAINT telemetry_event_raw_event_type_check CHECK (event_type IN (
        'mcp_tool_call','cli_command','ingest_complete','pull_models',
        'mcp_startup','mcp_shutdown','rerank'
    ));

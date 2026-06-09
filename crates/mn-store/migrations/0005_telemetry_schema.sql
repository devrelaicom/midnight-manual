-- 0005 — telemetry schema (Story 11, D27).
--
-- telemetry_event_raw is auto-deleted after MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS
-- (default 90; raised from FR-110's documented default of 7 — the
-- telemetry_search_daily rollup preserves the signal long-term, and 90 days of
-- raw rows provides a useful granular window) via the sweep job in mn-server.
-- telemetry_aggregate_daily is retained indefinitely.

-- ============================================================================
-- telemetry_event_raw: per-event rows, rolling 90-day retention (default)
-- ============================================================================
CREATE TABLE telemetry_event_raw (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    received_at timestamptz NOT NULL DEFAULT now(),
    event_type  text NOT NULL CHECK (event_type IN (
        'mcp_tool_call','cli_command','ingest_complete','pull_models','mcp_startup','mcp_shutdown'
    )),
    component   text NOT NULL CHECK (component IN ('cli','mcp','server')),
    version     text NOT NULL,
    fields      jsonb NOT NULL,
    request_id  text
);

CREATE INDEX idx_telemetry_event_raw_received_at ON telemetry_event_raw (received_at);
CREATE INDEX idx_telemetry_event_raw_type        ON telemetry_event_raw (event_type, received_at);

-- ============================================================================
-- telemetry_aggregate_daily: per-day counters retained indefinitely
-- ============================================================================
CREATE TABLE telemetry_aggregate_daily (
    day        date NOT NULL,
    event_type text NOT NULL,
    component  text NOT NULL,
    count      bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (day, event_type, component)
);

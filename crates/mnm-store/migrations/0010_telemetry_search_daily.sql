-- 0010 — telemetry_search_daily: dimensional rollup of search retrieval quality.
--
-- Preserves the retrieval-quality signal from mcp_tool_call (tool=search) events
-- past the raw-retention window. Populated by the sweep job before raw rows are
-- deleted. Retained indefinitely (like telemetry_aggregate_daily).

CREATE TABLE telemetry_search_daily (
    day               date NOT NULL,
    corpus_model      text NOT NULL DEFAULT '',
    attribution       text NOT NULL DEFAULT '',
    reranker          text NOT NULL DEFAULT '',
    top_source        text NOT NULL DEFAULT '',
    confidence_bucket text NOT NULL DEFAULT '',
    count             bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (day, corpus_model, attribution, reranker, top_source, confidence_bucket)
);

CREATE INDEX idx_telemetry_search_daily_day ON telemetry_search_daily (day);

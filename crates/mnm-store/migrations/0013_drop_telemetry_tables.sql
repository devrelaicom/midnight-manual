-- Gauge migration: midnight-manual no longer ingests telemetry; all usage
-- events now flow to the external Gauge server. Drop the three telemetry
-- tables (indexes and CHECK constraints drop with their tables). Forward-only;
-- never edit an applied migration.
DROP TABLE IF EXISTS telemetry_search_daily;
DROP TABLE IF EXISTS telemetry_aggregate_daily;
DROP TABLE IF EXISTS telemetry_event_raw;

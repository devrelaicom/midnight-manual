-- 0015: license metadata (spec docs/superpowers/specs/2026-07-27-preprocess-license-design.md)
-- Arrays of SPDX expression strings; NULL = nothing detected.
ALTER TABLE document ADD COLUMN license TEXT[];
ALTER TABLE source ADD COLUMN license TEXT[];

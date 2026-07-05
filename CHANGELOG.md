# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.0](https://github.com/devrelaicom/midnight-manual/compare/v0.9.0...v0.10.0) - 2026-07-03

### Added

- *(mcp)* search controls on advanced_search + get_document outline ([#157](https://github.com/devrelaicom/midnight-manual/pull/157))
- *(cli)* mnm manifest check --json structured issue output ([#145](https://github.com/devrelaicom/midnight-manual/pull/145)) ([#154](https://github.com/devrelaicom/midnight-manual/pull/154))
- *(skills)* registry-driven multi-skill support (install_skill, --skill) ([#152](https://github.com/devrelaicom/midnight-manual/pull/152))
- *(mcp)* param-alias rewriting + zero-result recovery and trust labels ([#158](https://github.com/devrelaicom/midnight-manual/pull/158))
- *(mcp)* cold-start corpus overview in no-arg facets + server instructions ([#155](https://github.com/devrelaicom/midnight-manual/pull/155))

### Fixed

- *(cli)* drop unwired ingest --include/--exclude; fix stale docs ([#144](https://github.com/devrelaicom/midnight-manual/pull/144)) ([#156](https://github.com/devrelaicom/midnight-manual/pull/156))
- *(ingest)* correct stale remediations, structural 409/413 match, tokenless-plan warning ([#140](https://github.com/devrelaicom/midnight-manual/pull/140)) ([#153](https://github.com/devrelaicom/midnight-manual/pull/153))
- *(mcp)* list `facets` in tool prose; report content-guard level in `status` ([#151](https://github.com/devrelaicom/midnight-manual/pull/151))
- *(ingest)* emit outcome=aborted IngestReport on every post-start failure ([#150](https://github.com/devrelaicom/midnight-manual/pull/150))
- *(mcp)* make symbol_path outputSchema truthful across chunk-read endpoints ([#146](https://github.com/devrelaicom/midnight-manual/pull/146))
- *(deps)* bump quick-xml to 0.41 to clear RUSTSEC-2026-0194/0195 ([#147](https://github.com/devrelaicom/midnight-manual/pull/147))
- *(mcp)* add RATE_LIMITED and AUTH_FAILED error kinds (429/401/403) ([#149](https://github.com/devrelaicom/midnight-manual/pull/149))

## [0.9.0](https://github.com/devrelaicom/midnight-manual/compare/v0.8.0...v0.9.0) - 2026-07-01

### Fixed

- *(ingest)* skip files with an over-long line at the walker ([#130](https://github.com/devrelaicom/midnight-manual/pull/130))

## [0.8.0](https://github.com/devrelaicom/midnight-manual/compare/v0.7.0...v0.8.0) - 2026-06-30

### Fixed

- ingestion correctness (zero-chunk, oversize, 32K context groups) + token refresh + Docker deploy ([#128](https://github.com/devrelaicom/midnight-manual/pull/128))

## [0.7.0](https://github.com/devrelaicom/midnight-manual/compare/v0.6.1...v0.7.0) - 2026-06-30

### Fixed

- *(ingest)* wrap chunking in a panic boundary so one bad file can't abort the run ([#125](https://github.com/devrelaicom/midnight-manual/pull/125))

## [0.6.0](https://github.com/devrelaicom/midnight-manual/compare/v0.5.0...v0.6.0) - 2026-06-29

### Other

- Unify ingest + generate on one FileFilter walker ([#116](https://github.com/devrelaicom/midnight-manual/pull/116))

## [0.5.0](https://github.com/devrelaicom/midnight-manual/compare/v0.4.0...v0.5.0) - 2026-06-25

### Other

- Client config: new fields, resolver-drift fixes, fail-loud config ([#113](https://github.com/devrelaicom/midnight-manual/pull/113))

## [0.4.0](https://github.com/devrelaicom/midnight-manual/compare/v0.3.0...v0.4.0) - 2026-06-25

### Added

- migrate telemetry to Gauge (gauge-telemetry); tear down server-side telemetry ([#111](https://github.com/devrelaicom/midnight-manual/pull/111))

## [0.3.0](https://github.com/devrelaicom/midnight-manual/compare/v0.2.4...v0.3.0) - 2026-06-24

### Added

- prompt-injection protection — ingest scanning (server) + response guarding (MCP) ([#103](https://github.com/devrelaicom/midnight-manual/pull/103)) ([#109](https://github.com/devrelaicom/midnight-manual/pull/109))

## [0.2.4](https://github.com/devrelaicom/midnight-manual/compare/v0.2.3...v0.2.4) - 2026-06-23

### Added

- add opt-in Sentry error reporting to server + client ([#102](https://github.com/devrelaicom/midnight-manual/pull/102)) ([#107](https://github.com/devrelaicom/midnight-manual/pull/107))

## [0.2.3](https://github.com/devrelaicom/midnight-manual/compare/v0.2.2...v0.2.3) - 2026-06-23

### Fixed

- make ingest 413 retry-split recursive ([#101](https://github.com/devrelaicom/midnight-manual/pull/101)) ([#105](https://github.com/devrelaicom/midnight-manual/pull/105))

## [0.2.2](https://github.com/devrelaicom/midnight-manual/compare/v0.2.1...v0.2.2) - 2026-06-23

### Added

- incremental re-ingest — skip re-embedding unchanged documents (carry-forward) ([#100](https://github.com/devrelaicom/midnight-manual/pull/100))

## [0.2.1](https://github.com/devrelaicom/midnight-manual/compare/v0.2.0...v0.2.1) - 2026-06-19

### Fixed

- resolve cross-element drift between server, CLI, and local MCP ([#98](https://github.com/devrelaicom/midnight-manual/pull/98))

## [0.2.0](https://github.com/devrelaicom/midnight-manual/compare/v0.1.0...v0.2.0) - 2026-06-18

### Fixed

- enforce --max-file-size, skip non-ingestable files, make `config show --effective` work ([#95](https://github.com/devrelaicom/midnight-manual/pull/95))

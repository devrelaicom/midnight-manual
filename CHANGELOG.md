# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

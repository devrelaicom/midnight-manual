# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.11.0](https://github.com/devrelaicom/midnight-manual/compare/v0.10.0...v0.11.0) - 2026-07-07

### Changed

- *(mcp)* align local rerank pool with CLI (RERANK_FETCH.max(limit)) ([#199](https://github.com/devrelaicom/midnight-manual/pull/199))

### Fixed

- *(search)* honor --limit above rerank floor; thread voyage_base_url through all embedder sites ([#170](https://github.com/devrelaicom/midnight-manual/pull/170)) ([#198](https://github.com/devrelaicom/midnight-manual/pull/198))
- *(auth)* OAuth CSRF nonce, ed25519 verify_strict, version_match overflow guards ([#177](https://github.com/devrelaicom/midnight-manual/pull/177)) ([#196](https://github.com/devrelaicom/midnight-manual/pull/196))
- *(skills/keys)* symlink/TOCTOU hardening for skill-dir + O_EXCL key writes ([#172](https://github.com/devrelaicom/midnight-manual/pull/172)) ([#195](https://github.com/devrelaicom/midnight-manual/pull/195))
- *(cli)* ingest report exit-after-commit, plan preview kind, code.dim fallback ([#171](https://github.com/devrelaicom/midnight-manual/pull/171)) ([#194](https://github.com/devrelaicom/midnight-manual/pull/194))
- *(cli)* surface false-success in models migrate + manifest check --strict ([#169](https://github.com/devrelaicom/midnight-manual/pull/169)) ([#191](https://github.com/devrelaicom/midnight-manual/pull/191))
- *(embedding)* correct Voyage token accounting — timeout retry + conflict-retry undercount ([#164](https://github.com/devrelaicom/midnight-manual/pull/164)) ([#190](https://github.com/devrelaicom/midnight-manual/pull/190))
- *(server)* stop ~2x content hold + validate min_confidence in /v1/search ([#187](https://github.com/devrelaicom/midnight-manual/pull/187))
- *(cli)* honor --config when resolving target server; fail loud on missing config ([#163](https://github.com/devrelaicom/midnight-manual/pull/163)) ([#185](https://github.com/devrelaicom/midnight-manual/pull/185))
- *(ingest)* isolate unknown attribution + capture full heading text ([#168](https://github.com/devrelaicom/midnight-manual/pull/168)) ([#188](https://github.com/devrelaicom/midnight-manual/pull/188))
- *(mcp)* harden render.rs — guard author-controlled fields + overflow-safe arithmetic ([#186](https://github.com/devrelaicom/midnight-manual/pull/186))
- *(mnm-core)* stop neutralize_tags panic on length-changing lowercase ([#159](https://github.com/devrelaicom/midnight-manual/pull/159)) ([#179](https://github.com/devrelaicom/midnight-manual/pull/179))
- *(store,embedding)* enforce three data-layer contracts ([#175](https://github.com/devrelaicom/midnight-manual/pull/175)) ([#197](https://github.com/devrelaicom/midnight-manual/pull/197))
- *(server)* request-handling hardening — charge invalid bearers, socket-peer IP, active-model 409 ([#176](https://github.com/devrelaicom/midnight-manual/pull/176)) ([#193](https://github.com/devrelaicom/midnight-manual/pull/193))
- *(ingest)* gate frontmatter stripping to Markdown ([#161](https://github.com/devrelaicom/midnight-manual/pull/161)) ([#180](https://github.com/devrelaicom/midnight-manual/pull/180))
- *(auth)* bound OAuth-state and challenge stores against OOM ([#181](https://github.com/devrelaicom/midnight-manual/pull/181))
- *(mcp)* harden client transport, cloud URL joins, embed-pair length check ([#192](https://github.com/devrelaicom/midnight-manual/pull/192))
- *(mcp)* JSON-RPC conformance for undetermined-id errors + malformed ids ([#173](https://github.com/devrelaicom/midnight-manual/pull/173)) ([#189](https://github.com/devrelaicom/midnight-manual/pull/189))
- *(server)* retain NULL/absent-value rows in none_of search filters ([#162](https://github.com/devrelaicom/midnight-manual/pull/162)) ([#184](https://github.com/devrelaicom/midnight-manual/pull/184))

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

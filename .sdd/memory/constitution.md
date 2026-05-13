# Midnight Manual Constitution

A Rust-based MCP server providing RAG-powered retrieval of Midnight Network documentation, examples, and ecosystem knowledge. This constitution is tailored to a small-team, long-lived, open-source community tool with a local-execution / cloud-data-store split, continuous releases, and a comprehensive testing posture.

## Core Principles

### I. API First Design (NON-NEGOTIABLE)

The MCP protocol surface is the product. Every retrieval capability is defined as an MCP tool/resource contract before implementation begins. Tool names, input schemas, and result shapes are stable public APIs and follow semver. Breaking the contract requires a major version bump and a migration note in release notes.

**Why**: The server's value lives entirely at the MCP boundary. Internal refactors are cheap; client-facing contract churn is expensive and erodes ecosystem trust.

### II. Modularity with Clean Boundaries

The codebase is organized into well-bounded crates/modules with explicit, narrow interfaces: at minimum — protocol/transport, retrieval/ranking, content store/index client, telemetry, and configuration. No circular dependencies. Cross-module communication goes through typed interfaces, never shared mutable state.

**Why**: Long-lived, small-team Rust projects rot fastest at module seams. Clear boundaries make contributions reviewable and let any module be replaced (e.g. swap the retrieval backend) without rewriting the world.

### III. Integration Tests Against Real Components

Tests prefer real MCP clients and a real (or near-real, e.g. local fixture-backed) data store over mocks. Mocks are a last resort, used only when the real component is non-deterministic, expensive, or unavailable in CI. Every MCP tool has at least one integration test that exercises the full request → retrieve → respond path. Critical paths (query parsing, ranking, error reporting) carry unit tests; trivial code does not.

**Why**: A RAG-over-cloud system has many places where mocked tests can pass while real users see broken retrieval. We test what users will actually run.

### IV. Frictionless Setup & Speed Are Features

Install must be one command on every supported channel: `cargo install`, `brew install`, or downloading a release binary — followed by adding the server to an MCP client config. p95 retrieval latency must be under 1 second under nominal cloud-store conditions. Performance regressions are treated as bugs and gated in CI where measurable.

**Why**: This is a developer tool used inline during coding. Setup friction kills adoption; latency above ~1s breaks flow and pushes users back to web search.

### V. Errors Are Human-Readable and Actionable

Every error surfaced to the MCP client states what failed, why, and what the user can do next. Network failures name the endpoint and suggest checking connectivity. Auth failures name the credential and how to refresh it. Stale-index conditions are reported, not silently ignored. Internal panics are caught at the MCP boundary and translated into structured error responses — the server never crashes the client.

**Why**: Failures inside an MCP server are invisible by default; the client only sees what we tell it. Cryptic errors waste developer time and generate support load disproportionate to the underlying problem.

### VI. Graceful Degradation, Fail Fast on Programmer Errors

Transient cloud-store failures degrade gracefully: retries with backoff, clear "service unavailable" responses, and — where feasible — cached or partial results with explicit staleness signaling. Programmer errors (invalid configuration at startup, schema violations from our own code, contract mismatches) fail fast and loudly with full context. We distinguish "the world is broken" (degrade) from "the code is broken" (crash early, fix it).

**Why**: A cloud-backed local server lives or dies by how it handles network reality. But silently swallowing internal bugs to "stay up" trades a visible failure for a corrupted result, which is worse for a knowledge-retrieval tool.

### VII. Observability First, Telemetry with Consent

Structured logging is wired in from day one (request id, tool name, latency, outcome). Telemetry is opt-out, anonymized, and never includes query content, user identifiers, file paths, environment values, secrets, or any PII — only coarse-grained usage and error metrics. The opt-out mechanism is documented in the README and discoverable from the CLI itself. We never log secrets, tokens, or credentials at any level.

**Why**: We need visibility to debug a tool we can't reach into, and to understand which retrieval patterns serve users. We owe contributors and users a defensible privacy posture in exchange.

### VIII. Input Validation at Every Boundary

All inputs crossing a trust boundary — MCP requests, configuration files, environment variables, cloud-store responses — are parsed into typed Rust values with explicit schemas. Untyped data does not propagate inward. Deserialization failures produce actionable errors (see Principle V), never panics.

**Why**: Rust's type system is a security and correctness asset; ignoring it at boundaries forfeits the benefit. RAG systems are particularly susceptible to garbage-in/garbage-out failures that look like bad answers, not bugs.

### IX. Trunk-Based Development with Continuous Release

Work happens on short-lived branches off `main`. Every PR is reviewed by at least one other contributor before merge. Merges to `main` trigger an automated release pipeline: version bump (Conventional Commits + release-please or equivalent), changelog generation, crate publish, GitHub release, and Homebrew tap update. `main` is always in a deployable, releasable state.

**Why**: Continuous release rewards small batches and punishes large, risky merges. It keeps the contributor experience tight and the release cadence honest.

### X. Conventional Commits & Semantic Versioning

All commits follow `type(scope): subject` format. Breaking changes are explicitly marked (`!` or `BREAKING CHANGE:` footer). Versioning is strict semver: MAJOR for MCP contract breaks, MINOR for additive capabilities, PATCH for fixes. Release notes call out behavioral changes that affect retrieval quality even when the contract is unchanged.

**Why**: Continuous release pipelines need machine-readable commit history. Semver is a contract with downstream users — including AI agents that pin tool versions.

### XI. Documentation Lives With Code

The README covers install on each supported channel, MCP client configuration, opt-out telemetry, and a basic query example — and is verified at release time. Each module has a brief doc-comment explaining its responsibility and key types. Comments in code explain *why* a non-obvious choice was made, never *what* the code does. Public APIs (Rust and MCP) carry doc comments that compile into rustdoc and into the MCP tool descriptions seen by clients.

**Why**: Long-lived OSS projects with rotating contributors decay without close-to-the-code docs. The MCP tool descriptions are also part of the user experience — agents read them.

## Additional Constraints

### Technology & Distribution
- **Language**: Rust (stable channel; MSRV pinned and tested in CI).
- **Distribution channels**: `cargo install`, GitHub Releases (prebuilt binaries for major OS/arch), Homebrew tap, build-from-source. All channels release from the same commit.
- **Dependencies**: Prefer mature, widely-used crates. New direct dependencies require justification in the PR description. Audit for license compatibility (the project is open source; copyleft dependencies need explicit review).

### Security & Privacy Constraints
- Telemetry payloads are reviewed at PR time when telemetry code changes; any new field must be justified and documented.
- Secrets (API keys, auth tokens for the cloud store) are loaded only from environment variables or platform-appropriate keystores — never from committed config files.
- The opt-out flag for telemetry must be documented in `--help`, the README, and the first-run output.

### Performance Standards
- p95 retrieval latency: < 1 second under nominal cloud-store conditions.
- Cold start (process launch to MCP handshake complete): < 500ms.
- Performance-sensitive paths have benchmarks; significant regressions block merge.

## Development Workflow

### Quality Gates (enforced in CI)
- `cargo fmt --check` and `cargo clippy -- -D warnings` pass.
- `cargo test` (unit + integration) passes on all supported platforms.
- MCP contract tests pass against a real local MCP client harness.
- Documentation builds without warnings (`cargo doc`).
- Release-please / Changesets validates Conventional Commits.

### Review Requirements
- Every PR is reviewed by at least one other maintainer before merge.
- Changes to the MCP contract, telemetry payloads, or security-sensitive code require explicit reviewer acknowledgement of the impact area.
- Author and reviewer must agree the change is consistent with the constitution; deviations require an Amendment (see Governance).

### Issue & Release Hygiene
- Bugs reproduce in the form of a failing test before the fix is written, where feasible.
- Each release has a curated changelog distinguishing user-visible changes from internal cleanup.
- Breaking changes are announced one minor release ahead when possible.

## Governance

This constitution supersedes ad hoc practice. When this document and a habit conflict, this document wins until amended.

- **Compliance**: Every PR review verifies the change is consistent with these principles. Reviewers may block on constitutional grounds; authors may rebut by proposing an amendment.
- **Justifying complexity**: Any addition that increases architectural surface (new module, new dependency, new MCP tool) must articulate why simpler options were rejected.
- **Amendments**: Changes to this constitution require a PR that updates `CONSTITUTION.md`, a written rationale, and approval from a maintainer who did not author the change. A migration plan is required if the amendment invalidates existing code or workflows. Version is bumped per the rules below.
- **Versioning of this document**: MAJOR for principle removal or backward-incompatible governance change; MINOR for new principle or materially expanded section; PATCH for clarifications and wording.
- **Runtime guidance**: Day-to-day development guidance that does not rise to the level of a principle (style preferences, tooling tips, project-specific lore) lives in `CLAUDE.md` / `AGENTS.md` / contributor docs, not here.

**Version**: 1.0.0 | **Ratified**: 2026-05-07 | **Last Amended**: 2026-05-07

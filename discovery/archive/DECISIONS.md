# Decision Log: rag-platform

*Chronological record of all decisions made during discovery.*

---

[Decision entries will be added as decisions are made]

## D1: Embedding library: fastembed-rs — 2026-05-13

**Context**: All three components (CLI ingestion, cloud read API, local MCP server) need to produce vectors. Model parity between ingest and query is non-negotiable for vector search to work.

**Question**: Which Rust embedding crate do we standardize on?

**Options Considered**:
fastembed-rs (ONNX); candle (HF safetensors); rust-bert (libtorch); embed_anything; outsourced embedding API

**Decision**: fastembed-rs (crate: fastembed)

**Rationale**: Mature, ONNX-backed, broad model support (BGE/jina/MiniLM/nomic), 3-5x faster than Python equivalents, integrates a reranker in the same crate which means one ML dependency surface across the project. Candle is more flexible but slower and less specialized for embedding-only workloads. An outsourced embedding API breaks Constitution VII (telemetry/privacy: query content would leave the machine) and adds latency.

**Implications**:
Lock vector dimension at Story 1 to the chosen model's output size. CLI and MCP server both link fastembed. Model changes are treated as re-embedding migrations.

**Stories Affected**: 1,2,3,5

**Related Questions**: [Questions not specified]

---

## D2: Reranking: server-side cross-encoder via fastembed-rs — 2026-05-13

**Context**: Hybrid FTS + vector retrieval lifts recall but precision-at-top-K benefits from cross-encoder reranking. Constitution targets p95 < 1s; reranking must fit.

**Question**: Where (and whether) to rerank?

**Options Considered**:
No reranking (rely on RRF alone); cross-encoder rerank in MCP server (client-side); cross-encoder rerank in cloud server

**Decision**: Cross-encoder reranking in the local MCP server, using fastembed-rs reranker support. Tool flag to disable for ultra-low-latency callers.

**Rationale**: Reranking client-side keeps the cloud server stateless and CPU-light (cost on Fly.io scales linearly with compute), pushes the heaviest ML workload onto the developer's machine (which already runs a local LLM client), and means reranking improvements ship via MCP server release without touching production. BGE-reranker-v2-m3 ONNX gives ~160ms CPU for K=20 — fits the budget.

**Implications**:
MCP server has a non-trivial ML dependency footprint. Cold start budget (< 500ms per Constitution) must include model load — investigate lazy/async model load. Story 5 owns reranker. Story 6 (confidence) consumes reranker scores.

**Stories Affected**: 5,6

**Related Questions**: [Questions not specified]

---

## D3: Query rewriting: delegate to the caller — 2026-05-13

**Context**: HyDE / multi-query rewriting boosts recall but costs an LLM call. The MCP client is already an LLM that can rewrite for free.

**Question**: Should the server perform query rewriting?

**Options Considered**:
Server-side LLM rewriting (HyDE); bundled small local LLM in MCP server; outbound LLM API call; delegate rewriting to the calling agent with a multi-query tool signature

**Decision**: Delegate rewriting to the caller. The retrieval tool accepts queries: string[] (1..N). Server runs hybrid retrieval per query and RRF-merges across both queries and retrieval modes. MCP tool description documents HyDE/multi-query as a recommended client pattern.

**Rationale**: Avoids bundling a generation model in the MCP server (frictionless setup, Constitution IV). Avoids outbound LLM calls (privacy, Constitution VII). Lets sophistication scale with the calling agent: dumb clients get a sensible default; smart agents extract more recall by passing better queries.

**Implications**:
Story 5: tool input shape includes queries: string[]. Story 7 scope shrinks to: server-side support for multi-query + documentation/cookbook for the pattern. No LLM dependency added to the project.

**Stories Affected**: 5,7

**Related Questions**: [Questions not specified]

---

## D4: Hybrid retrieval pattern: parallel FTS + pgvector, RRF in app code — 2026-05-13

**Context**: Need both lexical and semantic search; Postgres + pgvector chosen. Must decide native FTS vs an extension and where to fuse.

**Question**: How do we structure hybrid retrieval on Postgres + pgvector?

**Options Considered**:
Native tsvector + pgvector + RRF in app; pg_search/ParadeDB BM25 + pgvector + RRF in app; pg_search BM25 + pgvector + RRF in SQL; vector-only with metadata filters

**Decision**: Native Postgres tsvector (with ts_rank_cd) and pgvector run as parallel queries. RRF (k=60) is computed in Rust in the cloud server, not in SQL. Reserve the option to swap in pg_search later as an optimization only if measurement shows native FTS recall is a bottleneck.

**Rationale**: Native FTS works on any managed Postgres including Fly.io, so deploy is unconstrained by extension availability. RRF in app code is unit-testable and tuneable without DB migrations. Hybrid + RRF lifts recall@10 to ~91% in published benchmarks — likely enough headroom that a true-BM25 upgrade is a v2 concern.

**Implications**:
Story 1 schema: chunks have both tsvector (generated/stored) and vector columns, both indexed (GIN for FTS, IVFFlat/HNSW for vector). Story 4: read API runs two parallel queries then RRF-merges. Story 9 (ops): no extension prerequisite — simpler deploy.

**Stories Affected**: 1,4,9

**Related Questions**: [Questions not specified]

---

## D5: Default embedding model: BGE-small-en-v1.5 (384 dims) — 2026-05-13

**Context**: Vector dimension is a one-time decision that affects DB storage, query speed, and recall. Midnight docs are English-only and the corpus is bounded (one network's worth of docs + curated examples), so we don't need multilingual or maximum-quality variants.

**Question**: Which fastembed-supported model do we default to?

**Options Considered**:
bge-small-en-v1.5 (384 dims); bge-base-en-v1.5 (768 dims); bge-large-en-v1.5 (1024 dims); nomic-embed-text-v1.5 (768 dims, long-context); jina-embeddings-v2-base-en (768 dims)

**Decision**: bge-small-en-v1.5 (384 dimensions).

**Rationale**: Smallest credible quality tier. Fastest CPU inference (~30-50ms/query on laptop). Cheapest storage (384*4 bytes = 1.5 KB/vector). Quality is competitive with text-embedding-3-small for short technical chunks. If retrieval quality is measured as insufficient post-MVP, upgrade to bge-base is a re-embedding migration, not a redesign — the schema already treats model changes as migrations.

**Implications**:
Story 1 schema: vector column is VECTOR(384). Schema stores the model name and version per chunk so a corpus can transitionally hold multiple model dimensions during a migration.

**Stories Affected**: 1,2,3,5

**Related Questions**: [Questions not specified]

---

## D6: Code chunking: tree-sitter for known languages, line-window fallback — 2026-05-13

**Context**: Code must chunk on logical boundaries (functions, classes, modules) and reconstruct back to source. The user requires Compact, TypeScript/JavaScript, and Rust minimum.

**Question**: How do we segment source files into logical chunks?

**Options Considered**:
Fixed-line windows; AST-based via tree-sitter; LSP-based via rust-analyzer/typescript-language-server; LLM-based segmentation

**Decision**: tree-sitter grammars for known languages (TypeScript/TSX, JavaScript/JSX, Rust, and Compact if a grammar exists; otherwise heuristic line-based for Compact). Unknown languages fall back to a fixed-line-window chunker (default 60 lines, 20-line overlap, configurable).

**Rationale**: tree-sitter is the de facto standard for code-aware tooling (used by GitHub semantic, Continue, Cody) — fast, incremental, has Rust bindings (tree-sitter crate). Grammars for TS/JS/Rust are first-class; a Compact grammar may need to be vendored or written. Line-window fallback guarantees the system always indexes, even for niche languages.

**Implications**:
Story 3 (code ingest) carries a tree-sitter dependency. Compact grammar status is a clarifying question — if no grammar exists, ship line-window for Compact at MVP and add the grammar later. Each chunk stores (start_byte, end_byte) into the original file so source reconstruction is trivial.

**Stories Affected**: 1,3

**Related Questions**: [Questions not specified]

---

## D7: Parent-chain inference: filesystem default, manifest override — 2026-05-13

**Context**: Chunks need a parent chain up to a source root (e.g. chunk → page → guide → docs). Markdown docs sites (Docusaurus, Starlight, MkDocs) carry sidebar / nav config; loose collections do not.

**Question**: How do we determine each page's parent chain at ingest time?

**Options Considered**:
Filesystem only (parent = directory); manifest only (admin supplies hierarchy YAML); two-mode: filesystem default + optional manifest override; auto-detect from known docs frameworks

**Decision**: Filesystem default + optional manifest override. Default: each page's parents are its directory chain up to the ingest root. Manifest mode (--manifest hierarchy.yaml): admin supplies a YAML that maps page paths to logical parents (overrides filesystem). Auto-detect (Docusaurus sidebar.js, etc.) is explicitly out of scope for MVP.

**Rationale**: Filesystem mode gives a working answer for any source with zero config. Manifest mode handles real docs sites where the on-disk layout doesn't match the published hierarchy (e.g. a flat /docs folder rendered as a multi-level guide). Auto-detect of framework conventions is high-effort and brittle — defer.

**Implications**:
Story 1 schema: parent chain stored as a denormalized array of node ids per chunk (or a closure-table). Story 2 ingest: accepts --manifest. The manifest schema is its own small thing; document at the start of Story 2.

**Stories Affected**: 1,2

**Related Questions**: [Questions not specified]

---

## D8: Source versioning: per-ingest source snapshots, not per-chunk — 2026-05-13

**Context**: Midnight ships breaking changes often. Retrieved chunks must answer 'is this current?'. Re-chunking is acceptable on content change.

**Question**: How do we model versioning of ingested content?

**Options Considered**:
No versioning (overwrite); per-chunk hash dedup; per-page version; per-source version snapshots; full corpus snapshots

**Decision**: Per-source version snapshots. Each ingest run for a given source_id creates a new immutable 'source_version' row (uuid, ingested_at, content_hash, mn-manual ingest cli version, status). Chunks belong to one source_version. Read API returns chunks from the latest live source_version by default; older versions are retained for a configurable window (default: keep last 3, time-bounded retention behind that). 'Retire' marks all of a source's versions as inactive without deleting.

**Rationale**: Per-source snapshots give clean 'roll back the last ingest' semantics, support side-by-side A/B for retrieval quality, and naturally answer 'is this the current version?'. Per-chunk versioning is more granular but more complex and saves little disk vs the operational ergonomics gain. Full corpus snapshots are operationally heavy and break independent updates.

**Implications**:
Story 1 schema: source, source_version, chunk are three tables. chunk has FK to source_version (not source). Read API filters to active versions by default; admin tooling can target a specific version. Story 8 (CLI lifecycle) gets list-versions / promote / rollback / retire commands.

**Stories Affected**: 1,4,8

**Related Questions**: [Questions not specified]

---

## D9: Compact package/module detection: in-source 'module' declarations — 2026-05-13

**Context**: Compact has no Cargo.toml/package.json equivalent. User confirmed (FungibleToken.compact example) that Compact files declare a top-level 'module <Name> { ... }' block. Items inside the module are marked 'export'.

**Question**: How do we identify the package/library identifier for a Compact source file?

**Options Considered**:
Directory name; repo name; in-source 'module Foo {' parse; explicit CLI flag only

**Decision**: Parse the source file's top-level 'module <Name> { ... }' declaration. The Name token becomes the package identifier. If a file contains no module declaration (e.g. an example snippet), fall back to directory name. An explicit '--compact-package=<name>' CLI flag overrides both.

**Rationale**: User-confirmed Compact convention: each .compact file carries an in-source module declaration. Parsing it directly is unambiguous and matches developer intent. Mirrors how we'd extract a Rust crate name from 'Cargo.toml [package].name' or a TS package name from 'package.json .name'.

**Implications**:
Story 3 (code ingest) gains a lightweight Compact parser — sufficient to extract the module name; full grammar not required at MVP. Story 1 schema: 'package' identifier on chunk metadata; populated per language: 'Cargo.toml [package].name' for Rust, 'package.json .name' for TS/JS, 'module <Name>' for Compact.

**Stories Affected**: 1,3

**Related Questions**: [Questions not specified]

---

## D10: Admin auth: challenge-response with public/private keys; file-backed user store — 2026-05-13

**Context**: Admin write authentication needs to be auditable, revocable, and bootstrap-friendly. The user wants a Wireguard/SSH-style flow with no third-party identity provider.

**Question**: How do admin clients authenticate to the cloud server for writes?

**Options Considered**:
Shared API token; OIDC (GitHub OAuth); challenge-response with Ed25519 keys; mutual TLS

**Decision**: Per-user public/private keypair (Ed25519). Server holds a list of (user_id, public_key, role) entries in a file on disk (loaded at process start), not in the DB. Auth flow: client announces user_id; server returns a fresh random challenge nonce; client signs with private key; server verifies with stored public key; on success, server issues a short-lived bearer token (JWT or opaque, TBD) used for subsequent requests until expiry.

**Rationale**: File-backed user store enables bootstrap during deploy (commit the initial admin user to the deploy config) and clean recovery (redeploy with a fresh user file revokes everyone). Ed25519 keys are tiny, fast, and well-supported in Rust (ed25519-dalek). No third-party dependency. Each request can still be attributed to a user_id for audit.

**Implications**:
Story 8 (CLI lifecycle): adds 'keys generate' (local keypair), 'users add/list/show/update/remove' (writes to the server's user file via authenticated API, or locally if used at deploy bootstrap), 'login' (does the challenge-response, caches the bearer token in a local keychain or config file). Story 9 (cloud ops): the user store file is part of the deploy config; format is versioned TOML or JSON with a typed schema parsed at startup (Constitution VIII). Token issuance: pick JWT-with-short-TTL vs opaque-with-DB-lookup later in Story 9.

**Stories Affected**: 8,9

**Related Questions**: [Questions not specified]

---

## D11: Read auth: anonymous + GitHub SSO uplift + CIDR override windows — 2026-05-13

**Context**: The read API is public to the Midnight ecosystem but must be protected from abuse. Hackathons regularly produce 100s of users behind one NAT IP, which would trip naive per-IP limits.

**Question**: How do we authenticate and rate-limit read traffic?

**Options Considered**:
Truly anonymous (no limits); per-IP limit only; required GitHub SSO for everyone; tiered: anonymous-lowrate / SSO-highrate

**Decision**: Three-tier rate limiting: (1) anonymous: low per-IP limit (e.g. 30 req/min, exact value TBD in Story 4); (2) GitHub-SSO-authenticated: higher per-user limit (e.g. 600 req/min); (3) admin-configured CIDR overrides with TTL. CIDR overrides are stored as DB rows: (cidr, limit_rps, expires_at, created_by, note). Cloud server checks CIDR overrides first, then per-user/per-IP tier.

**Rationale**: Solves the hackathon NAT problem cleanly. Admins lift limits for a known IP range and time window without changing global config or shipping a new server release. GitHub SSO is free, friction is low for the target audience (developers already have GitHub accounts), and it produces a stable per-user identity for sustainable use.

**Implications**:
Story 4 (read API): rate limiter consults CIDR overrides + tier table. Story 8 (CLI lifecycle): 'ratelimits add --cidr 169.155.237.15/25 --limit 200/s --ttl 48h --note hackathon-london', plus list/remove/extend. Story 9: GitHub OAuth app setup is part of deploy. Story 11 (telemetry): hits should be aggregated by tier for visibility, never by identity (Constitution VII).

**Stories Affected**: 4,8,9,11

**Related Questions**: [Questions not specified]

---

## D12: ML model lifecycle: CLI-managed, MCP-server-aware, server enforces model match — 2026-05-13

**Context**: Two ML models live on the developer machine (embedding + reranker) and must match the model the cloud corpus was indexed with. Mismatches must be detected and recoverable without manual debugging.

**Question**: Where does model management live, and how do we detect and recover from model mismatches?

**Options Considered**:
MCP server downloads silently; CLI manages; both equally; user manages manually

**Decision**: CLI is the primary surface for model management. CLI commands: 'models list' (what models exist locally and which are active), 'models pull' (download or upgrade), 'models prune' (remove unused). The MCP server exposes a mirror tool — 'mn_manual_pull_models' — so an agent can invoke the same action from within a session. Every request from the MCP server to the cloud read API includes the embedding model identifier (name@version) used to compute the query vector. The cloud server compares against the corpus's active embedding model. On mismatch the cloud server returns a structured error (HTTP 409 + typed JSON body) naming the corpus model. The MCP server translates this into an actionable MCP tool error instructing the agent to call 'mn_manual_pull_models'. On missing-model startup the MCP server starts in a 'degraded' state where every retrieval tool returns the same actionable error.

**Rationale**: CLI-primary keeps the install flow explicit (install CLI → pull models → install MCP server). Server-side enforcement of model identity makes mismatches impossible to silently corrupt retrieval. Mirror MCP tool means agents can self-heal without the user dropping back to a terminal.

**Implications**:
Story 1 schema: 'source_version' tracks the embedding model used; cloud server exposes a /models endpoint or includes the current model in every response. Story 4 (read API): every read request body or header carries 'client_embedding_model'; mismatch returns 409 with body { error: 'embedding_model_mismatch', corpus_model: 'bge-small-en-v1.5@1', client_model: 'all-MiniLM-L6-v2@1', remediation: 'pull bge-small-en-v1.5' }. Story 5 (MCP server): three states — ready / models-missing / models-stale; tools surface state-specific errors. Story 8 (CLI lifecycle): models subcommands. Story 10 (distribution): install docs spell out the three-step install (CLI → pull → MCP install).

**Stories Affected**: 1,4,5,8,10

**Related Questions**: [Questions not specified]

---

## D13: Page-level (not chunk-level) provenance URLs; renamed 'canonical' → 'published_url' — 2026-05-13

**Context**: User correctly noted that URLs apply to whole pages/files, not individual chunks. 'Canonical' was an overloaded term.

**Question**: Where do source/published URLs live in the schema, and what do we call them?

**Options Considered**:
Per-chunk both URLs; per-page both URLs; per-source-version only

**Decision**: URLs live on the 'page' (Markdown) or 'file' (code) entity, not on individual chunks. Two URL fields, both nullable: 'source_url' (where the raw content was fetched from — e.g. raw.githubusercontent.com/...) and 'published_url' (where end users actually read the content — e.g. docs.midnight.network/getting-started/quickstart). 'published_url' replaces the earlier 'canonical_url' name. Chunks inherit both via their FK to the page/file row.

**Rationale**: Per-page storage is normalized — re-chunking a page doesn't churn chunk-level URL fields. The two-URL split distinguishes 'where this lives in version control' from 'where users actually see it' — both useful for different downstream uses (citations, link-back UX, freshness checks).

**Implications**:
Story 1 schema: 'page' / 'file' tables carry source_url + published_url. Citation construction in Story 5 MCP responses picks published_url when set, falls back to source_url. Story 2 ingest: --source-url and --published-url flags, or read from manifest.

**Stories Affected**: 1,2,5

**Related Questions**: [Questions not specified]

---

## D14: Default embedding model (revised): bge-base-en-v1.5 — 2026-05-13

**Context**: Supersedes D5. User noted Fly.io storage at /bin/zsh.28/GB-month makes storage essentially free, weakening the bge-small case.

**Question**: With storage cost off the table, what is the right v1 default embedding model?

**Options Considered**:
bge-small (384); bge-base (768); gte-base (768); nomic-embed-text-v1.5 (768, 8K context); bge-large (1024); gte-large (1024)

**Decision**: bge-base-en-v1.5 (768 dims) as the v1 default. PENDING USER CONFIRMATION — alternative under consideration: gte-base-en-v1.5 (~+0.8 MTEB at the cost of breaking BGE family alignment with the reranker).

**Rationale**: (1) Family-aligned with the BGE reranker chosen in D2 — single training-convention surface across embed/rerank. (2) ~+1.6 MTEB retrieval over bge-small at modest cost (~70ms embed, ~450MB RAM). (3) Latency budget remains comfortable: ~70 (embed) + ~100 (cloud) + ~100 (db) + ~160 (rerank) ≈ 430ms typical, well under 1s p95. (4) RAM under 500MB is unobtrusive next to typical IDE + AI client. (5) Upgrade path to bge-large is well-defined via D12 model-mismatch detection if quality measurement demands it.

**Implications**:
Story 1: vector column dimension changes from VECTOR(384) to VECTOR(768). All decisions referencing 384 dims (in D5) are superseded. Story 5: cold start budget reassessment for MCP server — bge-base + bge-reranker-base loaded eagerly is ~600-700MB RAM; lazy load may be needed to keep cold start under 500ms.

**Stories Affected**: 1,2,3,5

**Related Questions**: [Questions not specified]

---

## D15: Versioning retention: keep last 5 source-versions per source — 2026-05-13

**Context**: Supersedes the 'last 3' retention default in D8. User confirmed: keep versioning, retention = 5.

**Question**: How many historic source-versions do we retain per source?

**Options Considered**:
1 (current only); 2 (current + previous); 3; 5; unbounded with time-based eviction

**Decision**: Default retention = last 5 source-versions per source. Older versions are eligible for cleanup by a periodic sweep job. Retention is per-source-configurable (override on the source row) for special cases like long-term archival of a deprecated tutorial.

**Rationale**: Five steps of rollback is enough headroom for: (1) a bad ingest, (2) a bad fix to a bad ingest, (3) ongoing A/B during the recovery, plus (4-5) two prior reference points for comparison. Beyond five, marginal value drops sharply while operational cost (storage, sweep complexity, CLI listing noise) climbs linearly. User confirmed this as the desired default.

**Implications**:
Story 1 schema: source row carries retention_count INT NOT NULL DEFAULT 5. Story 8 (CLI lifecycle): 'sources update --retention <n>' to override per-source. Story 9 (ops): retention sweep is a periodic job in the cloud server or a CLI command — TBD in Story 9.

**Stories Affected**: 1,8,9

**Related Questions**: [Questions not specified]

---

## D16: CLI binary name: midnight-manual with mnm alias — 2026-05-13

**Context**: Single project ships an admin CLI used both interactively and in CI.

**Question**: What is the binary name?

**Options Considered**:
midnight-manual; mnm; mn; midnight; mn-manual

**Decision**: Two installed binaries: midnight-manual (canonical, used in docs and scripts) and mnm (short alias for interactive use). Both built from the same crate as separate [[bin]] entries in Cargo.toml, both dispatching to the same entry point.

**Rationale**: Canonical full name optimizes for discoverability and unambiguous scripting; mnm alias optimizes for interactive typing speed. Same binary content via cargo's bin aliasing, so no extra build cost.

**Implications**:
Cargo.toml ships two [[bin]] sections. Homebrew formula installs both; cargo-install must install both. Release process verifies both names exist post-install.

**Stories Affected**: 2,3,8,10

**Related Questions**: [Questions not specified]

---

## D17: Global CLI flags — 2026-05-13

**Context**: Every command shares a common flag surface. Set the contract once so each story doesn't reinvent it.

**Question**: Which flags are global, accepted on every command?

**Options Considered**:
--json/--quiet/--server/--profile/--no-color; same but with --config replacing --profile and --log-level added; bigger or smaller surface

**Decision**: Global flags: --json (structured output), --quiet (suppress non-essential stdout), --server <url> (override cloud server URL), --config <path> (override config file path), --log-level <trace|debug|info|warn|error> (stderr verbosity, default info), --no-color (disable ANSI). No --profile.

**Rationale**: User-specified set. --config is more explicit than --profile and covers the same use case (point at a different file). --log-level affects stderr/diagnostics; --quiet affects stdout. The two are orthogonal — user can have --log-level=debug for tracing while --quiet keeps stdout JSON-only.

**Implications**:
Every command parser inherits the global flag set (clap derive with a shared GlobalOpts struct). Config-file precedence and --log-level routing tested once, applied everywhere. Story 2/3/8 build on this contract without redefining flags.

**Stories Affected**: 2,3,8,9,10

**Related Questions**: [Questions not specified]

---

## D18: Configuration discovery order — 2026-05-13

**Context**: CLI accepts settings from multiple sources; precedence must be unambiguous.

**Question**: Where does the CLI look for configuration, and in what order?

**Options Considered**:
Env-only; flag-only; flag > env > file; env > flag > file; XDG file > env > flag

**Decision**: Discovery order (highest priority first): (1) command-line flag, (2) environment variable (MIDNIGHT_MANUAL_* prefix), (3) config file at $XDG_CONFIG_HOME/midnight-manual/config.toml (falls back to ~/.config/midnight-manual/config.toml), (4) built-in defaults.

**Rationale**: Standard Unix convention: explicit flag wins for one-off overrides; env supports CI/secrets; XDG file supports persistent user config; defaults are the safety net. Standard order matches gh, kubectl, cargo.

**Implications**:
Config loader resolves each setting independently through the precedence chain. Sensitive values (tokens) MUST come from env or keychain, never from the config file. Config file uses TOML (Rust-idiomatic) with a versioned schema.

**Stories Affected**: 2,3,8,9,10,11

**Related Questions**: [Questions not specified]

---

## D19: CLI command grouping: noun-first — 2026-05-13

**Context**: As the CLI grows, command discoverability matters.

**Question**: How are commands grouped — verb-first (add-source) or noun-first (sources add)?

**Options Considered**:
verb-first (add-source); noun-first (sources add); flat namespace; mixed

**Decision**: Noun-first grouping. Commands are organized as 'mnm <noun> <verb> [args]'. Cross-cutting actions (ingest, login, doctor) live at the top level when they don't fit a noun cleanly.

**Rationale**: Matches gh, aws, kubectl, cargo conventions. Scales better as the command set grows — discovering all source operations is 'mnm sources --help'. New developers learn the shape once and predict the rest.

**Implications**:
Command tree at the top level: sources, versions, ingest, models, users, keys, ratelimits, config, login, doctor (subset; full list emerges through Stories 2-9). Each noun-group is a clap subcommand with its own verbs.

**Stories Affected**: 2,3,8,9,10

**Related Questions**: [Questions not specified]

---

## D20: User store is a deployable artifact (load-only, no runtime mutation) — 2026-05-13

**Context**: D10 specified a file-backed user store loaded at process start. Need to nail down whether the running server can mutate it.

**Question**: Can the cloud server mutate the user store at runtime, or is it strictly read-only?

**Options Considered**:
Runtime CRUD via /v1/admin/users; load-only (admin edits the artifact and redeploys); hybrid (mutate then snapshot to FS); external user-management service

**Decision**: Load-only. The user store is a TOML file shipped as a Fly.io secret (its content IS the secret). The cloud server reads it once at startup into memory. There are NO runtime mutation endpoints. Adding, updating, or revoking a user means editing the TOML and redeploying — which also rotates the JWT signing secret, invalidating any outstanding admin tokens. The CLI's 'mnm users add/list/show/update/remove' commands edit a LOCAL copy of the file that the admin then commits to the secret store and redeploys.

**Rationale**: Matches the user's explicit intent ('redeploy resets keys'). Avoids a stateful side-channel on Fly.io's ephemeral disk. Eliminates a class of subtle bugs (write-through inconsistencies between memory state and on-disk state). Keeps the admin-token revocation story trivial: rotate the JWT signing key on every deploy and outstanding tokens die.

**Implications**:
Story 8 (CLI lifecycle): 'mnm users' commands operate on a local file (with --user-store-path or env override). No POST /v1/admin/users endpoint exists. Story 9 (this story): server-side user store loader; failure to parse the user store at startup is a fatal error (Constitution VI — programmer error, fail fast).

**Stories Affected**: 8,9

**Related Questions**: [Questions not specified]

---

## D21: Admin tokens: short-lived JWT signed with HS256 — 2026-05-13

**Context**: D10 deferred the token issuance shape ('JWT-with-short-TTL vs opaque-with-DB-lookup'). Need a concrete choice for Story 9.

**Question**: How are admin bearer tokens minted, validated, and revoked?

**Options Considered**:
JWT HS256 with short TTL; JWT RS256/EdDSA; opaque random tokens persisted in DB; PASETO

**Decision**: JWT signed with HS256 (HMAC-SHA-256), 1-hour TTL, signing secret loaded from a Fly.io secret. Claims: sub (user_id), iat, exp, role, jti (random id for telemetry correlation). No refresh tokens — admins re-run challenge-response when their token expires (or, in CI, set TTL via an env override at the auth step).

**Rationale**: JWT is stateless — no DB round-trip on every request — which keeps the read API fast and the implementation simple. HS256 is fine for a single deployment unit (no third-party verifiers). 1-hour TTL bounds the blast radius of token leak without making interactive admin use annoying. Revocation needs are met by 'rotate the signing secret on redeploy' — same pattern as D20.

**Implications**:
Story 9: jsonwebtoken crate (or similar) for sign/verify; the HS256 secret is a required Fly.io secret at deploy. CLI 'mnm login' caches the JWT until exp; subsequent CLI calls automatically re-auth on 401. CI usage: the user generates a fresh keypair per CI environment and runs login on each job (cheap).

**Stories Affected**: 8,9,10

**Related Questions**: [Questions not specified]

---

## D22: Database migrations run at startup behind a flag — 2026-05-13

**Context**: Cloud server is deployed continuously (Constitution IX). Migrations must apply on each release. Production safety requires a way to disable auto-apply in some environments.

**Question**: When and how do schema migrations run?

**Options Considered**:
Auto-run at server startup; run by a separate CLI command; run by deploy pipeline; manual DBA process

**Decision**: Migrations run automatically at server startup by default using sqlx-cli's embedded migrator (or refinery, TBD in implementation). The behavior is gated by an env var: MIDNIGHT_MANUAL_AUTO_MIGRATE (default 'true'). Production deployments may set it 'false' and run 'mnm db migrate' as a one-shot preflight in the deploy pipeline. Migrations are idempotent and applied in lexical order.

**Rationale**: Continuous release without manual migration steps matches the trunk-based-development principle. The opt-out env flag preserves the option for ops teams that prefer migration-as-preflight (e.g. to gate a deploy on migration success before rolling pods).

**Implications**:
Story 8 (CLI lifecycle): adds 'mnm db migrate' and 'mnm db status' commands. Story 9 (this story): server fails fast at startup on migration error (Constitution VI). All migrations are forward-only; rollback is via a new forward migration.

**Stories Affected**: 8,9

**Related Questions**: [Questions not specified]

---

## D23: CLI admin commands hidden from help by default — 2026-05-13

**Context**: The CLI carries two audiences: developers (who need 'mnm models pull', 'mnm mcp install', read-only inspection) and Midnight Network admins (who manage users, keys, ratelimits, ingest, versions). Polluting one audience's help output with the other's commands is friction.

**Question**: How do we keep admin commands invokable without showing them in default --help output?

**Options Considered**:
Single binary with --admin flag toggling visibility; separate admin binary (mnm-admin); env-gated visibility on a single binary; clap hidden flag on each admin subcommand always

**Decision**: Single binary; admin commands are invokable by name regardless, but are hidden from --help output by default. Visibility is controlled by (in precedence order, per D18): MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS env var, then 'cli.show_admin_cmds' boolean in config.toml, defaulting to false. Visibility never affects invocation behavior — 'mnm users list' works whether help is suppressed or not. Sub-help ('mnm users --help') always works once the user has named the command.

**Rationale**: Single binary keeps the install surface tight (one cargo install, one Homebrew formula) per Constitution IV. Hiding rather than disabling means admins who already know the commands aren't blocked, CI scripts don't break if the env isn't set, and curious developers running 'mnm users --help' can still discover the surface intentionally. Matches the pattern docker, kubectl, and gh use for less-common subcommands.

**Implications**:
Admin command set is enumerated in Story 8 (users, keys, ratelimits, versions promote/rollback/retire, sources add/update/retire, ingest md, ingest code, login, db migrate). Read-only and developer-facing commands stay visible: models, mcp install, doctor, sources list/show, versions list/show, config, search. Story 8 also adds a row to 'mnm doctor' output reporting whether admin mode is active.

**Stories Affected**: 8,5

**Related Questions**: [Questions not specified]

---

## D24: Confidence scoring: separate trust and relevance, blend, expose factors — 2026-05-13

**Context**: Story 1 invested in rich provenance metadata so callers can reason about trust. Story 6 needs to turn that metadata into a single number per result, alongside retrieval relevance, in a way that's tuneable and explainable.

**Question**: How is the per-result 'confidence' value computed, exposed, and used for ranking?

**Options Considered**:
(A) Single opaque score combining everything; (B) Two separate values (trust + relevance) reported separately; (C) Two separate values plus a blended 'confidence' and a per-factor breakdown; (D) Caller-supplied scoring formula

**Decision**: Option C. Cloud server computes a 'trust_score' from provenance (attribution, verification, freshness, deprecation, version-constraint match) and returns it with every search result alongside a 'confidence_factors' breakdown. The cloud also returns a default 'confidence' computed as a weighted geometric mean of trust_score and the RRF-derived relevance. The MCP server, when reranking is enabled (D2), recomputes 'confidence' substituting reranker_score for the relevance term. Results are sorted by confidence by default; callers can override with sort_by ∈ {confidence, trust, relevance} and filter with min_confidence ∈ [0,1]. The scoring policy (factor weights, freshness half-life, attribution multipliers) is loaded at startup from a TOML pointed at by MIDNIGHT_MANUAL_SCORING_POLICY; absence falls back to compiled defaults.

**Rationale**: Separation of trust and relevance prevents conflating 'I know this answer is from the Foundation' with 'this chunk matches the query well' — agents need both signals to behave well. The factor breakdown lets agents EXPLAIN their confidence, not just report it, which matches Constitution V (errors / decisions are actionable). Policy-as-TOML rather than code-baked weights means tuning is a config change, not a release. Defaulting to a weighted geometric mean (vs arithmetic) penalizes results that are weak on either axis (a high-relevance match with low trust gets a moderate confidence, not a high one).

**Implications**:
Story 1 schema is unchanged — all factors already live in document.provenance + source_version + ingested_at. Story 4 response shape gains trust_score, confidence, confidence_factors per result (additive — not a breaking contract change per Constitution I). Story 5 MCP search result gains the same; MCP server replaces relevance with reranker_score in the blend. Story 9 (cloud server ops) adds MIDNIGHT_MANUAL_SCORING_POLICY to required-secrets-or-defaults list.

**Stories Affected**: 1,4,5,6,9

**Related Questions**: [Questions not specified]

---

## D25: Multi-query bounds: max 10 queries per request, rate-limited per query — 2026-05-13

**Context**: D3 delegated query rewriting to callers via queries:string[]. Without bounds, an abusive caller could DoS the server with thousand-query requests. Without per-query rate-limit accounting, multi-query callers get an unfair advantage over single-query callers.

**Question**: How do we bound multi-query input and price it against rate limits?

**Options Considered**:
Unbounded queries with per-request rate cost; bounded queries with per-request cost; bounded queries with per-query rate cost; quadratic cost scaling

**Decision**: Max 10 queries per /v1/search request (configurable via MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST, hard ceiling 50). Each request consumes max(1, queries.length) tokens from the caller's rate-limit bucket — so a 5-query request costs 5 tokens. Sophisticated callers that genuinely benefit from multi-query pay proportional cost; casual callers using 1-3 queries see no functional change.

**Rationale**: Per-query cost makes the rate-limit fair: a 10-query power-user request shouldn't burn the same budget slot as a 1-query casual request when the DB work scales linearly. A 10-query cap protects the server from pathological inputs while leaving generous headroom for HyDE/multi-query/step-back patterns (typically 2-4 queries each). The configurable env var lets ops tune for traffic mix.

**Implications**:
Story 7 (this story): explicit max-queries check at request validation, 400 invalid_request when exceeded. Story 4: rate-limit middleware accepts a 'cost' parameter (default 1, multi-query routes pass queries.length). Story 9: rate_limit_override schema and FR-031 unchanged — the cost is applied to the same bucket. The cap doesn't apply to admin/write endpoints (they have their own model).

**Stories Affected**: 4,7,9

**Related Questions**: [Questions not specified]

---

## D26: Binary layout: one user-facing CLI (with MCP-serve subcommand) and one server-only binary — 2026-05-13

**Context**: D16 fixed two CLI binary names (midnight-manual + mnm alias). The MCP server and cloud server need to be addressed too. Distribution needs to know what's shipped to users vs what's only used in deploy.

**Question**: How many distinct binaries does the crate produce, what does each do, and which are shipped to users?

**Options Considered**:
Three user-facing binaries (cli + mcp + server); two user-facing (cli aliases) + one server only; one binary with command modes; separate crates per binary

**Decision**: Two distinct binaries from one crate. (1) midnight-manual / mnm (aliased) — the user-facing CLI; it serves admin commands, developer commands, AND the MCP server mode via 'mnm mcp serve' (stdio JSON-RPC). Shipped to users via cargo / Homebrew / GitHub Releases. (2) midnight-manual-server — the cloud HTTP server. Built into the Fly.io Docker image only; NOT shipped to user channels (no cargo install, no Homebrew formula).

**Rationale**: Two binaries is the minimum that cleanly separates 'thing running on a developer's laptop' from 'thing deployed to Fly'. Folding MCP-serve into the CLI binary avoids a third user-facing artifact and lets 'mnm mcp install' point at the same binary the user already has. Excluding midnight-manual-server from user channels prevents accidental local launches (which would need DATABASE_URL, secrets, etc.) and keeps the install surface tight per Constitution IV.

**Implications**:
Cargo.toml ships three [[bin]] entries: midnight-manual, mnm, midnight-manual-server. cargo install and the release pipeline only publish the first two for user channels. The Docker image built for Fly contains the server binary. Story 5 'mnm mcp serve' is the entry point that the agent config points at.

**Stories Affected**: 5,8,9,10

**Related Questions**: [Questions not specified]

---

## D27: Telemetry transport, storage, and lifecycle — 2026-05-13

**Context**: Constitution VII requires opt-out, anonymized telemetry that never includes query content / PII / tokens. We need a concrete shape: where do events go, how are they stored, how long are they kept?

**Question**: How is telemetry transmitted from CLI/MCP, stored on the cloud, and retired?

**Options Considered**:
Self-host (own cloud server endpoint); third-party SaaS (Plausible/PostHog); third-party self-hosted (Umami); no telemetry at all

**Decision**: Self-hosted on the same cloud server. CLI and MCP server POST batched NDJSON events to /v1/telemetry on the cloud (same Fly app, no separate deploy). The cloud stores per-event rows in Postgres with auto-deletion after 7 days (configurable via MIDNIGHT_MANUAL_TELEMETRY_RAW_RETENTION_DAYS) and aggregates rolling counters into per-day, per-event-type, per-component tables retained indefinitely. Events are anonymous (no user_id / IP-of-emitter tied to the event row beyond IP-rate-limit accounting which is separate). The /v1/telemetry endpoint accepts events at the anonymous rate-limit tier; opted-out clients send no events at all.

**Rationale**: Self-hosting avoids shipping user telemetry to a third-party SaaS — important for a developer-trust project. Same Fly app means no separate deploy story. Raw event retention bounded at 7 days satisfies privacy minimization (Constitution VII); aggregates let the team learn from longitudinal usage without per-event records. NDJSON batches keep the wire format trivial.

**Implications**:
Story 9 schema gains telemetry_event_raw (auto-deleted) and telemetry_aggregate_daily tables. Story 4 endpoint surface gains POST /v1/telemetry. Story 9 sweep job extended to delete telemetry rows past retention. Story 8 'mnm telemetry status' surfaces the endpoint URL and queue depth.

**Stories Affected**: 4,8,9,11

**Related Questions**: [Questions not specified]

---

## D28: Two-token credential model: distinct admin vs read-uplift, file-based storage, no keychain in v1 — 2026-05-13

**Context**: Story 5/8/9 conflated two credentials (admin JWT vs GitHub-SSO read-uplift bearer). The MCP subprocess can't read a keychain entry that 'mnm login' put there because the AI client passes no env vars. The Rust keyring crate is also flaky on macOS (unsigned binary re-prompts, signing requires Apple Developer cert).

**Question**: How do we store and resolve credentials so the CLI and MCP server share one source of truth without keychain pain?

**Options Considered**:
(A) Keychain default with file fallback; (B) File-only default with keychain opt-in; (C) File-only, no keychain in v1; (D) Status-quo conflated single-token model

**Decision**: Two distinct tokens, both stored in a single 0600-permissioned file ~/.config/midnight-manual/auth.toml. (1) Admin JWT: short-lived (1h, D21), used by interactive CLI write commands only. Flag: --admin-token. Env: MIDNIGHT_MANUAL_ADMIN_TOKEN. (2) Read-uplift bearer: long-lived (30 days, configurable via MIDNIGHT_MANUAL_READ_TOKEN_TTL_DAYS), used by MCP server and CLI read commands. Flag: --token. Env: MIDNIGHT_MANUAL_TOKEN. Both follow resolution order: explicit flag > env var > auth.toml entry > (fail or anonymous as appropriate). No keychain support in v1 — file-based only, matching gh, aws, kubectl, helm, flyctl conventions. The MCP server NEVER reads the admin token (smaller attack surface for a long-lived subprocess).

**Rationale**: (1) Two tokens with different TTLs solves the 'agent dies every hour' problem without weakening admin security. (2) File-based storage avoids the keyring crate's macOS code-signing pain and works in SSH/container/headless environments. (3) Distinct flag and env-var names per token prevent silent role-confusion (an admin token slipped into MIDNIGHT_MANUAL_TOKEN would be a privilege-escalation latent bug — making them distinct names makes this impossible). (4) The MCP server never holding admin credentials keeps the blast radius of MCP-process compromise to read uplift only. (5) Matches the industry norm for developer CLIs; the trust model (filesystem access = token access) is the same as keychain in practice.

**Implications**:
Story 5 FR-043 token resolution: --token > MIDNIGHT_MANUAL_TOKEN > auth.toml[read_uplift].token > anonymous. CLI write commands: --admin-token > MIDNIGHT_MANUAL_ADMIN_TOKEN > auth.toml[admin].token > fail 'run mnm login'. New CLI commands: 'mnm auth github' (OAuth flow, web with --no-browser device-flow fallback), 'mnm auth status'. mnm doctor reports auth.toml path, permission bits, and both token states (presence, expiry, user_id/github_login). Story 8 command tree gains 'auth' subtree (developer-visible — GitHub login is for all ecosystem developers). Story 9 FR-062 specifies the 30-day TTL for read-uplift bearers. EC-69 stays as a 'file is unreadable or corrupt' edge case rather than 'keychain unavailable'. The bearer_token_env knob in Story 5 config becomes redundant and can be removed.

**Stories Affected**: 5,8,9

**Related Questions**: [Questions not specified]

---

---
title: CLI reference
sidebar_label: CLI
---

# CLI reference

The `mnm` CLI (also installed as `midnight-manual`) is the command-line interface to the Midnight Manual corpus. This page documents the everyday subcommand tree, sourced from `crates/midnight-manual/src/cli.rs` and `crates/midnight-manual/src/commands/`. Operator and admin commands for running your own server are documented in [Operator & admin reference](/docs/self-hosting/operator-reference).

## Global flags

These flags are accepted by every subcommand:

| Flag | Env var | Description |
|---|---|---|
| `--config` | `MIDNIGHT_MANUAL_CONFIG` | Override the discovered config file path. |
| `--server` | `MIDNIGHT_MANUAL_SERVER` | Override the cloud server URL. |
| `--json` | — | Emit JSON on stdout instead of human-formatted text. |
| `--log-level` | `RUST_LOG` | Logging verbosity: `error`, `warn`, `info`, `debug`, `trace`. |
| `--no-telemetry` | — | Disable telemetry for this invocation. |
| `--voyage-api-key` | `VOYAGE_API_KEY` | Voyage API key for BYOK embedding (overrides env + config). |

---

## Subcommands

### `version`

Show the CLI version and build metadata.

---

### `doctor`

Diagnostic report covering auth state, corpus ingest summary, telemetry configuration, and environment health.

| Flag | Description |
|---|---|
| `--json` | Emit a single JSON object. |

---

### `status`

Connectivity, authentication, and model readiness check. Exits non-zero when the cloud is unreachable, so you can script it as a health probe.

---

### `search`

Ad-hoc retrieval: `mnm search "query"`.

**Query and output flags**

| Flag | Description |
|---|---|
| `query` (positional) | Primary query string. Required unless `--queries-stdin` is set. |
| `--query` | Additional query texts for multi-query retrieval (HyDE / expansion / step-back). Repeatable. |
| `--queries-stdin` | Read a JSON document `{"queries": [...]}` from stdin. Mutually exclusive with positional query and `--query`. |
| `--limit` | Maximum number of results (default 10, capped server-side at 100). |
| `--embedding-model` | Override the embedding-model wire id. When omitted (`auto`), the corpus's active model is fetched automatically. |

**Retrieval control flags**

| Flag | Description |
|---|---|
| `--mode` | Query mode: `hybrid` (default), `vector`, or `fts`. |
| `--code-mode` | Code-vector fusion mode: `on` (default for hybrid/vector), `off`, or `exclusive` (code vectors replace the general vector list). Incompatible with `--mode fts`. |
| `--rerank` | Where reranking runs: `auto` (default), `local` (BYOK Voyage), `server`, or `off`. |
| `--rerank-model` | Voyage rerank model: `rerank-2.5` or `rerank-2.5-lite` (faster, half tokens server-side). |
| `--rerank-instructions` | Natural-language rerank instruction (max 400 chars). Replaces the derived default. Keep it terse; instruction tokens multiply by pool size. |
| `--version-match` | Version-filter semantics: `permissive` (default) biases ranking; `strict` hard-filters. Only meaningful with a version-bearing filter. |

**Granular filter flags**

These narrow the candidate set before ranking. They are mutually exclusive with `--filter-json`.

| Flag | Description |
|---|---|
| `--kind` | Restrict to these chunk kinds (`markdown` \| `code` \| `plaintext`). Repeatable. |
| `--language` | Restrict to these programming languages. Repeatable. |
| `--exclude-language` | Exclude these languages. Repeatable. |
| `--tag` | Restrict to these tags. Repeatable. |
| `--exclude-tag` | Exclude these tags. Repeatable. |
| `--symbol` | Match symbols as `kind:name` (either side optional, e.g. `circuit:` or `:deployContract`). Repeatable. |
| `--source` | Restrict to these source slugs. Repeatable. |
| `--content-type` | Restrict to these content types. Repeatable. |
| `--attribution` | Restrict to these attributions. Repeatable. |
| `--no-deprecated` | Exclude deprecated content. |
| `--verified` | Restrict to verified content. |
| `--ingested-after` | Only chunks ingested on/after this ISO date (`YYYY-MM-DD`). |
| `--ingested-before` | Only chunks ingested on/before this ISO date (`YYYY-MM-DD`). |
| `--min-tokens` | Minimum chunk token count. |
| `--max-tokens` | Maximum chunk token count. |
| `--filter-json` | Full filter object as JSON. Mutually exclusive with the granular filter flags above. |

Run `mnm search --help` for the authoritative list.

See [Searching with the CLI](/docs/cli/searching) for usage patterns.

---

### `facets`

Print the corpus's filterable facets (retrieval modes and filter keys/values). No flags beyond the globals.

---

### `sources`

Source registry inspection.

| Subcommand | Description |
|---|---|
| `list` | List active sources from the cloud (anonymous read). |
| `show [slug]` | Show one source's metadata by slug (anonymous read). |

Admin operations (`create`, `update`, `retire`, `list-all`) are in [Operator & admin reference](/docs/self-hosting/operator-reference).

---

### `versions`

Source-version inspection.

| Subcommand | Description |
|---|---|
| `list <slug>` | List all source versions for a slug (anonymous read). |
| `show <slug> <revision>` | Show one source version by revision (anonymous read). |

Admin operations (`promote`, `rollback`, `retire`) are in [Operator & admin reference](/docs/self-hosting/operator-reference).

---

### `config`

Show the resolved configuration.

| Subcommand | Description |
|---|---|
| `show` | Print the resolved configuration (config file merged with defaults). Pass `--effective` to also layer env and global-flag overrides; secrets are redacted. |

---

### `mcp`

MCP server and related tooling: the subcommand AI clients invoke.

| Subcommand | Description |
|---|---|
| `serve` | Run the MCP server (long-running). Speaks stdio by default — this is the subcommand you add to your AI client config. Pass `--http` for stateless Streamable HTTP on `127.0.0.1:2400`, and `--bind <IP:PORT>` (requires `--http`; env `MIDNIGHT_MANUAL_MCP_BIND`) to move the listener. |

See [Add to an AI client](/docs/add-to-ai-client) and [How the MCP server works](/docs/mcp/how-it-works).

---

### `models`

Local model management and corpus-side model information.

| Subcommand | Description |
|---|---|
| `pull` | Ensure the local model-cache directory exists. Both the embedder and reranker are remote VoyageAI, so nothing is downloaded. Accepts `--cache-dir` to override the cache location. |
| `active` | Show the corpus's currently active embedding model. |

Admin operations (`status`, `migrate`) are in [Operator & admin reference](/docs/self-hosting/operator-reference).

---

### `auth`

GitHub OAuth read-uplift flow and local auth-file inspection.

| Subcommand | Description |
|---|---|
| `github` | Run the GitHub OAuth read-uplift flow. Flags: `--no-browser` (print the URL instead of opening it), `--dry-run` (don't persist the token), `--timeout [secs]` (listener bind timeout; default 300). |
| `status` | Show the state of your read-uplift token. |
| `logout` | Remove the read-uplift token from `auth.toml`. |

---

### `telemetry`

Telemetry opt-out toggle and status.

| Subcommand | Description |
|---|---|
| `disable` | Persistently disable telemetry on this machine. |
| `enable` | Re-enable telemetry (removes the persistent marker). |
| `status` | Show the resolved opt-out state. |
| `flush` | _(Hidden)_ Internal: drain the on-disk telemetry queue and exit. |

---

### `chunks`

Inspect corpus chunks directly.

| Subcommand | Description |
|---|---|
| `show [id]` | Fetch and render one chunk with bundled document and source context. |
| `next [id]` | Fetch the next N chunks after the anchor in the same document. |
| `prev [id]` | Fetch the previous N chunks before the anchor in the same document. |
| `neighbors [id]` | Fetch prev + anchor + next in one call. |

---

### `documents`

Inspect corpus documents.

| Subcommand | Description |
|---|---|
| `show [id]` | Render the document overview with the ordered chunk skeleton. |
| `chunks [id]` | Render a windowed slice of the document's chunks. |

---

### `manifest`

Manifest authoring and validation (local only, no network calls).

| Subcommand | Description |
|---|---|
| `init` | Write an empty starter manifest with comments. |
| `generate` | Populate a `hierarchy.yaml` from globs and an optional sitemap. |
| `check` | Validate a manifest locally: schema, paths, file existence. |

See [Authoring manifests](/docs/self-hosting/manifests).

---

### `skills`

Install, inspect, or remove the `midnight-advanced-search` skill.

| Subcommand | Description |
|---|---|
| `add` | Install (or update) the advanced-search skill. |
| `status` | Show where the skill is installed and whether it's current. |
| `remove` | Remove the advanced-search skill. |

---

## Operator & admin commands

Server-side commands (`keys`, `login`, `users`, `admin`, `ingest`, `ratelimits`, `tokenlimits`) are hidden from `--help` by default and documented in [Operator & admin reference](/docs/self-hosting/operator-reference).

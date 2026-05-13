# Quickstart — midnight-manual v1

**Feature**: 001-rag-platform | **Date**: 2026-05-13

Get the CLI + MCP server running locally against the deployed cloud server in five minutes. Three audiences are served by the same binary; pick the path that matches your role.

## Prerequisites

| Tool | Version | Why |
|---|---|---|
| Rust toolchain | stable ≥ 1.83 (MSRV) | Build the workspace |
| Docker (optional) | 24+ | Only if running integration tests via `testcontainers` or the local server |
| GitHub account | n/a | Required for read-uplift tier (higher rate limit); not required for anonymous read |
| Postgres 16 + pgvector (optional) | n/a | Only if running the cloud server locally |

## Install

Pick one channel. All three install the same `midnight-manual` and `mnm` binaries from the same release SHA (FR-095).

```bash
# Channel 1: cargo
cargo install midnight-manual

# Channel 2: Homebrew (macOS + Linux)
brew install midnight-network/tap/midnight-manual

# Channel 3: GitHub Release tarball (verify checksum)
curl -L https://github.com/midnight-network/midnight-manual/releases/download/v1.0.0/midnight-manual-v1.0.0-aarch64-apple-darwin.tar.gz -o mnm.tgz
tar xzf mnm.tgz
cd midnight-manual-v1.0.0-aarch64-apple-darwin
sha256sum -c SHA256SUMS
sudo install midnight-manual mnm /usr/local/bin/

# Verify
mnm version --json
# → {"version":"1.0.0","commit":"abc1234","build_date":"2026-05-13"}
```

## First-time setup (developer path)

You want to **use** the MCP server inside an AI client. No write access needed.

### 1. Download the local models

The MCP server runs a local embedding model and a reranker (~700 MB combined). The CLI manages model lifecycle (D12).

```bash
mnm models pull
# → 2 events on stderr (NDJSON in --json mode):
# {"type":"progress","model":"bge-base-en-v1.5","downloaded_mb":35,"total_mb":110}
# {"type":"progress","model":"bge-reranker-base","downloaded_mb":85,"total_mb":110}
# {"type":"summary","result":"ok","total_bytes":219000000,"took_ms":42312}

mnm models list
# → bge-base-en-v1.5    768 dims    active
#   bge-reranker-base   reranker    active
```

### 2. (Optional) Sign in for higher rate limits

Anonymous reads get a low per-IP rate limit. Authenticating via GitHub (your account must be in the Midnight Network org) lifts you to the per-user tier (D11, FR-117).

```bash
mnm auth github
# → Opens https://github.com/login/oauth/authorize?...
# → On completion: "logged in as @aaron-bassett, read-uplift token expires in 30 days"

# Headless / SSH path:
mnm auth github --no-browser
# → "Visit https://github.com/login/device and enter code: ABCD-1234"

mnm auth status
# → admin:        not logged in
#   read_uplift:  aaron-bassett, expires 2026-06-12T14:00:00Z
```

### 3. Install into your AI client

```bash
mnm mcp install --agent claude-code
# → Updated ~/Library/Application Support/Claude/claude_desktop_config.json
# → Added MCP server: midnight-manual → mnm mcp serve

# Other supported agents:
mnm mcp install --agent cursor
mnm mcp install --agent continue

# Unrecognized agent — prints the snippet for manual installation:
mnm mcp install --agent some-other-agent
```

Restart your AI client. It should now list `midnight-manual` in its tool catalog with seven tools (`search`, `get_chunk`, `get_chunk_siblings`, `get_chunk_parents`, `list_sources`, `pull_models`, `status`).

### 4. Verify

```bash
mnm doctor
# Cloud:           reachable (https://manual.midnight.network)
# Corpus model:    bge-base-en-v1.5@1
# Local embedding: bge-base-en-v1.5 ✓
# Local reranker:  bge-reranker-base ✓
# Model state:     ready
# MCP installation: claude-code ✓
# Auth file:       ~/.config/midnight-manual/auth.toml (0600)
#   admin:         not logged in
#   read_uplift:   aaron-bassett, expires in 29 days
# Admin commands:  hidden (default)
# Telemetry:       enabled
```

Done. Ask your agent to search for something — `"how do I compile a Compact contract?"` — and it should invoke `search` and return reranked, confidence-scored chunks with full citations.

## Maintainer path (Midnight Network staff)

You can **write** to the corpus. Requires an Ed25519 keypair registered in the deployed user store (D20).

### One-time keypair setup

```bash
mnm keys generate --user-id aaron
# → Writes ~/.config/midnight-manual/keys/aaron.{public,private} (private: 0600)
# → Prints the TOML row to paste into the user store:
#
#   [[users]]
#   user_id    = "aaron"
#   role       = "admin"
#   public_key = "ed25519:Ux9Az..."
#   created_at = "2026-05-13"
#   note       = "founding admin"
```

Submit the public key to the Midnight Network team (or update the deployed `MIDNIGHT_MANUAL_USER_STORE` Fly secret yourself if you have ops access). The change takes effect on the next server redeploy (D20).

### Authenticate and ingest

```bash
# Enable admin commands in --help (D23)
export MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1

mnm login --user-id aaron
# → Completes challenge-response; admin JWT written to auth.toml[admin] (1h TTL)

# Register a new source
mnm sources add midnight-docs \
    --kind docs_site \
    --display-name "Midnight Docs" \
    --origin-url https://github.com/midnightntwrk/midnight-docs

# Ingest a local checkout of midnight-docs
git clone https://github.com/midnightntwrk/midnight-docs ~/work/midnight-docs
mnm ingest md midnight-docs ~/work/midnight-docs \
    --manifest ~/work/midnight-docs/hierarchy.yaml \
    --source-url-prefix https://raw.githubusercontent.com/midnightntwrk/midnight-docs/main/ \
    --published-url-prefix https://docs.midnight.network/ \
    --dry-run
# → Preview; no writes to the cloud

mnm ingest md midnight-docs ~/work/midnight-docs --manifest ~/work/midnight-docs/hierarchy.yaml
# → Real ingest; resumes if interrupted

# Code ingest (Story 3)
mnm ingest code compact-examples ~/work/compact-examples
# Or pull straight from git:
mnm ingest code compact-examples --git https://github.com/OpenZeppelin/compact-contracts --ref main

# Version lifecycle
mnm versions list midnight-docs
mnm versions show midnight-docs 12

# Rollback to a previous version
mnm versions rollback midnight-docs
mnm versions promote midnight-docs --revision 11

# Hackathon mode: bump rate limit for a CIDR
mnm ratelimits add \
    --cidr 169.155.237.15/25 \
    --limit 200/s \
    --ttl 48h \
    --note "hackathon-london-2026"
```

## Operator path (running the cloud server)

You're deploying or maintaining the Fly.io app.

### Local development server

```bash
# Bring up local Postgres + pgvector
docker run -d --name mn-postgres -p 5432:5432 \
    -e POSTGRES_PASSWORD=dev \
    pgvector/pgvector:pg16

export DATABASE_URL=postgresql://postgres:dev@localhost:5432/postgres
export MIDNIGHT_MANUAL_JWT_SECRET=$(openssl rand -base64 32)
export MIDNIGHT_MANUAL_USER_STORE=$(cat dev-users.toml)
export MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_ID=...   # for GitHub flow testing
export MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET=...
export MIDNIGHT_MANUAL_GITHUB_ORG=midnight-network

# Build and run the server
cargo run --release --bin midnight-manual-server
# → Server listening on 0.0.0.0:8080
# → /healthz: 200; /readyz: 200 (DB reachable, user store loaded)

# Or via mn-cli's debug helper
mnm --server http://localhost:8080 search "compile compact contract"
```

### Deploy to Fly.io

```bash
fly launch --image ghcr.io/midnight-network/midnight-manual:v1.0.0
fly secrets set \
    MIDNIGHT_MANUAL_JWT_SECRET=$(openssl rand -base64 32) \
    MIDNIGHT_MANUAL_USER_STORE="$(cat users.toml)" \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_ID=$GH_CLIENT_ID \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET=$GH_CLIENT_SECRET \
    MIDNIGHT_MANUAL_GITHUB_ORG=midnight-network
fly deploy
```

Continuous release is automatic on merge to `main` — see `.github/workflows/release.yml`.

## Development workflow (contributor)

```bash
git clone https://github.com/midnight-network/midnight-manual
cd midnight-manual

# Set up local tooling (see Phase 2 of plan.md)
rustup component add rustfmt clippy
cargo install sqlx-cli --no-default-features --features postgres,native-tls

# Format, lint, test
just check           # cargo fmt --check && cargo clippy -- -D warnings && cargo test
just check-msrv      # same against MSRV toolchain
just bench           # criterion benchmarks for scoring + RRF

# Database migrations
just migrate-up
just migrate-status

# Integration tests (boots ephemeral Postgres+pgvector via testcontainers)
just test-integration

# Canary tests (CI gate; FR-112 / SC-061)
just test-canary
```

## Common commands reference

| Command | Audience | Purpose |
|---|---|---|
| `mnm search "..."` | dev | Hit the cloud search directly (debug) |
| `mnm models pull` / `list` / `prune` | dev | Manage local ML models |
| `mnm mcp install --agent <name>` | dev | Wire MCP into an AI client config |
| `mnm auth github` | dev | Get read-uplift bearer |
| `mnm doctor [--json]` | dev | Diagnostic report |
| `mnm telemetry status` / `disable` / `enable` | dev | Inspect/control telemetry |
| `mnm config show --effective` | dev | Show resolved config (env > flag > file > default) |
| `mnm login --user-id <id>` | admin | Get admin JWT (1h) |
| `mnm logout` | admin | Clear admin JWT only |
| `mnm sources add/update/retire` | admin | Manage sources |
| `mnm ingest md <slug> <path>` | admin | Ingest Markdown |
| `mnm ingest code <slug> <path>` | admin | Ingest source code |
| `mnm versions promote/rollback/retire` | admin | Manage source_version lifecycle |
| `mnm users add/list/show/update/remove` | admin | Edit local user-store TOML (D20) |
| `mnm keys generate` | admin | Generate Ed25519 keypair |
| `mnm ratelimits add/list/extend/remove` | admin | CIDR override CRUD |
| `mnm db migrate` / `status` | admin (ops) | Preflight migrations when auto-migrate is off |

## Privacy

This tool is opt-out telemetry by default. We collect 6 event types of coarse-grained scalars (no query content, no chunk content, no tokens, no file paths, no env values, no PII). Retention: 7 days raw, aggregates retained.

**Disabling telemetry**:
```bash
mnm telemetry disable           # writes config
# or
export MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1
```

See README's "Telemetry & Privacy" section for the full list of collected fields per event type, the forbidden-data set, and the canary tests that mechanically enforce these promises (FR-112, SC-061).

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `search` returns `embedding_model_mismatch` | Corpus migrated to a new embedding model | Run `mnm models pull` (or invoke the `pull_models` MCP tool from inside your agent) |
| MCP server logs `models_missing` | Never ran `mnm models pull` | Same |
| `mnm login` fails with `nonce_expired` | Network latency or clock skew | Retry; nonces have a 60s TTL |
| All requests return 429 | Anonymous rate-limit cap reached | Sign in: `mnm auth github` |
| `mnm doctor` reports `cloud: unreachable` | Network / firewall | Check `--server` URL or proxy config |
| Telemetry events not visible in /metrics | Opted out or pre-flush | Check `mnm telemetry status` |

Full error code reference: `specs/001-rag-platform/contracts/openapi.yaml` (the `Error.code` enum) and `specs/001-rag-platform/spec.md` (Story 4 error code table).

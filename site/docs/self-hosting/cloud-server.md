---
title: Cloud server & deploy
sidebar_label: Cloud server & deploy
description: Architecture and full provisioning runbook for midnight-manual-server on Fly.io — Postgres, secrets, OAuth, VoyageAI, and first deploy.
---

# Cloud server & deploy

`midnight-manual-server` is the corpus host. Most people never run it; they use the hosted instance. It is a single self-contained binary if you want your own.

## Architecture

- **Stack:** `axum` + `tower` over PostgreSQL 16 with the `pgvector` extension. An HNSW index powers vector search; a GIN index powers full-text.
- **API surface:** anonymous **read** endpoints (`/v1/search`, `/v1/embeddings` proxy, `/v1/facets`, `/v1/chunks/{id}` + batch variants + `/next`/`/prev`/`/parents`, `/v1/documents/{id}` + `/chunks`, `/v1/sources` + `/{slug}` + `/versions`, `/v1/models/active`, `/v1/me`) and authenticated **admin** endpoints (the ingest-run protocol, version promote/retire, rate-limit and token-limit management).
- **Auth:** Ed25519 challenge-response for admin principals; GitHub OAuth for read-uplift tokens; HS256 JWTs signed with `MIDNIGHT_MANUAL_JWT_SECRET`.
- **Tiered rate limiting.** Anonymous traffic is limited per-IP. Signing in via GitHub OAuth raises the limit for 30 days. Admins can add per-CIDR overrides. A tier guard runs before the role guard, so a read-uplift token can never gain write access.
- **Health endpoints:** `/healthz` (liveness) and `/readyz` (readiness after the DB pool and model registry are loaded). Request-ID propagation on every request for traceability.
- **Image:** Multi-stage Docker build onto `gcr.io/distroless/cc-debian12` (no shell, no toolchain), built for `linux/amd64` and `linux/arm64`. Server deploys are always operator-run, not wired into CI.

## Running locally against Postgres

```bash
export DATABASE_URL=postgres://localhost/midnight_manual
export MIDNIGHT_MANUAL_USER_STORE=./users.toml
export MIDNIGHT_MANUAL_JWT_SECRET=…     # HS256 signing secret, ≥ 32 bytes
cargo run --release -p midnight-manual-server
```

The server runs automatic migrations at boot when `MIDNIGHT_MANUAL_AUTO_MIGRATE=true` (the default in `fly.toml`). Migrations live in `crates/mnm-store/migrations/`.

For prerequisites and the full list of binaries and Cargo features, see [Building from source](./building-from-source.md).

## Fly.io provisioning runbook

The `fly.toml` at the repo root configures the app for Fly.io deployment. Primary region is `lhr` (London). The server image is built from `Dockerfile.server`.

**Server deploys are always operator-run** (`flyctl deploy`). They are intentionally not wired into CI. The release pipeline (`release-plz`) publishes crates and CLI binaries but does not touch the server image.

### Prerequisites

```bash
brew install flyctl gh
flyctl auth login
gh auth login
```

You also need: write access to the GitHub repo (for OAuth App configuration), a Fly.io org with billing, and a VoyageAI account.

### 1. Create the Fly app

```bash
flyctl apps create midnight-manual --org <your-org-slug>
```

The app name `midnight-manual` matches `fly.toml`. Use a different name only if you also edit `fly.toml`.

### 2. Provision Managed Postgres + pgvector

Use Fly **Managed Postgres** (`fly mpg`). The `--pgvector` flag enables the extension for you:

```bash
flyctl mpg create \
    --name midnight-manual-pg \
    --org <your-org-slug> \
    --region lhr \
    --plan basic \
    --volume-size 10 \
    --pgvector
```

Attach the cluster to the app, which sets `DATABASE_URL` automatically:

```bash
flyctl mpg list
flyctl mpg attach <cluster-id> --app midnight-manual
```

### 3. Generate the JWT signing secret

```bash
openssl rand -hex 32 | tr -d '\n' | \
    flyctl secrets set MIDNIGHT_MANUAL_JWT_SECRET=- --stage --app midnight-manual
```

`--stage` queues the secret without restarting machines yet.

### 4. Author and stage the user-store TOML

Generate a keypair for your first admin principal:

```bash
mnm keys generate --user-id ops-primary
```

This writes `$XDG_CONFIG_HOME/midnight-manual/keys/ops-primary.private` (mode `0600`) and echoes the public key to stdout. Build `user-store.toml`:

```toml
schema_version = 1

[[users]]
user_id    = "ops-primary"
role       = "admin"
public_key = "ed25519:<base64 from mnm keys generate>"
created_at = "2026-05-25"
```

Keep `user-store.toml` and all `.private` files out of git. Stage it as a Fly secret:

```bash
flyctl secrets set MIDNIGHT_MANUAL_USER_STORE="$(cat user-store.toml)" \
    --stage --app midnight-manual
```

See [Users & access](./users-access.md) for the full user-management reference.

### 5. Register the GitHub OAuth App

GitHub OAuth provides read-uplift tokens for members of your configured org. Skip this step if you do not need the read-uplift tier.

1. Create an OAuth App at `https://github.com/settings/applications/new`.
2. Set **Authorization callback URL** to `https://midnight-manual.midnightntwrk.expert/v1/auth/github/callback` (match your cert hostname).

```bash
flyctl secrets set \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_ID=<client-id> \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET=<client-secret> \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_REDIRECT_URL=https://midnight-manual.midnightntwrk.expert/v1/auth/github/callback \
    MIDNIGHT_MANUAL_GITHUB_ORG=midnight-network \
    --stage --app midnight-manual
```

If any of these four secrets is missing the `/v1/auth/github/*` endpoints return 503 cleanly. A partial configuration is safe.

### 6. Set the VoyageAI key

```bash
flyctl secrets set VOYAGE_API_KEY=<voyage-platform-key> \
    --stage --app midnight-manual
```

Without `VOYAGE_API_KEY`, `/v1/embeddings` returns **503** and inline reranking degrades to RRF order. The rest of the server still boots and serves reads.

Before pointing real traffic at the proxy, enable **zero-retention** on the Voyage account whose key the server uses: training disabled, no data retention. Non-BYOK callers' query text flows through this account, so zero-retention is what keeps it from being retained upstream.

### 7. Deploy

```bash
flyctl deploy --app midnight-manual
flyctl logs --app midnight-manual
```

Expected boot sequence in the logs: "resolved active embedding model" -> "starting midnight-manual-server" -> migrations applied -> listener bound on `:8080`.

### 8. Smoke test

```bash
HOST=https://midnight-manual.midnightntwrk.expert

curl -fs "$HOST/healthz"      # → 200
curl -fs "$HOST/readyz"       # → 200 once DB + model registry ready

curl -fs "$HOST/v1/models/active" | jq .
# → {"name":"voyage-context-3","revision":1,"dim":1024,"provider":"voyageai",
#    "code":{"name":"voyage-code-3","revision":1,"dim":1024,"provider":"voyageai"}}
```

## Environment variable reference

The following variables configure server behaviour. Secrets should be set via `flyctl secrets set`; non-secrets belong in `fly.toml`'s `[env]` block.

| Variable | Secret? | Default | Description |
|---|---|---|---|
| `DATABASE_URL` | yes | (set by Fly attach) | PostgreSQL connection string. |
| `MIDNIGHT_MANUAL_JWT_SECRET` | yes | (required) | HS256 signing secret, ≥ 32 bytes. |
| `MIDNIGHT_MANUAL_USER_STORE` | yes | (required) | TOML user-store body (server) or file path (CLI). |
| `VOYAGE_API_KEY` | yes | (unset) | Voyage platform key. Without it, `/v1/embeddings` returns 503. |
| `MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_ID` | yes | (unset) | GitHub OAuth client ID for read-uplift. |
| `MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET` | yes | (unset) | GitHub OAuth client secret. |
| `MIDNIGHT_MANUAL_GITHUB_OAUTH_REDIRECT_URL` | yes | (unset) | OAuth callback URL. |
| `MIDNIGHT_MANUAL_GITHUB_ORG` | yes | (unset) | GitHub org whose members get read-uplift. |
| `MIDNIGHT_MANUAL_AUTO_MIGRATE` | no | `true` | Run migrations at boot. |
| `MIDNIGHT_MANUAL_VOYAGE_CONTEXT_MODEL` | no | `voyage-context-3` | Contextualized embedding model (general chunks). |
| `MIDNIGHT_MANUAL_VOYAGE_MODEL` | no | `voyage-code-3` | Flat embedding model (code chunks + `/v1/embeddings` proxy). |
| `MIDNIGHT_MANUAL_VOYAGE_DIM` | no | `1024` | Embedding output dimension. |
| `MIDNIGHT_MANUAL_VOYAGE_DTYPE` | no | `float` | Embedding output dtype. |
| `MIDNIGHT_MANUAL_SERVER_RERANK` | no | (enabled) | Set to `off` to disable inline reranking. |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_ANON_HOURLY` | no | `2000` | Hourly token budget, anonymous tier. |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_ANON_DAILY` | no | `20000` | Daily token budget, anonymous tier. |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_UPLIFT_HOURLY` | no | `4000` | Hourly token budget, read-uplift tier. |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_UPLIFT_DAILY` | no | `40000` | Daily token budget, read-uplift tier. |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_ADMIN_HOURLY` | no | `500000` | Hourly token budget, admin tier. |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_ADMIN_DAILY` | no | `100000000` | Daily token budget, admin tier. |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_GLOBAL` | no | `10000000` | Site-wide token ceiling (non-admin tiers). `u64::MAX` disables it. |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_GLOBAL_WINDOW_SECS` | no | `10800` | Rolling window for the global ceiling (3 h). |
| `MIDNIGHT_MANUAL_TOKEN_SNAPSHOT_SECS` | no | `300` | Interval for flushing token-usage counters (5 min). |
| `MIDNIGHT_MANUAL_SCORING_POLICY` | yes (optional) | (compiled defaults) | Custom confidence-scoring policy TOML. |
| `RUST_LOG` | no | `info` | Log level. |

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `503` on `/v1/auth/challenge` or `/v1/auth/verify` | `MIDNIGHT_MANUAL_USER_STORE` or `MIDNIGHT_MANUAL_JWT_SECRET` unset. |
| `503` on `/v1/auth/github/*` | Any of the four `MIDNIGHT_MANUAL_GITHUB_*` secrets missing. |
| `409 embedding_model_mismatch` from a CLI | Client embedding-model id does not match the active corpus model. Run `mnm models active` to see the corpus's active wire id, then re-run with `--embedding-model <wire-id>` (or `--embedding-model auto`); use `mnm models migrate` to realign every source in bulk. |
| `relation "chunk" does not exist` in logs | `pgvector` extension missing or `MIDNIGHT_MANUAL_AUTO_MIGRATE=false` and migrations not applied. |
| `failed to resolve active embedding model` at boot | `embedding_model` table empty: migration `0006_seed_embedding_model.sql` did not apply. |

## Related pages

- [When to self-host](./when-to-self-host.md) — deciding whether you need your own server.
- [Users & access](./users-access.md) — generating keypairs, managing the user store, and authenticating.
- [Running an ingest](./running-an-ingest.md) — populating the corpus after first deploy.
- [Versions & rate limits](./versions-rate-limits.md) — managing corpus versions and per-CIDR overrides.

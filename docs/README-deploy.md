# Deploy runbook: midnight-manual-server on Fly.io

This is the first-time deploy runbook for the cloud server (`mn-server` →
`midnight-manual-server`). Once the infrastructure is provisioned, ongoing
releases are automatic: merging a `release-please` PR cuts a tag, the release
workflow builds a multi-arch Docker image, pushes to GHCR, and `flyctl deploy`s
it.

Everything in this runbook is **operator-executed**. The repo can't provision
real infrastructure for you; this document just enumerates every command in the
order it needs to run, with the rationale for each step.

## Prerequisites

Install the CLIs on the workstation running this runbook:

```bash
brew install flyctl gh                 # or your platform's package manager
brew install rustup-init && rustup default 1.91.0
docker --version                       # 24+ recommended
```

Authenticate:

```bash
flyctl auth login
gh auth login
```

You will also need:

- Write access to the GitHub repo (for OAuth-App and Secrets configuration).
- A Fly.io org with billing attached (Fly Postgres + Machines have a small
  cost on `shared-cpu-1x`/1 GB).
- A Hugging Face account (anonymous downloads are fine for `bge-base-en-v1.5`
  and `bge-reranker-base`; an HF token only helps if you hit anonymous rate
  limits during the initial model pull).

## 1. Create the Fly app

```bash
# From the repo root.
flyctl apps create midnight-manual \
    --org <your-org-slug>
```

The app name `midnight-manual` matches `fly.toml:4`. Use a different name only
if you also edit `fly.toml`.

## 2. Provision Fly Postgres + pgvector

```bash
# Create a managed Postgres cluster.
flyctl postgres create \
    --name midnight-manual-pg \
    --org <your-org-slug> \
    --region lhr \
    --initial-cluster-size 1 \
    --vm-size shared-cpu-1x \
    --volume-size 10

# Attach it to the app — this sets DATABASE_URL as a secret automatically.
flyctl postgres attach midnight-manual-pg --app midnight-manual

# Enable the pgvector extension on the cluster.
# (Connect with `flyctl postgres connect -a midnight-manual-pg` then run:)
#     CREATE EXTENSION IF NOT EXISTS vector;
flyctl postgres connect --app midnight-manual-pg \
    --command "CREATE EXTENSION IF NOT EXISTS vector;"
```

The server runs migrations at boot (`MIDNIGHT_MANUAL_AUTO_MIGRATE=true` in
`fly.toml`), so once the extension exists the 6 numbered migrations under
`crates/mn-store/migrations/` will apply on first deploy.

## 3. Generate the JWT signing secret

The HS256 secret needs to be ≥ 32 bytes.

```bash
openssl rand -hex 32 | tr -d '\n' | \
    flyctl secrets set MIDNIGHT_MANUAL_JWT_SECRET=- --stage --app midnight-manual
```

`--stage` queues the secret without restarting machines yet; we'll deploy
everything in one batch at the end.

## 4. Author the admin user-store TOML

The user-store gates admin endpoints (source CRUD, rate-limit overrides, etc.).
Schema: `schema_version = 1` with `[admin]` and `[read_uplift]` sections. The
admin's ed25519 public key authenticates challenge requests (`POST
/v1/auth/admin/challenge` → signed nonce → `/verify` → JWT).

Generate an admin keypair locally:

```bash
mnm keys generate --label "ops-primary" --out ./ops-primary.toml
# This writes both the private key (kept locally, chmod 0600) and prints the
# public-key block for the user-store.
```

Author `user-store.toml`:

```toml
schema_version = 1

[admin]
[admin.principals.ops-primary]
ed25519_pubkey_base64 = "<paste from `mnm keys generate` output>"

[read_uplift]
github_org = "midnight-network"     # any member of this org can request a
                                    # read-uplift bearer via GitHub OAuth
```

Stage it as a secret (Fly stores the verbatim TOML body):

```bash
flyctl secrets set MIDNIGHT_MANUAL_USER_STORE="$(cat user-store.toml)" \
    --stage --app midnight-manual
```

Keep `user-store.toml` and `ops-primary.toml` **out of git**.

## 5. Register the GitHub OAuth App

Used by FR-062: members of the configured GitHub org can exchange an OAuth
flow for a read-uplift JWT that bumps their rate-limit tier.

1. <https://github.com/settings/applications/new> (or
   `https://github.com/organizations/<org>/settings/applications/new` for an
   org-owned OAuth App).
2. **Application name**: `midnight-manual` (anything works).
3. **Homepage URL**: `https://midnight-manual.fly.dev` (or your custom domain
   once configured — see step 8).
4. **Authorization callback URL**:
   `https://midnight-manual.fly.dev/v1/auth/github/callback`.
5. Generate a client secret on the next screen; copy both values.

```bash
flyctl secrets set \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_ID=<client-id> \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET=<client-secret> \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_REDIRECT_URL=https://midnight-manual.fly.dev/v1/auth/github/callback \
    MIDNIGHT_MANUAL_GITHUB_ORG=midnight-network \
    --stage --app midnight-manual
```

If any one of these four is missing the `/v1/auth/github/*` endpoints return
503 cleanly (`app.rs` build-time gate), so a half-configured deploy is safe.

## 6. (Optional) Custom scoring policy

The compiled-in confidence policy (US6/D24) is the recommended default. If you
want to tune weights, ship a TOML file via secret:

```bash
flyctl secrets set MIDNIGHT_MANUAL_SCORING_POLICY="$(cat scoring-policy.toml)" \
    --stage --app midnight-manual
```

The server fails startup on a malformed policy (Constitution VIII fail-fast),
so test it locally first with `cargo test -p mn-core scoring_policy`.

## 7. First deploy

Two paths:

### 7a. Direct (skip release-please for the first cut)

```bash
flyctl deploy --app midnight-manual
```

This builds the Docker image from `Dockerfile.server` on Fly's remote builder
and rolls out the machine. Watch the logs:

```bash
flyctl logs --app midnight-manual
```

Expected sequence: "resolved active embedding model" → "starting
midnight-manual-server" → migrations applied → listener bound on `:8080`.

### 7b. Via the release pipeline (recommended for repeatable deploys)

Push to `main` triggers `release-please` to open a release PR. Merge it; the
release workflow tags the commit, builds the multi-arch image, pushes to GHCR
(`ghcr.io/<owner>/midnight-manual:vX.Y.Z`), and runs `flyctl deploy --image
<that tag>`.

Either way, the very first run needs `FLY_API_TOKEN` as a GitHub Actions
secret on the repo:

```bash
flyctl tokens create deploy --app midnight-manual | \
    gh secret set FLY_API_TOKEN --repo <owner>/midnight-manual
```

## 8. Smoke test

Once the machine is up, verify the basics:

```bash
HOST=https://midnight-manual.fly.dev

# Liveness + readiness.
curl -fs "$HOST/healthz"      # → 200 "ok"
curl -fs "$HOST/readyz"       # → 200 once DB pool + model registry are loaded

# Active embedding model (FR-039).
curl -fs "$HOST/v1/models/active" | jq .
# → {"name":"bge-base-en-v1.5","revision":1,"dim":768,"provider":"baai"}

# Search against an empty corpus — should 200 with no results.
curl -fs -X POST "$HOST/v1/search" \
    -H 'content-type: application/json' \
    -d '{"query":"hello","vector":'"$(jq -n '[range(0;768)|0]')"',"client_embedding_model":"bge-base-en-v1.5@1","limit":5}' \
    | jq '.results | length'
# → 0
```

## 9. Ingest a corpus

The corpus is initially empty. Two options:

### 9a. Ingest the sample scaffold (smoke test)

```bash
# Create the source row first.
mnm sources create \
    --slug sample \
    --kind docs-site \
    --display-name "Sample" \
    --retention-count 3 \
    --server "$HOST"

# Ingest the sample fixture under corpus/sample/.
mnm ingest corpus/sample/hierarchy.yaml \
    --source-slug sample \
    --revision "$(git rev-parse --short HEAD)" \
    --server "$HOST"
```

This pushes a few placeholder docs through the pipeline end to end — embed,
chunk, upload, finalize. The sample is intentionally trivial; it proves the
pipeline works, not the corpus content.

### 9b. Ingest a real Midnight-docs repo

Author or clone the upstream `midnight-docs` repo, add a `hierarchy.yaml`
manifest at the root (schema: `crates/mn-content/src/manifest.rs`), then:

```bash
mnm sources create \
    --slug midnight-docs \
    --kind docs-site \
    --display-name "Midnight Docs" \
    --retention-count 5 \
    --server "$HOST"

mnm ingest /path/to/midnight-docs/hierarchy.yaml \
    --source-slug midnight-docs \
    --revision "$(cd /path/to/midnight-docs && git rev-parse --short HEAD)" \
    --note "first production ingest" \
    --server "$HOST"
```

`--dry-run` first if you want the plan before any writes.

## 10. (Optional) Custom domain + TLS

```bash
flyctl certs create midnight-manual.example.com --app midnight-manual
# Follow the DNS instructions; Fly handles the ACME flow.
```

Then update the GitHub OAuth App callback URL and the
`MIDNIGHT_MANUAL_GITHUB_OAUTH_REDIRECT_URL` secret to match.

## 11. (Optional) Alerting

The server emits Prometheus metrics on `GET /metrics`. Point Grafana or your
metrics collector at it; useful starting alerts:

- `up{job="midnight-manual"} == 0` for 2m → page on-call.
- `histogram_quantile(0.95, rate(http_request_duration_seconds_bucket{route="/v1/search"}[5m])) > 0.5`
  → SC-013 budget breach.
- `rate(http_requests_total{status=~"5.."}[5m]) > 0.05` → 5% server error rate.

Dashboards/alert YAML aren't checked into this repo yet — see the open
operational gaps in the project's production-readiness audit.

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| `503 service_unavailable` on `/v1/auth/admin/*` | `MIDNIGHT_MANUAL_USER_STORE` or `MIDNIGHT_MANUAL_JWT_SECRET` unset. |
| `503` on `/v1/auth/github/*` | Any of the four `MIDNIGHT_MANUAL_GITHUB_*` secrets missing. |
| `409 embedding_model_mismatch` from a CLI | Client embedding-model id doesn't match the active corpus model. Run `mnm models pull` and retry. |
| `relation "chunk" does not exist` in server logs | pgvector extension missing (step 2 not run) or migrations disabled (`MIDNIGHT_MANUAL_AUTO_MIGRATE=false`). |
| `failed to resolve active embedding model` at boot | The `embedding_model` table is empty. Migration `0006_seed_embedding_model.sql` should have populated it — confirm the migration ran. |

## What's not yet covered

These are tracked as open production gaps:

- Backup / restore runbook (Fly Postgres supports snapshots, but the
  recipe isn't here yet).
- Grafana dashboard JSON / Prometheus alert rules.
- Recall + load benchmark gates (`tests/recall/`, `tests/load/`).

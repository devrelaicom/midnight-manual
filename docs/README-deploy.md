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

## 2. Provision the custom domain + TLS cert

The production URL is `https://midnight-manual.midnightntwrk.expert`. Fly's
default `midnight-manual.fly.dev` hostname keeps working alongside it, so this
step is safe to start in parallel with the rest of provisioning — DNS
propagation and cert issuance can happen while you work on Postgres + secrets.

```bash
flyctl certs create midnight-manual.midnightntwrk.expert --app midnight-manual
```

Flyctl prints the DNS records you need to set at whoever hosts
`midnightntwrk.expert`. Typically two:

| Record | Value |
| --- | --- |
| `CNAME midnight-manual.midnightntwrk.expert` | `midnight-manual.fly.dev` |
| `TXT  _acme-challenge.midnight-manual.midnightntwrk.expert` | `<token from flyctl>` |

Set those at the DNS provider, then poll until the cert is `Issued`:

```bash
flyctl certs show midnight-manual.midnightntwrk.expert --app midnight-manual
```

A few minutes is typical once DNS resolves; up to ~30 minutes if the registrar
has slow TTLs. `force_https = true` in `fly.toml` handles the redirect on both
the custom domain and the Fly default once the cert is in place.

You can defer this step if you like — the server is reachable via
`midnight-manual.fly.dev` until the cert issues. But the GitHub OAuth App
callback URL (step 6 below) has to match the *final* host, so it's cleaner to
have the cert in flight before you register the OAuth App.

## 3. Provision Managed Postgres + pgvector

Use Fly **Managed Postgres** (`fly mpg`), not the legacy unmanaged
`flyctl postgres create`. Fly explicitly will not support the unmanaged path
anymore; the `--pgvector` flag on `mpg create` enables the extension for you
so there's no separate `CREATE EXTENSION` step.

```bash
flyctl mpg create \
    --name midnight-manual-pg \
    --org <your-org-slug> \
    --region lhr \
    --plan basic \
    --volume-size 10 \
    --pgvector
```

Plan trade-offs (current Fly MPG pricing):

| Plan | Spec | Monthly | When to pick it |
| --- | --- | --- | --- |
| **basic** | shared-2x · 1 GB RAM | ~$38 | first deploy, small corpus (≲ 50k chunks) |
| starter | shared-2x · 2 GB RAM | ~$72 | safer for a full midnight-docs corpus |
| launch | performance-2x · 8 GB RAM | ~$282 | production scale |

Storage is metered separately at ~$0.28/GB-month, so 10 GB ≈ $2.80/mo on top.
Start at `basic` and bump if you hit memory pressure once the HNSW index is
warm.

Then attach the cluster to the app — this sets `DATABASE_URL` on the app's
secrets automatically.

```bash
# Cluster IDs come from `fly mpg list`; the attach command takes the ID, not
# the name.
flyctl mpg list
flyctl mpg attach <cluster-id> --app midnight-manual

# Sanity: DATABASE_URL now exists on the app.
flyctl secrets list --app midnight-manual | grep DATABASE_URL
```

Confirm pgvector is enabled (optional but cheap):

```bash
flyctl mpg connect <cluster-id>
# psql prompt:
#   SELECT extname, extversion FROM pg_extension WHERE extname = 'vector';
# → one row, version 0.7.x or higher.
```

The server runs migrations at boot (`MIDNIGHT_MANUAL_AUTO_MIGRATE=true` in
`fly.toml`), so the 6 numbered migrations under
`crates/mn-store/migrations/` apply on first deploy.

## 4. Generate the JWT signing secret

The HS256 secret needs to be ≥ 32 bytes.

```bash
openssl rand -hex 32 | tr -d '\n' | \
    flyctl secrets set MIDNIGHT_MANUAL_JWT_SECRET=- --stage --app midnight-manual
```

`--stage` queues the secret without restarting machines yet; we'll deploy
everything in one batch at the end.

## 5. Author the admin user-store TOML

The user-store is the roster of human/CI principals who authenticate via
Ed25519 challenge-response (FR-057). Each row binds a stable `user_id` to a
public key and a role. The server loads it once at boot from
`MIDNIGHT_MANUAL_USER_STORE` and never mutates it at runtime; updating the
roster is a "edit local file → `flyctl secrets set` → redeploy" flow.

GitHub-OAuth *read-uplift* bearers are a separate concept handled by the
`MIDNIGHT_MANUAL_GITHUB_*` secrets in step 6 — they do **not** appear in
`users.toml`.

Two roles ship in v1:

| Role | What it can do |
| --- | --- |
| `admin` | full surface incl. `/v1/admin/*` (source CRUD, rate-limit overrides) |
| `writer` | ingest writes + reads; cannot reach `/v1/admin/*` |

### Mint a keypair

```bash
mnm keys generate --user-id ops-primary
```

This writes the 32-byte signing seed to
`$XDG_CONFIG_HOME/midnight-manual/keys/ops-primary.private` (mode `0600` on
Unix) — the operator does **not** choose the path. The public half is echoed
to stdout in `users.toml` wire form, ready to paste. Add more principals (a
CI bot, a co-maintainer, …) by rerunning with a different `--user-id` per
principal.

Useful flags: `--dry-run` (print + intended write path, touch nothing),
`--force` (overwrite an existing `<user_id>.private` — refuses by default).

### Build `user-store.toml` around the printed row

```toml
schema_version = 1

[[users]]
user_id    = "ops-primary"
role       = "admin"
public_key = "ed25519:<base64 from `mnm keys generate` output>"
created_at = "2026-05-25"
# note     = "optional human note"

# Add more rows by appending another [[users]] block.
```

Unknown fields and duplicate `user_id` values are rejected at load time
(FR-057 fail-fast); the server boots with `auth = None` (and the
`/v1/auth/admin/*` endpoints 503) if the secret is unset or malformed.

### Stage as the Fly secret

```bash
flyctl secrets set MIDNIGHT_MANUAL_USER_STORE="$(cat user-store.toml)" \
    --stage --app midnight-manual
```

Keep `user-store.toml` and every `<user_id>.private` **out of git**.

## 6. Register the GitHub OAuth App

Used by FR-062: members of the configured GitHub org can exchange an OAuth
flow for a read-uplift JWT that bumps their rate-limit tier.

1. <https://github.com/settings/applications/new> (or
   `https://github.com/organizations/<org>/settings/applications/new` for an
   org-owned OAuth App).
2. **Application name**: `midnight-manual` (anything works).
3. **Homepage URL**: `https://midnight-manual.midnightntwrk.expert`.
4. **Authorization callback URL**:
   `https://midnight-manual.midnightntwrk.expert/v1/auth/github/callback`.
5. Generate a client secret on the next screen; copy both values.

The callback URL must exactly match the cert hostname from step 2; using the
Fly default (`midnight-manual.fly.dev`) here would block OAuth on the
production domain later.

```bash
flyctl secrets set \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_ID=<client-id> \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET=<client-secret> \
    MIDNIGHT_MANUAL_GITHUB_OAUTH_REDIRECT_URL=https://midnight-manual.midnightntwrk.expert/v1/auth/github/callback \
    MIDNIGHT_MANUAL_GITHUB_ORG=midnight-network \
    --stage --app midnight-manual
```

If any one of these four is missing the `/v1/auth/github/*` endpoints return
503 cleanly (`app.rs` build-time gate), so a half-configured deploy is safe.

## 7. (Optional) Custom scoring policy

The compiled-in confidence policy (US6/D24) is the recommended default. If you
want to tune weights, ship a TOML file via secret:

```bash
flyctl secrets set MIDNIGHT_MANUAL_SCORING_POLICY="$(cat scoring-policy.toml)" \
    --stage --app midnight-manual
```

The server fails startup on a malformed policy (Constitution VIII fail-fast),
so test it locally first with `cargo test -p mn-core scoring_policy`.

## 8. First deploy

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

## 9. Smoke test

Once the machine is up, verify the basics. If the cert from step 2 hasn't
issued yet, swap in `https://midnight-manual.fly.dev` for the smoke run — both
hostnames serve the same machine.

```bash
HOST=https://midnight-manual.midnightntwrk.expert

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

## 10. Ingest a corpus

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

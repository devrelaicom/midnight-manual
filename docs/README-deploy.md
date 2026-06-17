# Deploy runbook: midnight-manual-server on Fly.io

This is the first-time deploy runbook for the cloud server (`midnight-manual-server` →
`midnight-manual-server`). Once the infrastructure is provisioned, **server
deploys are run by the operator** with `flyctl deploy` — they are intentionally
not wired into CI. Crate + CLI releases are separate and automatic: merging the
release-plz "Release vX.Y.Z" PR publishes the workspace crates to crates.io and
builds the prebuilt CLI binaries + Homebrew formula (it does not touch the
server image or Fly).

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
- A VoyageAI account with an API key for server-side embedding and reranking
  (both are remote API calls; no local model weights are downloaded).

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
`fly.toml`), so the 12 numbered migrations under
`crates/mnm-store/migrations/` apply on first deploy.

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

## 7. Configure Voyage embedding + token limits

The corpus uses dual VoyageAI embeddings: `voyage-context-3` (general contextualized
embeddings for all chunks) and `voyage-code-3` (a second vector for code chunks). Both
embedding and reranking are remote VoyageAI API calls — no model weights are downloaded
or loaded locally.

Clients that hold their own Voyage key (BYOK) call VoyageAI directly and never touch
this server's key. Clients that don't POST raw query text to `POST /v1/embeddings`,
which the server embeds under **this server's** Voyage account. So
**non-BYOK user query text is processed under the key you set here** — treat it
accordingly.

### Enable Voyage zero-retention

Before pointing real traffic at the proxy, enable **zero-retention** on the
Voyage account whose key this server uses — training disabled, no data
retention. Because non-BYOK callers' query text flows through this account,
zero-retention is what keeps that text from being retained or used for model
training upstream. This is an operator action in the Voyage dashboard, not a
server setting.

### Set the Voyage key (secret)

`VOYAGE_API_KEY` enables server-side embedding and inline reranking. Without it,
`POST /v1/embeddings` returns **503** (`server embedding is not configured`) and
reranking degrades to RRF order with reason `provider_error`. The rest of the
server still boots and serves reads, so a deploy without it is safe but limited.

```bash
flyctl secrets set VOYAGE_API_KEY=<voyage-platform-key> \
    --stage --app midnight-manual
```

### Embedding + token-limit knobs (plain env, set in `fly.toml`)

These are tuning knobs with safe defaults — set them only to override. They are
**not** secrets, so put them in `fly.toml`'s `[env]` block rather than
`flyctl secrets set`. (`VOYAGE_API_KEY` above is the one secret.)

| Variable | Purpose | Default |
| --- | --- | --- |
| `VOYAGE_API_KEY` | **Secret.** Voyage platform key for server-side embedding and reranking. Unset → `/v1/embeddings` 503s; rerank degrades to RRF order. | _(unset)_ |
| `MIDNIGHT_MANUAL_VOYAGE_CONTEXT_MODEL` | VoyageAI contextualized embedding model (general corpus chunks). | `voyage-context-3` |
| `MIDNIGHT_MANUAL_VOYAGE_MODEL` | VoyageAI flat embedding model (code chunks; also used for `/v1/embeddings` proxy). | `voyage-code-3` |
| `MIDNIGHT_MANUAL_VOYAGE_DIM` | Output dimension. | `1024` |
| `MIDNIGHT_MANUAL_VOYAGE_DTYPE` | Output dtype. | `float` |
| `MIDNIGHT_MANUAL_SERVER_RERANK` | Inline rerank kill switch. Set to `off` to disable server-side reranking (searches fall back to RRF order). | _(enabled)_ |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_ANON_HOURLY` | Hourly token budget, anonymous tier. | `2000` |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_ANON_DAILY` | Daily token budget, anonymous tier. | `20000` |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_UPLIFT_HOURLY` | Hourly token budget, read-uplift (GitHub SSO) tier. | `4000` |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_UPLIFT_DAILY` | Daily token budget, read-uplift tier. | `40000` |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_ADMIN_HOURLY` | Hourly token budget, admin tier. | `500000` |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_ADMIN_DAILY` | Daily token budget, admin tier. | `100000000` |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_GLOBAL` | Site-wide token ceiling over the global window (anti-Sybil backstop on non-admin tiers). `u64::MAX` disables it. | `10000000` |
| `MIDNIGHT_MANUAL_TOKEN_LIMIT_GLOBAL_WINDOW_SECS` | Rolling window for the global ceiling. | `10800` (3 h) |
| `MIDNIGHT_MANUAL_TOKEN_SNAPSHOT_SECS` | Interval at which token-usage counters flush to the store. | `300` (5 min) |

`POST /v1/embeddings` logs only token counts and the caller's subject
key (the client IP / SSO user id) — never the submitted query text. A 429
over-budget response carries only window/limit/reset metadata. That invariant is
enforced by a CI privacy canary.

> The client-side embedder and reranker resolve their own Voyage key and an
> optional `MIDNIGHT_MANUAL_VOYAGE_BASE_URL` override on the CLI/MCP side. The
> server does **not** read `MIDNIGHT_MANUAL_VOYAGE_BASE_URL`; it always talks to
> the default Voyage endpoint.

## 8. (Optional) Custom scoring policy

The compiled-in confidence policy (US6/D24) is the recommended default. If you
want to tune weights, ship a TOML file via secret:

```bash
flyctl secrets set MIDNIGHT_MANUAL_SCORING_POLICY="$(cat scoring-policy.toml)" \
    --stage --app midnight-manual
```

The server fails startup on a malformed policy, so test it locally first with
`cargo test -p mnm-core scoring_policy`.

## 9. Deploying the server

Server deploys are **always operator-run** — they are intentionally not wired
into CI. Build and roll out from `Dockerfile.server` on Fly's remote builder:

```bash
flyctl deploy --app midnight-manual
```

Watch the logs:

```bash
flyctl logs --app midnight-manual
```

Expected sequence: "resolved active embedding model" → "starting
midnight-manual-server" → migrations applied → listener bound on `:8080`.

Because deploys run from your machine, you only need to be authenticated locally
(`flyctl auth login`) — there is **no** `FLY_API_TOKEN` GitHub Actions secret and
no CI deploy step.

> The release pipeline (`release-plz`) is independent of server deploys: merging
> the "Release vX.Y.Z" PR publishes the workspace crates to crates.io and builds
> the prebuilt CLI binaries + Homebrew formula. It does **not** build the server
> image or deploy to Fly.

## 10. Smoke test

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
# → {"name":"voyage-context-3","revision":1,"dim":1024,"provider":"voyageai",
#    "code":{"name":"voyage-code-3","revision":1,"dim":1024,"provider":"voyageai"}}

# Search against an empty corpus — should 200 with no results.
# (The server embeds the query via VoyageAI; no client-side vector needed.)
curl -fs -X POST "$HOST/v1/search" \
    -H 'content-type: application/json' \
    -d '{"query":"hello","limit":5}' \
    | jq '.results | length'
# → 0
```

## 11. Ingest a corpus

The corpus is initially empty. The new ingest tools live in two
top-level command groups:

- `mnm manifest {init,generate,check}` — purely local. Builds and
  validates a `hierarchy.yaml`. No server contact required, so these
  can be used against any docs source — including repos you don't
  have write access to.
- `mnm ingest {plan,run}` — talks to the server. `plan` is a
  dry-run; `run` does the real ingest.

> **Model-migration note.** If you ever update the active embedding model,
> every source must be re-ingested before it returns hits under the new model
> (search filters out chunks whose stored model id differs from the active corpus
> model, so a partially-migrated corpus stays correct — just smaller — during the
> rollover). Use `mnm models status` to list sources still on the old model and
> `mnm models migrate --to <new-model-wire-id>` to re-ingest them in batch.

### 11a. Smoke-test with the sample corpus

```bash
mnm manifest check corpus/sample/hierarchy.yaml
mnm ingest run corpus/sample/hierarchy.yaml \
    --source-slug sample \
    --yes   # auto-create the 'sample' source on first run
```

Watch the progress lines stream by; on success you'll see
`finalized revision 1 (first version); +N new`.

### 11b. Ingest a real Midnight-docs repo

If the docs repo is one you own, commit the manifest alongside the
content; otherwise generate it locally and keep it next to your
`auth.toml`.

```bash
# Generate a manifest from globs + a sitemap.
mnm manifest generate 'docs/**/*.md' 'docs/**/*.mdx' \
    --base /path/to/midnight-docs \
    --sitemap https://docs.midnight.network/sitemap.xml \
    -o midnight-docs.yaml

# Plan the ingest (no writes).
mnm ingest plan midnight-docs.yaml \
    --source-slug midnight-docs

# Run it.
mnm ingest run midnight-docs.yaml \
    --source-slug midnight-docs \
    --yes
```

The `--server` flag is no longer needed in the common case — it
defaults to `https://midnight-manual.midnightntwrk.expert`. Set
`MIDNIGHT_MANUAL_SERVER` (or the `[server].url` config field) to
point at a different deployment.

## 12. (Optional) Alerting

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
| `503 service_unavailable` on `/v1/auth/challenge` or `/v1/auth/verify` | `MIDNIGHT_MANUAL_USER_STORE` or `MIDNIGHT_MANUAL_JWT_SECRET` unset. |
| `503` on `/v1/auth/github/*` | Any of the four `MIDNIGHT_MANUAL_GITHUB_*` secrets missing (client ID, client secret, redirect URL, org). |
| `409 embedding_model_mismatch` from a CLI | Client embedding-model id doesn't match the active corpus model. Run `mnm models pull` and retry. |
| `relation "chunk" does not exist` in server logs | pgvector extension missing (step 2 not run) or migrations disabled (`MIDNIGHT_MANUAL_AUTO_MIGRATE=false`). |
| `failed to resolve active embedding model` at boot | The `embedding_model` table is empty. Migration `0006_seed_embedding_model.sql` should have populated it — confirm the migration ran. |

## What's not yet covered

These are tracked as open production gaps:

- Backup / restore runbook (Fly Postgres supports snapshots, but the
  recipe isn't here yet).
- Grafana dashboard JSON / Prometheus alert rules.
- Recall + load benchmark gates (`tests/recall/`, `tests/load/`).

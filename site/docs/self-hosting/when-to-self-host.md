---
title: When to self-host
sidebar_label: When to self-host
description: How the hosted Midnight Manual instance compares to running your own, and when it makes sense to operate your own server.
---

# When to self-host

Most people never need to run `midnight-manual-server` themselves. The hosted instance at `https://midnight-manual.midnightntwrk.expert` carries the full Midnight corpus, exposes all MCP and CLI endpoints, and handles rate limiting, embeddings, and corpus management for you.

Self-hosting exists for teams who need control that the hosted instance cannot give them: private corpora, air-gapped environments, or specific SLA/data-residency requirements.

## The hosted instance

The hosted instance is the right choice if:

- You are working with the public Midnight corpus (docs, SDKs, example repos, partner code).
- You want zero infrastructure to maintain: no Postgres, no Fly.io, no VoyageAI API key to manage.
- You are comfortable with the hosted rate limits and the standard [tiered access](/docs/mcp/rate-limits) model (anonymous, GitHub-OAuth uplift, admin).

The hosted server is `midnight-manual-server`, a single self-contained binary, running on Fly.io against Fly Managed Postgres with the `pgvector` extension. It uses dual VoyageAI embeddings (`voyage-context-3` for general chunks, `voyage-code-3` for code) and inline reranking. The API surface, rate-limit tiers, and corpus contents are the same whether you hit it via MCP or the CLI.

## When self-hosting makes sense

**Private corpus.** The hosted instance only carries sources the Midnight Manual maintainers have ingested. If your team's internal docs, proprietary contracts, or unreleased SDK branches need to be searchable in your AI workflows, you need your own server.

**Air-gapped or data-residency environments.** The server is a single binary deployed from `Dockerfile.server`. It builds for `linux/amd64` and `linux/arm64`. If your environment cannot reach `midnight-manual.midnightntwrk.expert`, you run your own.

**Custom trust and scoring.** The confidence-scoring policy is compiled in but overridable at boot via `MIDNIGHT_MANUAL_SCORING_POLICY`. Self-hosting lets you tune corpus trust weights without waiting for upstream changes.

**Full corpus control.** You decide which sources to ingest, at which versions, and what `retention_count` to apply. Rollbacks, retirements, and version promotion happen on your schedule.

## What running your own involves

The server requires:

- **PostgreSQL 16** with the `pgvector` extension (HNSW index for vector search, GIN index for full-text).
- A **VoyageAI API key** for embedding and reranking, or BYOK from the CLI side. Without a server key, `/v1/embeddings` returns 503 and reranking degrades to RRF order.
- A **JWT signing secret** (`MIDNIGHT_MANUAL_JWT_SECRET`, HS256, ≥ 32 bytes).
- A **user store** TOML listing your admin principals and their Ed25519 public keys.

The binary runs automatic migrations at boot (`MIDNIGHT_MANUAL_AUTO_MIGRATE=true`), exposes `/healthz` (liveness) and `/readyz` (readiness), and propagates request IDs on every response. There is no embedded UI; the server is an API only.

See [Cloud server & deploy](./cloud-server.md) for the complete provisioning runbook.

## Related pages

- [Cloud server & deploy](./cloud-server.md) — full deployment runbook, Fly.io provisioning, secrets reference.
- [Users & access](./users-access.md) — managing Ed25519 keypairs and the user store.
- [Running an ingest](./running-an-ingest.md) — how to populate your corpus after the server is up.
- [MCP rate limits](/docs/mcp/rate-limits) — the hosted tier system and uplift mechanism.

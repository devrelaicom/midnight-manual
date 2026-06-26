---
title: Rate limits
sidebar_label: Rate limits
description: Tiers, the uplift mechanism, response headers, and checking your current rate-limit state.
---

# Rate limits

The hosted corpus is open and anonymous; no key is required to search. Rate limiting bounds how fast any one caller can hit it.

Limits are enforced by a per-request **token bucket**: each tier gets a refill rate in requests per second, and the bucket holds one second's worth of burst. Every response carries `x-ratelimit-limit`, `x-ratelimit-remaining`, and `x-ratelimit-reset`. Exceeding your budget returns `429 Too Many Requests` with a `Retry-After` header.

## Tiers

Your tier is resolved per request: a valid read-uplift token gets you the higher bucket, otherwise you fall to anonymous. You are charged against the matching bucket.

| Tier | How you get it | Limit | Keyed by |
|---|---|---|---|
| **Anonymous** | Default — no token | **10 req/s** | Client IP |
| **Read-uplift** | `mnm auth github` (GitHub SSO) | **60 req/s** | Your user |

### Multi-query cost

`advanced_search` costs `max(1, distinct queries)` tokens per call. A 3-query HyDE fan-out spends 3 tokens against your bucket. The [Advanced Search skill](./advanced-search-skill.md) is mindful of this and avoids unnecessary query expansion.

## The uplift mechanism

Anything beyond casual use should take the read-uplift: a **6× lift** (10 -> 60 req/s), free:

```bash
mnm auth github      # opens GitHub OAuth; mints a 30-day read-uplift token
mnm auth status      # show the active token and its expiry
```

The token is a 30-day JWT (configurable 1–90 days) stored in your local auth file. The CLI and MCP server send it automatically with every request.

A read-uplift token **only raises your rate limit**. It can never write to the corpus; the tier guard runs before the role check, so it is safe to mint freely. The 30-day life also outlasts any single working session, so you mint it once and rarely re-authenticate mid-task.

## Boosting limits for hackathons and events

When a room of attendees shares one IP or NAT range, the anonymous tier throttles the whole group as if it were a single caller. If your event runs against the hosted instance, ask its maintainers to lift the limit on your venue's network block for the duration; one grant covers everyone behind that IP, with no per-attendee signup. Running your own server, you set this yourself: see [Versions & rate limits](/docs/self-hosting/versions-rate-limits).

## Self-hosting

Running your own server, every limit is tunable by environment variable. See [Versions & rate limits](/docs/self-hosting/versions-rate-limits) for the knobs and their defaults.

## Checking your current state

Call the [`status`](./corpus-diagnostics.md) tool to see your current tier, remaining headroom in both limit families, and whether your read-uplift token is present and valid.

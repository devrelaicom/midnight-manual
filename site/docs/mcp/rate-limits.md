---
title: Rate limits
sidebar_label: Rate limits
description: Tiers, the uplift mechanism, and how to boost limits for hackathons and events.
---

# Rate limits

The hosted corpus is open and anonymous; no key is required to search. Rate limiting keeps it fast and fair for everyone.

Limits are enforced by a per-request **token bucket**: each tier gets a refill rate in requests per second, and the bucket holds one second's worth of burst. Every response carries `x-ratelimit-limit`, `x-ratelimit-remaining`, and `x-ratelimit-reset`. Exceeding your budget returns `429 Too Many Requests` with a `Retry-After` header.

## Tiers

Your tier is resolved per request in this order: **CIDR override -> admin -> read-uplift -> anonymous**. You are charged against the matching bucket.

| Tier | How you get it | Limit | Keyed by |
|---|---|---|---|
| **Anonymous** | Default — no token | **10 req/s** | Client IP |
| **Read-uplift** | `mnm auth github` (GitHub SSO) | **60 req/s** | Your user |
| **Admin** | Maintainer Ed25519 token | **1000 req/s** | Your user |
| **CIDR override** | Admin-granted, per network block | Custom | The CIDR |

### Multi-query cost

`advanced_search` costs `max(1, distinct queries)` tokens per call. A 3-query HyDE fan-out spends 3 tokens against your bucket. The [Advanced Search skill](./advanced-search-skill.md) is mindful of this and avoids unnecessary query expansion.

## The uplift mechanism

Anything beyond casual use should grab the free read-uplift: a **6× lift** (10 -> 60 req/s) at no cost:

```bash
mnm auth github      # opens GitHub OAuth; mints a 30-day read-uplift token
mnm auth status      # show the active token and its expiry
```

The token is a 30-day JWT (configurable 1–90 days) stored in your local auth file. The CLI and MCP server send it automatically with every request.

A read-uplift token **only raises your rate limit**. It can never write to the corpus; the tier guard runs before the role check, so it is safe to mint freely. The 30-day life also outlasts a long-running session; an admin token's one-hour window would not.

## Boosting limits for hackathons and events

When a room of attendees shares one IP or NAT range, an admin can grant a **per-CIDR override** that lifts everyone behind that network block for a fixed window, with no per-attendee signup:

```bash
# Lift an entire venue's network to 200 req/s for the weekend
mnm ratelimits add --cidr 203.0.113.0/24 --limit 200 --ttl 72h

mnm ratelimits list                    # see active overrides + expiry
mnm ratelimits extend <id> --ttl 24h   # give it more time
mnm ratelimits remove <id>             # revoke early
```

Overrides are time-boxed; they expire on their `--ttl`. The server refreshes its override cache every ~30 seconds, so grants and revocations take effect promptly. This is the recommended path for events: far simpler than minting tokens for every participant.

## Self-hosting

Every limit is tunable via environment variable when running your own server:

| Variable | Controls |
|---|---|
| `MIDNIGHT_MANUAL_RATE_LIMIT_ANONYMOUS_RPS` | Anonymous tier refill rate |
| `MIDNIGHT_MANUAL_RATE_LIMIT_UPLIFT_RPS` | Read-uplift tier refill rate |
| `MIDNIGHT_MANUAL_RATE_LIMIT_ADMIN_RPS` | Admin tier refill rate |
| `MIDNIGHT_MANUAL_RATE_LIMIT_ENABLED` | Toggle the whole subsystem |

## Checking your current state

Call the [`status`](./corpus-diagnostics.md) tool to see your current tier, remaining headroom in both limit families, and whether your read-uplift token is present and valid.

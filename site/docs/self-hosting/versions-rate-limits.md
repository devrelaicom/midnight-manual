---
title: Versions & rate limits
sidebar_label: Versions & rate limits
description: How to inspect, roll back, and retire source versions, and how to manage per-CIDR rate-limit overrides with mnm versions and mnm ratelimits.
---

# Versions & rate limits

After an ingest run, the corpus holds a new active `source_version`. The `mnm versions` commands let you inspect the version history, roll back to a prior revision, and retire stale versions. The `mnm ratelimits` commands let you add and manage per-CIDR rate-limit overrides.

Admin commands are hidden by default. Reveal them with:

```bash
export MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1
```

## Version management

Every ingest run produces a new `source_version` for a source slug. Versions have a lifecycle:

| Status | Description |
|---|---|
| `building` | In progress; invisible to search. |
| `active` | The single live version for this slug; served to all search queries. |
| `inactive` | A prior version; retained per the source's `retention_count`. |
| `retired` | Marked for cleanup by the background sweep job. |

Only one version per slug can be `active` at a time. Promoting a new revision demotes the current active to `inactive` automatically.

### Listing versions

```bash
mnm versions list midnight-docs
```

Prints each revision with its status and whether it is active:

```
  rev 1      inactive
  rev 2      inactive
  rev 3      active   (active)
```

`mnm versions list` is an anonymous read and needs no login token.

### Showing one version

```bash
mnm versions show midnight-docs 2
```

Prints the revision, status, `is_active`, and whether it has been retired.

### Rolling back

`rollback` promotes the most recent `inactive` version back to `active`, demoting the current active. It is a convenience that calls `list` then `promote` internally:

```bash
mnm versions rollback midnight-docs
```

Requires an admin token (`mnm login` first).

If no prior inactive version exists (the slug has only one active `source_version`), the command exits with an error rather than attempting a no-op.

### Promoting a specific revision

To promote a specific historical revision:

```bash
mnm versions promote midnight-docs --revision 2
```

The named revision must currently be in `inactive` state. Requires an admin token.

### Retiring a version

`retire` marks a version for cleanup by the background sweep job. The active revision cannot be retired; promote another version first:

```bash
mnm versions retire old-source --revision 3
```

A background sweep job retires stale and aborted versions after a grace window. Retirement is a one-way operation.

## Rate-limit overrides

The server applies tiered rate limiting: anonymous traffic is limited per IP; signing in via GitHub OAuth (a 30-day read-uplift token) raises the limit; admins have a much higher ceiling. Per-CIDR overrides raise or lower limits for specific network blocks: a hackathon, a CI cluster, or a misbehaving client.

All `ratelimits` subcommands require an admin token.

### Adding an override

```bash
mnm ratelimits add \
    --cidr 203.0.113.0/24 \
    --limit 20 \
    --ttl 90d

# With a note
mnm ratelimits add \
    --cidr 203.0.113.0/24 \
    --limit 200 \
    --ttl 30d \
    --note "launch event"
```

`--limit` accepts `200` or `200/s`. `--ttl` accepts single-unit durations: `90s`, `30m`, `48h`, `7d`.

### Listing active overrides

```bash
mnm ratelimits list
```

Prints each active override with its UUID, CIDR, limit, and optional note. Overrides whose TTL has passed are not shown.

### Extending an override's TTL

```bash
mnm ratelimits extend <uuid> --ttl 30d
```

Sets a new expiry from now, based on the supplied TTL.

### Removing an override

```bash
mnm ratelimits remove <uuid>
# → prompts: Remove rate-limit override `<uuid>`? [y/N]

mnm ratelimits remove <uuid> --yes   # skip the confirmation
```

Removal is interactive by default. Pass `--yes` for scripts and non-interactive environments (the command refuses without it when stdin is not a terminal).

### Tuning the limits

The per-tier refill rates, and the subsystem itself, are set by environment variable on the server. The defaults match the tiers the hosted instance serves: 10 req/s anonymous, 60 req/s read-uplift, 1000 req/s admin.

| Variable | Controls |
|---|---|
| `MIDNIGHT_MANUAL_RATE_LIMIT_ANONYMOUS_RPS` | Anonymous tier refill rate |
| `MIDNIGHT_MANUAL_RATE_LIMIT_UPLIFT_RPS` | Read-uplift tier refill rate |
| `MIDNIGHT_MANUAL_RATE_LIMIT_ADMIN_RPS` | Admin tier refill rate |
| `MIDNIGHT_MANUAL_RATE_LIMIT_ENABLED` | Toggle the whole subsystem |

## Token-limit knobs

Per-tier token budgets (hourly and daily) are configured via environment variables on the server, not the CLI. See the token-limit table in [Cloud server & deploy](./cloud-server.md) for the full list of variables and their defaults.

## Related pages

- [Running an ingest](./running-an-ingest.md) — how to create new versions via `mnm ingest run`.
- [Users & access](./users-access.md) — getting the admin token required for version and rate-limit operations.
- [Cloud server & deploy](./cloud-server.md) — server-side token-limit configuration.
- [MCP rate limits](/docs/mcp/rate-limits) — how the hosted instance's rate limits work for end users.

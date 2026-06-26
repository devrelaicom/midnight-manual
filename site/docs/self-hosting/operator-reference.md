---
title: Operator & admin reference
sidebar_label: Operator & admin reference
description: The admin-only mnm subcommands and operator config keys for running your own server — keys, users, login, ingest, ratelimits, tokenlimits, and server-side settings.
---

# Operator & admin reference

These subcommands and settings operate the server side of Midnight Manual: registering and versioning sources, minting admin keys and users, running ingests, and tuning rate and token limits. They require an admin token and are hidden from `mnm --help` by default.

This page is the command and config surface. For the workflows behind it, see [Users & access](/docs/self-hosting/users-access), [Versions & rate limits](/docs/self-hosting/versions-rate-limits), [Running an ingest](/docs/self-hosting/running-an-ingest), and [Cloud server & deploy](/docs/self-hosting/cloud-server).

## Revealing the commands

Admin subcommands run when called by name regardless of visibility. To surface them in `--help` output, set either:

```bash
export MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1   # environment variable
```

```toml
[cli]
show_admin_cmds = true                     # config file
```

## Admin operations on shared commands

`sources`, `versions`, and `models` each expose anonymous reads (documented in the [CLI reference](/docs/reference/cli)) alongside admin operations. The admin operations require an admin token.

### `sources`

| Subcommand | Description |
|---|---|
| `create` | Register a new source (requires admin bearer). |
| `update` | Update an existing source. |
| `retire` | Retire a source: soft-delete, not reversible via the CLI. |
| `list-all` | List every source including retired ones. |

### `versions`

| Subcommand | Description |
|---|---|
| `promote <slug> --revision N` | Promote a historical version back to active. |
| `rollback <slug>` | Roll back to the most recent prior active version, a convenience wrapper around `promote`. |
| `retire <slug> --revision N` | Retire a single historical version. The active revision is rejected; promote another version first. |

### `models`

| Subcommand | Description |
|---|---|
| `status` | List sources still on an older embedding model. |
| `migrate` | Re-ingest every source not yet on the target embedding model. |

## Admin-only subcommands

### `keys`

Ed25519 keypair management.

| Subcommand | Description |
|---|---|
| `generate` | Generate a new keypair, persist the private half locally, print the public half in `users.toml` wire form. |

### `login`

Admin login via challenge-response.

### `users`

Local user-store CRUD.

| Subcommand | Description |
|---|---|
| `list` | List users in the local user store. |
| `show [id]` | Show one user by id. |
| `add` | Add a new user. |
| `update` | Update an existing user's role, public key, or note. |
| `remove` | Remove a user from the local store. |

### `admin`

Admin tooling group: prompt-injection detector warmup and ad-hoc scoring.

### `ingest`

Run an admin ingest from a manifest.

| Subcommand | Description |
|---|---|
| `plan` | Compute the ingest plan locally without starting a server-side run. |
| `run` | Execute an ingest against the cloud server. |

### `ratelimits`

Per-CIDR rate-limit override CRUD.

| Subcommand | Description |
|---|---|
| `add` | Create a new per-CIDR override. |
| `list` | List overrides still in effect. |
| `extend [id]` | Extend an existing override's TTL. |
| `remove [id]` | Remove an override. |

### `tokenlimits`

Per-CIDR or per-user embedding token-limit override CRUD.

| Subcommand | Description |
|---|---|
| `add` | Create a new per-CIDR or per-user override. |
| `list` | List overrides still in effect. |
| `extend [id]` | Extend an existing override's TTL. |
| `remove [id]` | Remove an override. |

## Operator configuration

Settings that only matter when running your own server.

### `[cli]`

| Key | Default | Description |
|---|---|---|
| `show_admin_cmds` | `false` | Reveal admin subcommands in `--help` output. Equivalent to `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1`. |

### Server-side environment variables

| Variable | Description |
|---|---|
| `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS` | Set to `1` to reveal admin subcommands in `--help`. |
| `MIDNIGHT_MANUAL_USER_STORE` | Path to the local user store. |
| `MIDNIGHT_MANUAL_JWT_SECRET` | JWT signing secret. |

Rate-limit and token-limit refill rates are configured with their own environment variables; see [Versions & rate limits](/docs/self-hosting/versions-rate-limits) and [Cloud server & deploy](/docs/self-hosting/cloud-server).

## Related pages

- [Users & access](/docs/self-hosting/users-access) — minting keys, the user store, and admin login.
- [Versions & rate limits](/docs/self-hosting/versions-rate-limits) — managing source versions and rate-limit overrides.
- [Running an ingest](/docs/self-hosting/running-an-ingest) — `mnm ingest plan` and `mnm ingest run`.
- [Cloud server & deploy](/docs/self-hosting/cloud-server) — provisioning and server-side token limits.

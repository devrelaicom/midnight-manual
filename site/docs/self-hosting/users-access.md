---
title: Users & access
sidebar_label: Users & access
description: How Midnight Manual manages admin principals — Ed25519 keypairs, the user store TOML, nonce-signed JWT authentication, and the mnm keys / users / login commands.
---

# Users & access

Midnight Manual uses **Ed25519 challenge-response** for admin authentication — no passwords, no shared secrets at rest, just public keys in a TOML store. Principals are managed with three CLI commands: `mnm keys`, `mnm users`, and `mnm login`.

Admin commands are hidden by default. Reveal them with:

```bash
export MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1
```

## The user store

Users live in a TOML file (the `users.toml` user store) loaded server-side from the `MIDNIGHT_MANUAL_USER_STORE` environment variable. On the CLI side, path precedence is:

1. `MIDNIGHT_MANUAL_USER_STORE` environment variable.
2. XDG-derived `<config_home>/midnight-manual/users.toml`.

The file shape:

```toml
schema_version = 1

[[users]]
user_id    = "ops-primary"
role       = "admin"
public_key = "ed25519:<base64>"
created_at = "2026-05-25"
# note     = "optional human note"

[[users]]
user_id    = "ingest-bot"
role       = "writer"
public_key = "ed25519:<base64>"
created_at = "2026-05-25"
```

Two roles:

| Role | Permissions |
|---|---|
| `admin` | Full surface including `/v1/admin/*` (source CRUD, rate-limit and token-limit management, user management). |
| `writer` | Ingest writes and all read endpoints. Cannot reach `/v1/admin/*` beyond ingest. |

The store is the authority for `user_id → public_key + role` lookups. The server loads it at boot from the environment variable and never mutates it at runtime. Updating the roster is a "edit local file → redeploy" flow (the CLI prints a deploy-warning after every mutation as a reminder).

## Generating a keypair

Every principal needs an Ed25519 keypair. The private half never leaves the machine it was generated on.

```bash
mnm keys generate --user-id alice
```

This:

1. Generates a fresh Ed25519 keypair using the OS RNG.
2. Writes the 32-byte signing seed to `$XDG_CONFIG_HOME/midnight-manual/keys/alice.private` with mode `0o600` on Unix.
3. Echoes the public half to stdout in `ed25519:<base64>` wire form, ready to paste into the user store.

The private half is never echoed to stdout, stderr, or logs.

Useful flags:

| Flag | Description |
|---|---|
| `--dry-run` | Print the intended write path and public key without touching the filesystem. |
| `--force` | Overwrite an existing `<user_id>.private`. Refused by default — the CLI will not silently rotate a live key. |

## Managing users

Once you have a public key, add the user to the store:

```bash
# Add a new admin
mnm users add \
    --user-id alice \
    --role admin \
    --public-key "ed25519:Base64NoPad…"

# List all users
mnm users list

# Show one user
mnm users show alice

# Update role or key
mnm users update --user-id alice --role writer

# Remove a user
mnm users remove --user-id alice
```

All mutation commands accept `--dry-run` to validate inputs without writing, and print a deploy-warning after every successful write: **the change is local only until you deploy the updated user-store file**.

`mnm users add` rejects duplicate `user_id` values and validates the `--public-key` against the wire format (`ed25519:<base64>`). An invalid key is rejected before any file is written.

## The full onboarding flow

Here is the three-step sequence for adding a new maintainer:

```bash
# Step 1: The new maintainer generates a keypair locally.
# Their private key never leaves their machine.
mnm keys generate --user-id alice

# Step 2: An existing admin adds them to the user store and redeploys.
mnm users add --user-id alice --role writer --public-key "ed25519:Base64NoPad…"
# → deploy the updated users.toml for the change to take effect

# Step 3: Alice logs in to mint a short-lived JWT.
mnm login --user-id alice
```

## Logging in

`mnm login` runs the Ed25519 challenge-response handshake:

1. Loads the local signing key from `<config_home>/midnight-manual/keys/<user_id>.private` (chmod-checked on Unix — fails if the file is group- or world-readable).
2. `POST /v1/auth/challenge {user_id}` → `{challenge_id, nonce_b64}`.
3. Decodes the nonce, signs it with the local key, base64-encodes the signature.
4. `POST /v1/auth/verify {challenge_id, signature_b64}` → `{token, user_id, expires_at}`.
5. Persists `{token, expires_at}` to `<config_home>/midnight-manual/auth.toml` under `[admin]` with mode `0o600`.

The token is an HS256 JWT carrying the role and an auth tier. It is never logged or printed — `--json` output carries `user_id` and `expires_at` but not the token bytes.

```bash
mnm login --user-id alice
# → logged in as alice; admin token expires in 60 min

# Dry-run: runs the full handshake without persisting the token
mnm login --user-id alice --dry-run
```

Subsequent CLI commands that need admin access (ingest run, versions rollback, ratelimits add, …) read the token from `auth.toml` automatically. If the token is missing or expired, the command exits with a clear `run mnm login --user-id <id>` error before any network call.

## GitHub OAuth read-uplift

GitHub OAuth is a separate concept from the Ed25519 admin flow. Members of the configured GitHub org can exchange an OAuth flow for a **read-uplift** JWT that bumps their rate-limit tier (see the MCP rate-limits page). A read-uplift token carries no role and can never gain write access — the tier guard runs before the role guard.

The OAuth endpoints (`/v1/auth/github/*`) are configured server-side via `MIDNIGHT_MANUAL_GITHUB_*` secrets; they do not appear in `users.toml`.

## Related pages

- [Cloud server & deploy](./cloud-server.md) — how to stage `users.toml` as a Fly secret and configure the server.
- [Versions & rate limits](./versions-rate-limits.md) — admin commands that require a login token.
- [MCP rate limits](/docs/mcp/rate-limits) — the GitHub OAuth uplift tier for readers.

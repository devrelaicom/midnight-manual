# Rate-limit enforcement middleware — design (Phase 17)

**Date:** 2026-05-22
**Status:** approved
**Consumes:** the `rate_limit_override` CRUD shipped in Phase 16 (PR #33).

## Problem

The read API has no rate limiting. The spec requires tiered limits — CIDR
overrides, GitHub-SSO uplift, and an anonymous per-IP floor — with informative
headers on every response and a clean `429` when a caller is over budget. Phase
16 made the override rows manageable; this phase makes them (and the tier
limits) actually enforced.

Relevant requirements: FR-029 (every response carries `X-RateLimit-Limit`,
`-Remaining`, `-Reset`), FR-031 (decision order: CIDR → SSO → anonymous;
chosen tier + limit reported via headers), FR-034 (log `rate_limit_decision`;
never log tokens/PII), D11 (the three tiers), acceptances #8/#9/#10 (Story 4),
and edge cases EC-62 (override expires mid-flight) and EC-63 (overlapping CIDRs).

## Scope

**In:**
- Per-request rate limiting across the read surface.
- Tier resolution: CIDR override → admin → read-uplift → anonymous.
- In-memory token buckets, per process (per Fly machine).
- An override cache refreshed periodically from Postgres.
- Client-IP extraction from a trusted proxy header.
- `X-RateLimit-Limit` / `-Remaining` / `-Reset` on **every** non-exempt response.
- `429` with `Retry-After` and a typed `rate_limited` error body that names the
  limit and reset time.

**Out (deferred to Story 7, with a hook left in place):**
- The D25 multi-query *N-token* cost and EC-88 / EC-90 / EC-92. Charging stays
  at **1 token per request**. The middleware stashes a `RateLimitContext` in the
  request extensions exposing `try_charge(extra)` so the multi-query work can
  layer the additional cost on without re-architecting.

**Non-goals:**
- Distributed / cross-machine limiting (no Redis). Per-machine buckets are
  acceptable for v1; revisit if Fly scales to many machines.
- Changing the server bind. Behind Fly the socket peer is the proxy, so the
  client IP comes from a header, not `ConnectInfo`.

## Architecture — two units

### 1. `crates/mn-server/src/ratelimit.rs` — the engine (no axum; pure, unit-tested)

- **`TokenBucket`** — `{ tokens: f64, last_refill: Instant }`. Continuous
  refill at `rps` tokens/sec, capacity = `rps` (a one-second burst).
  `try_take(n) -> Result<u32 /* remaining */, RetryAfter>` where `RetryAfter`
  carries the seconds until `n` tokens are available.
- **`RateLimiter`** — holds `Mutex<HashMap<Key, TokenBucket>>` plus a reaper
  that evicts buckets idle beyond a TTL (bounds memory). `Key` ∈
  `{ Ip(String), User(String), Cidr(String) }`. Methods:
  `check(key, rps, cost) -> Decision`, `reap()`, and
  `refresh_overrides_now(pool)` (used by tests and the background task).
- **`OverrideCache`** — `ArcSwap<Vec<ParsedOverride>>`, where `ParsedOverride`
  carries the parsed network (address + prefix length), `limit_rps`, and
  `created_at`. Refreshed by calling `rate_limit_override::list_active`.
  `match_ip(ip) -> Option<&ParsedOverride>` selects the **longest-prefix**
  match; ties on prefix length are broken by the newest `created_at` (EC-63),
  and overlapping CIDRs log a one-time startup warning.
- **`cidr_contains(network: &str, ip: IpAddr) -> Option<u8>`** — hand-rolled
  bit-mask containment for IPv4 and IPv6, returning the prefix length on a
  match. No new dependency (consistent with Phase 16's `validate_cidr`).

### 2. `crates/mn-server/src/middleware/rate_limit.rs` — thin HTTP glue

- Extract the client IP: configured header (default `fly-client-ip`) → first
  entry of `x-forwarded-for` → the sentinel `"unknown"` (a shared bucket) when
  neither is present.
- Resolve the tier and limit (see below), charge the base cost (1 token).
- On rejection: return `429` with `Retry-After`, a `rate_limited` typed body
  naming the limit and reset, and the `X-RateLimit-*` headers.
- On acceptance: stash a `RateLimitContext { key, tier, limit }` in the request
  extensions, run the handler, then read the bucket's current state and set the
  `X-RateLimit-*` headers on the way out (so any extra charge a handler makes is
  reflected automatically).
- Exempt paths: `/healthz`, `/readyz`, `/metrics`.
- Emit a `rate_limit_decision` tracing field (FR-034). Never log the token; the
  IP is used transiently and never persisted.

## Tier resolution & limits

Order (FR-031):

1. **CIDR override** — if the client IP matches an active override (longest
   prefix), key = `Cidr(network)`, limit = the row's `limit_rps`.
2. **Admin** — JWT `tier = Admin` (challenge-response). Key = `User(sub)`,
   limit = `rate_limit_admin_rps`. Modeled as the top bucket rather than a
   special-cased exemption so headers and `429`s stay uniform.
3. **Read-uplift** — JWT `tier = ReadUplift` (GitHub SSO). Key = `User(sub)`,
   limit = `rate_limit_uplift_rps`.
4. **Anonymous** — no usable token. Key = `Ip(client_ip)`, limit =
   `rate_limit_anonymous_rps`.

### New `ServerConfig` fields (env-driven)

`Default::default()` leaves rate limiting **disabled** so every existing test
(which fires many requests with no IP) is unaffected. Production opts in via
env, mirroring the embedder / retirement daemons.

| Field | Env var | Default |
|---|---|---|
| `rate_limit_enabled` | `MIDNIGHT_MANUAL_RATE_LIMIT_ENABLED` | `false` |
| `rate_limit_anonymous_rps` | `MIDNIGHT_MANUAL_RATE_LIMIT_ANONYMOUS_RPS` | `10` |
| `rate_limit_uplift_rps` | `MIDNIGHT_MANUAL_RATE_LIMIT_UPLIFT_RPS` | `60` |
| `rate_limit_admin_rps` | `MIDNIGHT_MANUAL_RATE_LIMIT_ADMIN_RPS` | `1000` |
| `rate_limit_client_ip_header` | `MIDNIGHT_MANUAL_RATE_LIMIT_CLIENT_IP_HEADER` | `fly-client-ip` |
| `rate_limit_override_refresh_secs` | `MIDNIGHT_MANUAL_RATE_LIMIT_OVERRIDE_REFRESH_SECS` | `30` |

## Headers

- `X-RateLimit-Limit` — the resolved tier's `rps` (bucket capacity).
- `X-RateLimit-Remaining` — `floor(tokens)` after the charge.
- `X-RateLimit-Reset` — seconds until the bucket is full again.
- On `429`: `Retry-After` — seconds until the next token is available.

## Wiring

- `RateLimiter` is built in `app::build` when `rate_limit_enabled` and stored in
  `AppState` (an `Option<Arc<RateLimiter>>`; `None` ⇒ middleware passes through).
- The middleware layer is added in `app.rs` after `request_id` and `bearer` (it
  needs the request id for error bodies and the `AuthContext` for the tier).
- The override-refresh loop and the bucket reaper spawn in `main.rs`, like the
  embedder and retirement jobs.

## EC-62 (override expires mid-flight)

The tier/limit is resolved once, at request start, before the handler runs; the
in-flight request completes under that decision. The override cache's staleness
window is bounded by the refresh interval, which satisfies the spec's intent
that the *next* request gets the default tier.

## Testing

**Unit (engine, no HTTP):**
- token-bucket refill / capacity / `try_take` math (including fractional refill);
- `cidr_contains` for IPv4 and IPv6, including non-matches and `/32` / `/128`;
- longest-prefix selection and the `created_at` tie-break;
- client-IP header precedence;
- tier resolution given each combination of override / admin / uplift / anon.

**Integration (`crates/mn-server/tests/rate_limit_enforcement.rs`):**
- success carries all three `X-RateLimit-*` headers (FR-029);
- anonymous caller exhausts the bucket → `429` + `Retry-After` + body naming the
  limit and reset (acceptance #8);
- read-uplift token gets the higher tier and a higher limit (acceptance #9);
- a seeded CIDR override + `refresh_overrides_now` raises the limit for a
  matching IP (acceptance #10);
- admin token gets the top tier;
- `/healthz` and `/metrics` are never throttled.

Tests construct a fresh `RateLimiter` per app (no cross-test contamination) and
set `Fly-Client-IP` to control the bucket key deterministically.

## Risks

- **Header trust / spoofing** — the client-IP header is set by Fly's proxy and
  the app isn't directly reachable; v1 trusts it. If the deployment topology
  changes, the trusted-header assumption must be revisited.
- **`Mutex<HashMap>` contention** — the critical section is a map lookup plus a
  little arithmetic; fine for v1 throughput. Sharding (or `dashmap`) is the
  obvious later optimization if it shows up in profiles.
- **Existing-test blast radius** — mitigated by `rate_limit_enabled` defaulting
  off in `ServerConfig::default()`.

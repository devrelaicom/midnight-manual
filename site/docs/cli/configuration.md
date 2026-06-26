---
title: Configuration
sidebar_label: Configuration
description: The mnm config file, environment variables, and config show --effective.
---

# Configuration

Configuration resolves in a clear precedence order: **command-line flag > environment variable > config file > compiled-in default**. You can override any setting at any layer without touching the others.

## Config file

The config file is TOML. `mnm` discovers it via:

1. `--config <path>` (or `MIDNIGHT_MANUAL_CONFIG` env)
2. `$XDG_CONFIG_HOME/midnight-manual/config.toml`
3. `$HOME/.config/midnight-manual/config.toml`

A missing file is fine; compiled-in defaults apply throughout.

```toml
[server]
url = "https://midnight-manual.midnightntwrk.expert"

[models]
embedding      = "voyage-context-3"  # remote VoyageAI general embedder (contextualized)
code_embedding = "voyage-code-3"     # remote VoyageAI code embedder (second vector)
# voyage_api_key = "…"              # optional — BYOK embedding + reranking (else server-proxied)
# voyage_timeout_secs = 120         # optional — per-request Voyage embed timeout (seconds)

[rerank]
location = "auto"      # auto (default) | local | server | off
model    = "rerank-2.5"  # rerank-2.5 (default) | rerank-2.5-lite

[telemetry]
enabled = true
# endpoint = "https://gauge-telemetry.fly.dev"  # optional — override the Gauge endpoint

[cli]
show_admin_cmds = false
```

The full key reference lives in a Reference section of this site, not yet published.

## Key environment variables

| Variable | Effect |
|---|---|
| `MIDNIGHT_MANUAL_SERVER` | Corpus URL (same as `--server`). |
| `MIDNIGHT_MANUAL_CONFIG` | Config file path (same as `--config`). |
| `MIDNIGHT_MANUAL_DISABLE_TELEMETRY` | Opt out of telemetry for every invocation. |
| `MIDNIGHT_MANUAL_GAUGE_ENDPOINT` | Override the Gauge telemetry endpoint. |
| `MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS` | Reveal admin subcommands in `--help`. |
| `VOYAGE_API_KEY` | Your Voyage key for BYOK embedding and reranking. Unset -> proxied by the hosted server. |
| `VOYAGE_TIMEOUT_SECS` | Per-request timeout (seconds) for Voyage embed calls (default 120). |
| `MIDNIGHT_MANUAL_RERANK` | Rerank placement: `auto` \| `local` \| `server` \| `off`. |
| `MIDNIGHT_MANUAL_RERANK_MODEL` | Voyage rerank model: `rerank-2.5` \| `rerank-2.5-lite`. |
| `RUST_LOG` | Log verbosity (`error` / `warn` / `info` / `debug` / `trace`). Logs go to stderr. |

## `mnm config show`

`mnm config show` prints the resolved configuration: the config file merged with compiled-in defaults. It does not show runtime overrides from flags or env vars.

```bash
mnm config show
mnm config show --json
```

### `--effective`

`mnm config show --effective` layers env-var and global-flag overrides on top of the config file, so you see the values the CLI would actually use for a given invocation. This is useful for diagnosing surprising behaviour.

```bash
mnm config show --effective
mnm config show --effective --json
```

What `--effective` resolves:

- `server.url`: the actual URL after `--server` / `MIDNIGHT_MANUAL_SERVER` is applied, with any trailing slash trimmed.
- `models.voyage_api_key`: shows `****` if a key is effective (from flag, env, or config), `null` if absent. The real value is never printed.
- `models.voyage_timeout_secs`: resolved from `VOYAGE_TIMEOUT_SECS` env or config.
- `rerank.location`: `auto` is resolved to the concrete placement (`local` when a Voyage key is present, `server` otherwise).
- `rerank.model`: the actual model that would be used.
- `models.cache_dir`: the resolved local cache directory path.
- `telemetry.enabled`: `false` if `--no-telemetry` was passed.

The `--effective` output is annotated with a comment noting that it is not a copy-paste config file. It reflects runtime decisions that config files cannot express, like `auto` rerank placement expanding to `local` or `server`.

## Precedence summary

```
--flag          (highest)
 ENVIRONMENT VAR
  config.toml
   compiled default  (lowest)
```

Any layer can be omitted; the next lower layer fills in. The one exception is the `--effective` diagnostic: it surfaces the result of this chain rather than being part of it.

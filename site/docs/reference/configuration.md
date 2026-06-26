---
title: Configuration reference
sidebar_label: Configuration
---

# Configuration reference

Configuration resolves in a fixed precedence order: **command-line flag › environment variable › config file › compiled-in default**. You can override anything at any layer without touching the others.

Source: README `## Configuration` section.

---

## Config file

Format: TOML. Location resolved in this order:

1. `--config` flag
2. `MIDNIGHT_MANUAL_CONFIG` environment variable
3. `$XDG_CONFIG_HOME/midnight-manual/config.toml`

Inspect the effective, merged configuration at any time:

```bash
mnm config show           # config file + defaults
mnm config show --effective  # config + env + flag overrides (secrets redacted)
```

### Full example

```toml
[server]
url = "https://midnight-manual.midnightntwrk.expert"

[models]
embedding      = "voyage-context-3"  # remote VoyageAI general embedder (contextualized)
code_embedding = "voyage-code-3"     # remote VoyageAI code embedder (second vector)
# voyage_api_key = "…"                # optional — BYOK embedding + reranking
# voyage_timeout_secs = 120           # optional — per-request Voyage embed timeout

[rerank]
location = "auto"                     # auto (default) | local | server | off
model    = "rerank-2.5"               # rerank-2.5 (default) | rerank-2.5-lite

[telemetry]
enabled  = true
# endpoint = "https://gauge-telemetry.fly.dev"  # optional — override the Gauge endpoint
```

---

## Config keys

### `[server]`

| Key | Default | Description |
|---|---|---|
| `url` | `https://midnight-manual.midnightntwrk.expert` | Corpus cloud server URL. |

### `[models]`

| Key | Default | Description |
|---|---|---|
| `embedding` | `voyage-context-3` | Remote VoyageAI general embedder (contextualized). |
| `code_embedding` | `voyage-code-3` | Remote VoyageAI code embedder (second vector for dual-embedding). |
| `voyage_api_key` | _(unset)_ | Your Voyage API key for BYOK embedding and reranking. When unset, embedding and reranking are proxied by the hosted server. |
| `voyage_timeout_secs` | `120` | Per-request timeout in seconds for Voyage embedding calls. |

### `[rerank]`

| Key | Default | Description |
|---|---|---|
| `location` | `auto` | Where reranking runs: `auto` picks local when a Voyage key is set, else server; `local` forces BYOK; `server` forces server-side; `off` disables reranking. |
| `model` | `rerank-2.5` | Voyage rerank model: `rerank-2.5` or `rerank-2.5-lite` (lower latency, billed at half tokens server-side). |

### `[telemetry]`

| Key | Default | Description |
|---|---|---|
| `enabled` | `true` | Telemetry opt-in. Set `false` to disable. Equivalent to `MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1`. |
| `endpoint` | `https://gauge-telemetry.fly.dev` | Gauge telemetry endpoint override. |

---

## Environment variables

| Variable | Description |
|---|---|
| `MIDNIGHT_MANUAL_SERVER` | Corpus URL, same effect as `--server`. |
| `MIDNIGHT_MANUAL_CONFIG` | Config file path. |
| `MIDNIGHT_MANUAL_DISABLE_TELEMETRY` | Set to `1` to opt out of telemetry. |
| `MIDNIGHT_MANUAL_GAUGE_ENDPOINT` | Override the Gauge telemetry endpoint (default `https://gauge-telemetry.fly.dev`). |
| `VOYAGE_API_KEY` | Your Voyage key for BYOK embedding and reranking. Unset means embedding and reranking are proxied by the hosted server. |
| `VOYAGE_TIMEOUT_SECS` | Per-request timeout in seconds for Voyage embedding calls (default `120`). Flag form: `--voyage-timeout-secs`. |
| `MIDNIGHT_MANUAL_RERANK` | Rerank placement: `auto` \| `local` \| `server` \| `off`. Same as `--rerank`. |
| `MIDNIGHT_MANUAL_RERANK_MODEL` | Voyage rerank model: `rerank-2.5` \| `rerank-2.5-lite`. Same as `--rerank-model`. |
| `RUST_LOG` | Log verbosity (`error`, `warn`, `info`, `debug`, `trace`). Same as `--log-level`. |

Operator-only settings (admin command visibility, the server-side user store, and the JWT signing secret) are documented in [Operator & admin reference](/docs/self-hosting/operator-reference).

---

## Precedence summary

For any setting, the order of precedence is:

1. **Command-line flag** (highest)
2. **Environment variable**
3. **Config file**
4. **Compiled-in default** (lowest)

The `--effective` flag on `mnm config show` displays the fully resolved values (secrets redacted) so you can confirm what the CLI will actually use.

See [CLI configuration](/docs/cli/configuration) for a guided walkthrough.

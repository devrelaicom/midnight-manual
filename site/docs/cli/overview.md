---
title: Overview
sidebar_label: Overview
description: The mnm CLI command map, global flags, and scripting with --json.
---

# CLI overview

`midnight-manual` / `mnm` is the command-line interface to the Midnight Manual corpus. Its design is noun-first: pick a noun (what you want to operate on), then a verb (what to do with it). Add `--json` to any command and you get machine-readable output on stdout: the same data, formatted for scripts instead of terminals.

## Command map

```text
mnm search   "<query>"                 ad-hoc hybrid search
mnm facets                             list the corpus's filterable facets
mnm sources  list | show               browse corpus sources
mnm versions list | show | promote …   inspect source versions
mnm chunks   show | next | prev | neighbors  walk the chunk graph by id
mnm documents show | chunks            read documents, windowed
mnm models   pull | active             model-cache dir / active embed model
mnm config   show                      show the resolved configuration
mnm telemetry disable | enable | status opt out (or back in)
mnm auth     github | status | logout  GitHub OAuth for rate-limit uplift
mnm manifest init | check | generate   author ingestion manifests
mnm skills   add | status | remove     install the advanced-search skill
mnm mcp      serve                     run the MCP server (stdio JSON-RPC)
mnm doctor                             environment & connectivity report
mnm status                             connectivity, auth & model readiness
mnm version                            build metadata
```

The CLI also carries operator commands for running your own server; they are hidden from `--help` by default and documented in [Self-hosting & operations](/docs/self-hosting/when-to-self-host).

## Global flags

Every subcommand inherits these flags; pass them before or after the subcommand name.

| Flag | Purpose |
|---|---|
| `--server <url>` | Point at a different corpus (env: `MIDNIGHT_MANUAL_SERVER`). Defaults to the hosted instance. |
| `--json` | JSON output instead of human-formatted text. |
| `--config <path>` | Use a specific config file (env: `MIDNIGHT_MANUAL_CONFIG`). |
| `--voyage-api-key <key>` | Voyage key for BYOK embedding and reranking (env: `VOYAGE_API_KEY`). |
| `--log-level <lvl>` | `error`, `warn`, `info`, `debug`, or `trace` (env: `RUST_LOG`). Diagnostics go to stderr; stdout is reserved for `--json` payloads. |
| `--no-telemetry` | Disable telemetry for this one invocation. |

## Scripting with `--json`

Adding `--json` makes any command emit newline-delimited JSON to stdout. The JSON shape is the stable contract; the human-readable formatting can change between versions.

```bash
# Search and feed results into jq
mnm search "nullifier double-spend prevention" --limit 5 --json \
  | jq '.results[] | {id: .chunk_id, score: .confidence, text: .content[0:120]}'

# Check the active embedding model in a script
model=$(mnm models active --json | jq -r '.wire_id')
echo "corpus is on $model"

# Pipe config to a pretty-printer
mnm config show --json | jq '.config.server'
```

The `--json` flag is global; it works on every subcommand, including `mnm telemetry status --json`, `mnm doctor --json`, and `mnm models pull --json`.

## Next steps

- [Searching](./searching.md): `mnm search` flags and filter options.
- [Reading content](./reading.md): navigating chunks and reading documents window by window.
- [Models](./models.md): what runs remotely and how to inspect it.
- [Configuration](./configuration.md): config file, env vars, and `mnm config show --effective`.
- [Skills and telemetry](./skills-telemetry.md): installing the advanced-search skill and opting out of telemetry.

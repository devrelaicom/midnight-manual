---
title: Skills and telemetry
sidebar_label: Skills and telemetry
description: mnm skills add — install the advanced-search skill; mnm telemetry — opt out of usage tracking.
---

# Skills and telemetry

## `mnm skills` — install the advanced-search skill

The advanced-search skill teaches your AI harness how to query the Midnight Manual corpus intelligently: multi-query expansion, HyDE, step-back prompting, filter construction, and trust-weighted result selection. Installing it once means you don't have to spell out retrieval strategies in every conversation.

### `mnm skills add`

Install (or update) the advanced-search skill into detected or specified harnesses.

```bash
mnm skills add                        # auto-detect installed harnesses
mnm skills add --harness claude-code  # install into a specific harness
mnm skills add --harness claude-code,cursor
mnm skills add --scope project        # install at project scope (this repo only)
```

| Flag | Default | Notes |
|---|---|---|
| `--harness <names>` | auto-detect | Comma-separated harness names. Supported: `claude-code`, `codex`, `opencode`, `cursor`. Omit to auto-detect installed harnesses. |
| `--scope <scope>` | `user` | `user` installs for all your projects; `project` installs into the current repository. |

When auto-detecting, `mnm skills add` scans for known harness installation markers on your machine. If a harness is not detected and you want to force installation, pass it explicitly with `--harness`.

The command reports each installed harness with the path written and the reload step (e.g. restart the harness or reload the plugin). If a skill file is already at the current version, it reports `already current` and exits cleanly.

### `mnm skills status`

Show where the skill is currently installed and whether each installation is up to date.

```bash
mnm skills status
mnm skills status --json
```

### `mnm skills remove`

Remove the advanced-search skill from detected or specified harnesses.

```bash
mnm skills remove
mnm skills remove --harness claude-code
```

## `mnm telemetry` — opt out of usage tracking

Telemetry is opt-out and carries no query content, chunk content, tokens, filesystem paths, or environment values. Seven event types are collected — all coarse scalars and closed enums (command name, duration, outcome). See the README's Telemetry & Privacy section for the complete field list.

### Opting out

Any one of these three mechanisms is enough to disable telemetry:

1. **Environment:** `MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1`
2. **Config file:** `telemetry.enabled = false`
3. **Runtime marker:** `mnm telemetry disable`

The runtime marker is written to disk and read at every subsequent startup, so it persists across shells and reboots.

```bash
mnm telemetry disable   # write the marker; telemetry stops immediately
mnm telemetry enable    # remove the marker; telemetry resumes (subject to env/config)
mnm telemetry status    # show the resolved state and which mechanisms are active
```

### `mnm telemetry disable`

Writes a persistent marker file. After this, every CLI and MCP invocation on this machine boots with telemetry off, regardless of env or config.

```bash
mnm telemetry disable
mnm telemetry disable --json
```

### `mnm telemetry enable`

Removes the marker. Telemetry resumes, subject to the env and config settings.

```bash
mnm telemetry enable
```

### `mnm telemetry status`

Shows the resolved opt-out state and the three per-mechanism disable flags.

```bash
mnm telemetry status
mnm telemetry status --json
```

Example human output:

```
telemetry: off
  endpoint:           https://gauge-telemetry.fly.dev
  marker file:        /home/user/.config/midnight-manual/telemetry-opt-out
  marker present:     true
  - disabled by runtime marker
```

The `--json` output includes `enabled`, `marker_present`, `disabled_by_env`, `disabled_by_config`, and `disabled_by_runtime` fields for scripting.

### Per-invocation flag

Use `--no-telemetry` (a [global flag](./overview.md)) to disable telemetry for a single invocation without writing any marker:

```bash
mnm search "Compact circuit" --no-telemetry
```

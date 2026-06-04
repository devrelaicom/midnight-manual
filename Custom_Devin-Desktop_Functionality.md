<!--
  ============================================================================
  Custom_Devin-Desktop_Functionality.md  (midnight-manual)
  ============================================================================
  PURPOSE
    This file documents how to run `midnight-manual` as an MCP server inside
    **Devin Desktop** (the IDE formerly known as **Windsurf**, by Codeium).
    The upstream README already documents Claude Code, Codex, and Cursor; this
    is the equivalent set of instructions for Devin Desktop users.

  WHO WROTE THIS
    Contributed by a community user (bytewizard42i) who is a *novice* developer.
    >>> Please double-check every command and config snippet below before
        relying on it. <<< It is accurate for the author's setup (Ubuntu on
        WSL2, Rust 1.91) but has NOT been tested across every platform.

  TERMINOLOGY NOTE
    "Devin Desktop" == "Windsurf". Codeium rebranded the Windsurf IDE to
    Devin Desktop. Anywhere this doc says "Devin Desktop", older docs/configs
    may still say "Windsurf" — they are the same product. The on-disk config
    paths still use the `~/.codeium/windsurf/` prefix as of this writing.
  ============================================================================
-->

# Using `midnight-manual` with Devin Desktop (formerly Windsurf)

`midnight-manual` ships an MCP server (`mnm mcp serve`). Devin Desktop is an
MCP client, so it can use this server exactly like Claude Code / Cursor /
Codex do. This guide is the Devin-Desktop-specific counterpart to the
"Install the MCP server into your AI client" section of the main README.

> **Novice disclaimer:** these steps were written by a learning developer.
> Treat them as a starting point and verify each command against your own
> environment before running it.

## 1. Build the `mnm` binary

```bash
# Requires a Rust toolchain >= 1.91 (matches this repo's MSRV).
#   rustc --version   # -> should be 1.91.0 or newer
#
# Build only the CLI/MCP crate (mn-cli) in release mode. This produces two
# identical binaries: `mnm` and `midnight-manual`.
cargo build --release -p mn-cli

# Put the short alias on your PATH. ~/.local/bin is a common choice that is
# usually already on PATH for interactive shells.
install -m 0755 target/release/mnm ~/.local/bin/mnm

# Sanity check.
mnm --version        # -> mnm 0.1.0
mnm mcp serve --help # confirms the `serve` subcommand exists
```

## 2. Register the server in Devin Desktop's MCP config

Devin Desktop reads its MCP servers from **`~/.codeium/windsurf/mcp_config.json`**
(the `windsurf` path is retained from before the rebrand). Add a
`midnight-manual` entry under `mcpServers`:

```jsonc
{
  "mcpServers": {
    // ... your existing servers ...
    "midnight-manual": {
      // Use the ABSOLUTE path to the binary. Devin Desktop may launch MCP
      // servers without your interactive shell's PATH, so do not rely on a
      // bare "mnm" here unless you are certain PATH is inherited.
      "command": "/home/<you>/.local/bin/mnm",
      "args": ["mcp", "serve"]
    }
  }
}
```

> **Tip — edit safely:** back the file up first (`cp mcp_config.json
> mcp_config.json.bak`) and, if scripting the edit, round-trip it through a
> JSON parser so you cannot leave it malformed. A broken `mcp_config.json`
> disables *all* your MCP servers, not just this one.

After saving, **reload MCP servers** in Devin Desktop (restart the app or use
its "refresh MCP servers" action). The server will not appear until you do.

## 3. (Recommended) Opt out of telemetry

Telemetry is **on by default**. To opt out, create the marker file (one of the
three documented opt-out mechanisms):

```bash
mkdir -p ~/.config/midnight-manual
touch ~/.config/midnight-manual/telemetry-disabled
```

## 4. Verify it works

```bash
# Health/reachability of the hosted corpus + local model state.
mnm doctor

# A real query. The FIRST run downloads the embedding + reranker models
# (a one-time, hundreds-of-MB fetch into your local cache), then returns
# ranked, source-attributed results.
mnm search "how do I declare a sealed ledger field in Compact"
```

Once verified on the CLI, the same corpus is available to Devin Desktop through
the MCP tools exposed by `mnm mcp serve`.

## What this enables in Devin Desktop

The assistant can now answer Midnight questions from the *real* indexed corpus
(docs + source) instead of a stale training set — with reranking, confidence
scores, and document navigation, just like the other supported clients.

<!--
  ---------------------------------------------------------------------------
  MAINTAINERS: this file only adds documentation. It introduces no code and
  changes no behavior. If you would prefer this content folded into the main
  README's client list (next to Claude Code / Codex / Cursor) rather than a
  standalone file, that is completely fine — say so on the PR and we will move
  it. We are new to contributing and happy to adjust.
  ---------------------------------------------------------------------------
-->

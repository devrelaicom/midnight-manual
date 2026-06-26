---
title: Advanced Search skill
sidebar_label: Advanced Search skill
description: What the midnight-advanced-search skill teaches, how to install it in one step, and which AI harnesses it supports.
---

# The Advanced Search skill

The MCP server gives your assistant hybrid retrieval, reranking, trust scoring, and chunk navigation. The **`midnight-advanced-search` skill** teaches the technique: how to combine those tools like a seasoned researcher instead of firing one naive query and hoping.

It is a persistent, auto-loaded Agent Skill (`SKILL.md`). Once installed, your agent reaches for the right retrieval pattern on its own, no prompting required.

## What the skill teaches

| Technique | What the agent does | Why it helps |
|---|---|---|
| **HyDE** (pseudo-answer) | Drafts a 1–2 sentence hypothetical answer phrased like documentation, then searches it alongside the bare question; both fuse via RRF. Cost: 2 queries. | Lifts recall when the question is short or uses different words than the docs. |
| **Multi-query** | Generates 2–3 paraphrases varying vocabulary and breadth, plus the original, fused in a single `advanced_search` call. Cost: the distinct query count. | Beats synonym mismatch between your phrasing and the corpus's. |
| **Step-back** | Pairs the specific question (a raw error, say) with a more abstract framing. Cost: 2 queries. | Rescues over-specific questions and raw error messages. |
| **Lexical anchoring** | Sends exact identifiers and error codes verbatim so the full-text half of hybrid search nails exact matches. | Catches the precise symbol, flag, or error the vector half would blur. |
| **Symbol-aware code search** | Scopes by `package` and `language`, uses `code_mode=exclusive`, navigates hits by their `symbol_path`. | Lands on the named circuit, contract, or function — not an arbitrary window. |
| **Version-matched retrieval** | Uses `version_satisfies` (a concrete version or semver range) with `version_match` `permissive`/`strict`, matched against versions extracted from Compact pragmas and package manifests. | Avoids answers pinned to the wrong Compact or SDK version. |
| **Retrieve-read-retrieve** | Broad first pass → read hits with `get_chunk_next` / `get_chunk_parents` → refine with newly-learned terms → search again. | Converges on precise answers the way a human researcher iterates. |
| **Trust-weighted selection** | Ranks and prunes on each result's `trust_score` and `confidence_factors` (attribution, verification, freshness, version-match). | Authoritative, version-matched sources rise; stale or deprecated ones sink. |
| **Cross-source comparison** | Pulls from multiple sources and surfaces disagreement instead of silently picking one. | Compensates for the deliberate absence of automatic contradiction detection. |

Worked examples for every pattern (the exact query array, the resulting `advanced_search` call, and the token cost) are folded into the bundled skill file.

## Install in one step

The easiest way is to ask your assistant to install it. The MCP server exposes an **`install_search_skill`** tool that writes the `SKILL.md` into every harness it detects and reports the per-harness reload step:

> "Install the midnight-advanced-search skill."

Your assistant calls `install_search_skill` and tells you exactly which files it wrote and what to do next (usually: restart the session or run the harness's skills-reload command).

## Manual install with the CLI

Prefer to drive it yourself:

```bash
mnm skills add                               # auto-detect installed harnesses
mnm skills add --harness claude-code,codex   # target a specific set
mnm skills add --scope project               # this repo only (default: user)
```

- `--harness` accepts a comma-separated list: `claude-code`, `codex`, `opencode`, `cursor`
- `--scope` is `user` (all your projects) or `project` (committed to the repo)

After installing, reload skills in your client. Both `mnm skills add` and the `install_search_skill` MCP tool print the exact reload step for each harness they touch.

## Supported harnesses

The skill ships as the same portable `SKILL.md` in every supported harness:

| Harness | Format | Installs to |
|---|---|---|
| **Claude Code** | `SKILL.md` (Agent Skill) | `~/.claude/skills/` · `.claude/skills/` |
| **Codex CLI** | `SKILL.md` (open Agent Skills standard) | `~/.agents/skills/` · `<repo>/.agents/skills/` |
| **OpenCode** | `SKILL.md` (native) | `~/.config/opencode/skills/` · `.opencode/skills/` |
| **Cursor** | `SKILL.md` (Agent Skill, Cursor 2.4+) | `~/.cursor/skills/` · `.cursor/skills/` |

Coming soon: Gemini CLI, Windsurf, Zed, Cline, Continue.

## Rate-limit awareness

Multi-query techniques like HyDE and step-back cost one rate-limit token per distinct query. The skill is mindful of this: a 3-query HyDE fan-out spends 3 tokens. See [Rate limits](./rate-limits.md) for tier details and how to get the free 6× read-uplift.

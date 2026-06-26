---
id: intro
title: Introduction
sidebar_position: 1
---

# Introduction

Your AI assistant's training data went stale the day it shipped. When you ask it a question about Midnight, it answers from whatever snapshot of the docs existed at training time — which may be months or years behind the current SDK, compiler, and contract patterns. midnight-manual fixes that.

## Ask your docs, not your model

midnight-manual is a retrieval engine purpose-built for the Midnight Network. It gives your AI assistant live search over the real Midnight docs and source code — ranked, cited, and current — so its answers trace back to something you can actually check.

The thesis is simple: **cited answers, not confident guesses.** Every result points back to the exact doc or source file it came from.

## What mnm is

One install ships three things that work together:

- **A local MCP server.** Drop one command into Claude Code, Codex, Cursor, or any MCP client and your assistant gains hybrid semantic search over the Midnight corpus, with reranking, source-aware confidence scoring, and document navigation.
- **A developer CLI.** Search the corpus from your terminal, read results in context, and manage settings with `mnm`. Add `--json` to anything to script it.
- **A hosted corpus.** The indexed corpus is hosted by default — most users never run a server. Search works the moment you install.

## Grounded, ranked, and current

Results are not a guess — they are retrieved from the live corpus and scored on three axes:

| Property | What it means |
|---|---|
| **Cited** | Every result points to the exact doc or source file, so your assistant shows its work. |
| **Ranked by trust** | Results are weighted by source, verification status, and freshness — the most trustworthy source rises to the top. |
| **Always live** | Searches the live Midnight corpus, not a frozen snapshot from whenever the model was trained. |

## No setup required

There is no database, no API key, and no account required to search. The indexed corpus is hosted and the default. Install `mnm`, point your client at it, and ask.

Ready to install? See [Install mnm](/docs/install), then [Add it to your AI client](/docs/add-to-ai-client), then [run your first search](/docs/first-search).

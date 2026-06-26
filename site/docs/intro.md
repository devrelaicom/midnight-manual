---
id: intro
title: Introduction
sidebar_position: 1
---

# Introduction

midnight-manual is a retrieval engine for the Midnight Network. It gives your AI assistant live search over the real Midnight docs and source code, and every result points to the exact doc or source file it came from.

A model answers Midnight questions from its training data, a snapshot that lags the current SDK and compiler by months. midnight-manual searches the live corpus at query time instead. Results reflect the docs and source as they are now.

## Components

midnight-manual has three parts:

- A local MCP server. One command wires it into Claude Code, Codex, Cursor, or any MCP client, where it exposes hybrid semantic search over the Midnight corpus, with reranking, source-aware confidence scoring, and document navigation.
- A developer CLI. Search the corpus from your terminal, read results in context, and manage settings with `mnm`. Add `--json` to any command to script it.
- A hosted corpus. The indexed corpus is hosted by default, so most users never run a server of their own.

## How results are ranked

Results are ordered by a trust weighting across source, verification status, and freshness. A verified, current source ranks above a stale or lower-trust match for the same query.

## What you need to search

The `mnm` CLI, and nothing else. There is no database to run and no API key or account to create; the hosted corpus is the default. Install `mnm`, point your client at it, and ask.

Start with [Install mnm](/docs/install), then [add it to your AI client](/docs/add-to-ai-client) and [run your first search](/docs/first-search).

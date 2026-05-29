# Developer Guide

Welcome to the developer guide for the corpus fixture.

## Getting Started

Before you begin, make sure you have the required dependencies installed.
This section walks you through the initial setup steps.

### Prerequisites

You will need a working Rust toolchain (stable, MSRV 1.91) and a local
Postgres instance with the pgvector extension enabled.

### Installation

Clone the repository and run the workspace build:

```bash
cargo build --workspace
```

After a successful build, run the migration to set up the database schema.

## Architecture Overview

The system is split into three deliverables that share a single Cargo workspace.

### The CLI

The `mn-cli` crate provides the `mnm` command-line interface. Use it to ingest
documents, query the search index, and manage sources.

### The MCP Server

The `mn-mcp` crate exposes a Model Context Protocol server over stdio. It is
launched by AI clients (like Claude Code) to answer queries against the indexed
corpus.

### The Cloud Server

The `mn-server` crate is a Fly.io-deployed HTTP API that backs both the CLI
and the MCP server. It stores documents, chunks, and embeddings in Postgres.

## Configuration

All configuration is driven by environment variables and a TOML auth file.
See the quickstart guide for a complete reference.

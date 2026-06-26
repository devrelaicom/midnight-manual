---
title: Corpus and diagnostics
sidebar_label: Corpus & diagnostics
description: The status, list_sources, and facets tools for inspecting the corpus and checking service health.
---

# Corpus and diagnostics

Four tools let you inspect what's in the corpus and whether the service is operating correctly.

## `status` — diagnose the retrieval setup

Call `status` when searches misbehave, return unexpected errors, or before starting a long session.

```
status()
```

No parameters. The response covers:

- **Cloud reachability** — whether the hosted corpus endpoint is reachable
- **Authentication state** — whether a read-uplift token is present and valid, and the tier it places you in
- **Rate-limit state** — both limit families: request rate and token budget; remaining headroom in each
- **VoyageAI key validity** — if you have a local `VOYAGE_API_KEY`, whether it resolves and is accepted
- **Rerank configuration** — whether reranking is configured and has been exercised in this session

If `search` or `advanced_search` returns an error you don't understand, call `status` first. It surfaces auth problems, exhausted rate limits, and misconfigured rerankers in one call.

## `list_sources` — enumerate corpus sources

List the sources that make up the corpus. Use this to discover what material exists and to get source slugs for `advanced_search` filters.

```
list_sources(cursor?, limit?, created_after?, created_before?, kind?, retired?)
```

| Parameter | Default | Notes |
|---|---|---|
| `cursor` | — | Opaque pagination token from a previous response's `next_cursor`. |
| `limit` | `20` | Results per page; max 100. |
| `created_after` | — | RFC3339 instant; only sources registered after this time. |
| `created_before` | — | RFC3339 instant; only sources registered before this time. |
| `kind` | — | Filter by source kind: `docs_site`, `code_repo`, `standalone`, or `mixed`. |
| `retired` | `false` | Include retired (inactive) sources. |

Each result includes the source's slug, display name, kind, and active revision. The slug is what you pass to `advanced_search` filters as a `source_slug` value.

## `facets` — discover filter dimensions

`facets` returns the filter dimensions that `advanced_search` accepts and the values present in the corpus. Call it before constructing a filtered search to avoid guessing.

```
facets(facet?, within?, cursor?, limit?)
```

| Parameter | Default | Notes |
|---|---|---|
| `facet` | — | Omit for the overview. Pass a facet name to page through all values of one open-set dimension. |
| `within` | — | Second drill level: enumerate version constraints within a named `language_target` or `sdk_dependency`, or within a package name. These are the values you pass to `advanced_search` via a filter's `version_satisfies` field. |
| `cursor` | — | Opaque pagination token. |
| `limit` | `50` | Values per page; max 200. |

Open-set facets you can drill into with `facet=`:

| Facet | What it contains |
|---|---|
| `source_slug` | Corpus source identifiers |
| `language` | Programming or documentation language tags |
| `tags` | Free-form tags |
| `package` | Package names (npm, crate, etc.) |
| `language_target` | Compact / SDK version targets extracted from source files |
| `sdk_dependency` | SDK dependency names and versions |

### Three-level discovery pattern

1. Call `facets()` (no arguments) — get the overview of all available filter dimensions and their approximate value counts
2. Call `facets(facet="source_slug")` — page through all source slugs in the corpus
3. Call `facets(facet="language_target", within="compact")` — drill into version constraints for a specific language target

Use the values you discover as inputs to `advanced_search` `filters`.

## When to use these tools

| Goal | Tool |
|---|---|
| Searches failing or returning errors | `status` |
| Need source slugs for a filter | `list_sources` |
| Don't know what filter values exist | `facets()` (bare) |
| Need all values for a specific facet | `facets(facet=...)` |
| Need version constraints within a facet | `facets(facet=..., within=...)` |

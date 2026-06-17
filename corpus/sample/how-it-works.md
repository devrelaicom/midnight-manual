# How this sample works

The sample corpus is a smoke-test fixture. Its purpose is to prove every step
of the ingest pipeline runs end to end on a freshly-deployed server, before
you point production traffic at it.

## Layout

```
corpus/sample/
  hierarchy.yaml      # manifest_version: 1, listing both placeholder docs
  welcome.md          # this directory's "first page" fixture
  how-it-works.md     # this file
```

## What each manifest entry does

The `hierarchy.yaml` manifest lists two `file:` entries, each with a
`published_url` and a `provenance` block. The provenance is what drives the
US6 confidence-scoring trust multiplier — these placeholders are tagged
`attribution: foundation` and `verified: true` so a search result for the
sample shows up with maximum trust (it's still placeholder text, but at least
the test exercises the high-trust code path).

## Replacing it

For real content:

1. Add a `corpus/<your-source>/` directory.
2. Author a `hierarchy.yaml` listing your Markdown files (or pin the manifest
   to inherit directory structure — see `crates/mnm-content/src/manifest.rs`
   for the full schema).
3. `mnm sources create --slug <your-source> --kind docs-site ...`.
4. `mnm ingest corpus/<your-source>/hierarchy.yaml --source-slug <your-source>`.

You can keep `corpus/sample/` around as a regression fixture, or delete it
once your real corpus is in place.

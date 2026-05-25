# `corpus/` — ingestable content roots

Each subdirectory under `corpus/` is an ingestable source root: a tree of
Markdown (and eventually code) plus a `hierarchy.yaml` manifest at the root
that declares the published hierarchy.

The `sample/` directory ships in this repo as a smoke-test fixture — see
[`docs/README-deploy.md`](../docs/README-deploy.md) §9a. It is intentionally
minimal and **not** authoritative Midnight content; it exists to prove the
ingest pipeline works end to end on a freshly-deployed server.

Real corpora (e.g. `midnight-docs/`) are typically cloned/symlinked in rather
than committed here so they can be versioned independently.

## Manifest schema

See [`crates/mn-content/src/manifest.rs`](../crates/mn-content/src/manifest.rs)
for the canonical Rust types. The top-level shape:

```yaml
manifest_version: 1
root:
  name: <human-readable name>
  children:
    - file: relative/path/to/doc.md
      published_url: https://example.com/path
      provenance: { attribution: foundation, verified: true }
    - name: A group
      children:
        - file: ...
```

Files not listed in the manifest fall back to directory-tree inference unless
the operator passes `--strict-manifest` to `mnm ingest`.

# Ingesting content

Two workflows depending on whether you own the docs source.

## Workflow A — docs repo you own

Commit `hierarchy.yaml` alongside the content. Operators in the
midnight-manual team re-ingest from the committed manifest.

```bash
cd /path/to/your-docs-repo

# Start with an empty template if this is your first time.
mnm manifest init -o hierarchy.yaml

# Or populate from a sitemap.
mnm manifest generate 'docs/**/*.md' \
    --sitemap https://your-site.example.com/sitemap.xml \
    -o hierarchy.yaml

# Validate before committing.
mnm manifest check hierarchy.yaml --sitemap https://your-site.example.com/sitemap.xml

git add hierarchy.yaml
git commit -m "chore: add midnight-manual ingest manifest"
```

A member of the midnight-manual team can then run:

```bash
mnm ingest run /path/to/your-docs-repo/hierarchy.yaml \
    --source-slug your-source --yes
```

## Workflow B — third-party docs repo

When you can't commit to the source repo, keep the manifest in your
own working tree:

```bash
mkdir -p ~/midnight-manual-manifests
mnm manifest generate \
    'docs/**/*.{md,mdx}' \
    --base ~/code/their-docs-repo \
    --sitemap https://their-docs.example.com/sitemap.xml \
    --name 'Their Project' \
    -o ~/midnight-manual-manifests/their-source.yaml

mnm ingest run ~/midnight-manual-manifests/their-source.yaml \
    --source-slug their-source --yes
```

## Re-running

`ingest run` is idempotent on content: documents whose hash matches
the prior active version are carried over (chunks re-linked, no
re-embed). Updated files re-chunk and re-embed; new files are added;
files absent from the new manifest become "deleted" relative to the
new version (the prior version is still retained per the source's
`retention_count`).

## Overriding source defaults

`mnm ingest run` auto-creates the source on first run with defaults
`kind=docs_site, retention_count=5, display_name=<slug>`. If you
need different defaults, create the source explicitly first:

```bash
MIDNIGHT_MANUAL_SHOW_ADMIN_CMDS=1 mnm sources create \
    --slug their-source \
    --kind docs-site \
    --display-name "Their Project" \
    --retention-count 10
```

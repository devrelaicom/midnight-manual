# Welcome

This page is a placeholder shipped with the `midnight-manual` repository to
exercise the ingest pipeline on a freshly-deployed server. It deliberately
contains no authoritative claims about the Midnight Network — the maintainer
replaces this directory with real corpus content before going live.

## What this file is for

Running `mnm ingest corpus/sample/hierarchy.yaml --source-slug sample` against
a deployed `midnight-manual-server` should:

1. Validate the manifest.
2. Walk the listed files and read their frontmatter.
3. Chunk this Markdown by heading.
4. Embed each chunk with the local `bge-base-en-v1.5` model.
5. Upload chunks + provenance to the cloud server.
6. Finalize the `source_version`, flipping it to `is_active = true`.

After that, a search like `mnm search "welcome placeholder"` should return one
or both chunks from this file.

## What this file is not

Anything you read here is fictional placeholder text. For the real Midnight
documentation corpus, point the ingest at the upstream `midnight-docs` repo
or your own content tree — see `docs/README-deploy.md` §9b.

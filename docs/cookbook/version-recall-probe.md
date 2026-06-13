# Version-recall probe (manual)

Checks whether contextualized embeddings actually discriminate version-qualified
queries — gates the skill's wording about semantic version matching (spec §7).

## Setup
1. Ingest a probe source with paired docs: identical tutorial bodies whose first
   paragraph states "This tutorial targets Compact 0.23" vs "... Compact 0.31"
   (plus one no-statement control). `mnm ingest run --source-slug version-probe ...`
2. Queries: "how to declare a ledger in compact 0.31", "... in compact 0.23",
   and the unqualified "how to declare a ledger in compact".

## Measure
For each query × mode (hybrid, vector, fts): note the rank order of the three
docs (`mnm search --json | jq '.results[].source_path'`).

## Interpretation
- Version-stated docs ranking above control for matching queries in **fts** but
  not **vector** ⇒ keep the skill's "put the version in your query text" wording
  (FTS-driven), do NOT claim semantic version matching.
- Discrimination in vector mode too ⇒ the skill may state contextualized
  embeddings carry version context.
Record the outcome here with the date and corpus model.

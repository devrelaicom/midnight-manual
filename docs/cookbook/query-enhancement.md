# Query enhancement cookbook

The `search` tool accepts more than one query at a time. When you pass several
queries, the server runs hybrid retrieval (full-text + vector) for each one and
fuses every ranked list — across both retrieval modes and across all of your
queries — with Reciprocal Rank Fusion (RRF, k=60) in a single pass. This lets a
calling agent trade a little LLM work for a measurable lift in recall.

This cookbook shows three patterns for generating those extra queries: **HyDE**,
**multi-query expansion**, and **step-back prompting**. Each section gives you
the LLM prompt the agent emits, the resulting `queries` array passed to the
`search` tool, and the rate-limit cost.

## How the input works

The MCP `search` tool takes **text** — you do not embed anything yourself. The
tool embeds each query locally (bge-base-en-v1.5) and posts the `{text, vector}`
pairs to the cloud `/v1/search` endpoint.

Single-query (convenience form):

```json
{ "query": "how do I pay transaction fees on Midnight?" }
```

Multi-query:

```json
{ "queries": ["how do I pay transaction fees on Midnight?", "what is the fee token?"] }
```

Both forms are equivalent for a single query; `{ "query": "x" }` behaves exactly
like `{ "queries": ["x"] }`.

### Rate-limit cost (D25)

A request costs `max(1, N)` tokens from your bucket, where `N` is the number of
**distinct** queries (identical queries are de-duplicated and do not inflate the
cost). A single query costs 1 token; a 3-query request costs 3. The
`X-RateLimit-Remaining` response header reflects the post-charge balance. The
hard ceiling is 10 queries per request (configurable lower via
`MIDNIGHT_MANUAL_MAX_QUERIES_PER_REQUEST`).

### Reading the diagnostics

Every response carries `search_metadata.per_query` (one record per distinct
query, with `fts_candidates`, `fts_latency_ms`, `vector_candidates`,
`vector_latency_ms`) and, on each result, `scores.matched_queries` — the 0-based
indices of the queries that contributed at least one FTS or vector rank to that
result. Use `matched_queries` to see *which* of your enhanced queries pulled a
given chunk in.

---

## 1. HyDE (Hypothetical Document Embeddings)

**When to use it.** The user's question is short, jargon-light, or phrased very
differently from how the corpus phrases the answer. A hypothetical answer tends
to land in the same embedding neighbourhood as the real documentation, so adding
it as a second query pulls in chunks the bare question would miss.

**LLM prompt the agent emits:**

```text
Write a short (1–2 sentence) hypothetical answer to the following question,
as if it were an excerpt from the documentation. Do not hedge; state it plainly
even if you are unsure — this text is used only to find similar real passages.

Question: {user_question}
```

**Resulting `queries` array** (the original question + the model's hypothetical
answer):

```json
{
  "queries": [
    "how do I pay transaction fees on Midnight?",
    "Transaction fees are paid in the network's fee resource, which is derived from the staking token and consumed when a transaction is submitted."
  ]
}
```

The hypothetical answer does **not** need to be correct — it is embedded and
discarded; only its position in vector space matters.

**Cost:** 2 tokens (2 distinct queries).

---

## 2. Multi-query expansion

**When to use it.** Synonyms or differing levels of specificity matter. The user
says "compile" but the docs say "build"; the user asks narrowly but the answer
lives in a broader page. Paraphrasing 2–3 ways widens the net.

**LLM prompt the agent emits:**

```text
Rewrite the following question as 2–3 alternative search queries. Vary the
vocabulary (use synonyms) and the breadth (one narrower, one broader). Return
one query per line, no numbering.

Question: {user_question}
```

**Resulting `queries` array** (include the original plus the paraphrases):

```json
{
  "queries": [
    "how do I compile a Compact contract?",
    "build a smart contract from source into a deployable artifact",
    "compactc command-line usage"
  ]
}
```

**Cost:** 3 tokens (3 distinct queries). If the model emits a paraphrase
identical to the original, the duplicate is dropped and you are charged for the
distinct count only.

---

## 3. Step-back prompting

**When to use it.** The user asked an over-specific question ("why did *this*
call fail with *that* code?") whose answer is a more general concept. Pairing the
specific question with a "stepped-back" abstract version retrieves both the
precise hit and the explanatory background.

**LLM prompt the agent emits:**

```text
Given the following specific question, write one more general "step-back"
question whose answer would provide the background needed to answer it. Return
only the step-back question.

Question: {user_question}
```

**Resulting `queries` array** (the specific question + the step-back question):

```json
{
  "queries": [
    "why did my contract call revert with a witness mismatch?",
    "how does Midnight validate contract calls and witnesses?"
  ]
}
```

**Cost:** 2 tokens (2 distinct queries).

---

## Combining patterns

The patterns compose: a HyDE answer plus two expansion paraphrases is a 4-query
request costing 4 tokens (capped at 10 queries / request). Start with a single
query, and reach for these only when recall on the bare question is weak — the
`matched_queries` field tells you whether the extra queries actually contributed.

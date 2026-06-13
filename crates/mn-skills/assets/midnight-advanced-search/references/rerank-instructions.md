# Rerank instructions — worked examples

`advanced_search` accepts `rerank_instructions` (max 400 chars): a
natural-language directive that steers how the candidate pool is reranked. It
**replaces** the derived default (code-focused under `code_mode=exclusive`;
version-preferring when a `language_target` filter carries `version_satisfies`
— derived from the **first `language_target` element that carries a
`version_satisfies`**), so fold those concerns back in yourself when you
override. The three shapes —
**emphasis** (what matters in the match), **filtering** (what kind of document),
and **disambiguation** (what an ambiguous term means) — are in `SKILL.md`; this
file works them against the real corpus. Every concrete value is illustrative —
confirm names with the `facets` tool.

Keep instructions terse: their tokens are multiplied across the ~50-doc
candidate pool, so a long directive is the most expensive thing you can add to a
search. Don't restate the query, don't stack contradictory goals, and omit the
instruction entirely when the derived defaults already fit.

## A. Filtering — official docs over community examples

**Query:** `compact contract upgrade pattern`
**Instruction:** `Prefer official Midnight documentation over community examples; deprioritize deprecated patterns.`
**Why:** attribution/deprecation live in trust scoring, but the instruction also
steers relevance toward chunks that *discuss* the current pattern rather than
merely mention upgrades.

## B. Emphasis — complete code over fragments

**Query:** `deploy a contract from the SDK`
**Filters:** `kind: { any_of: ["code"] }`, `content_type: { any_of: ["example"] }`
**Instruction:** `Prioritize complete, compilable examples over snippets or partial fragments.`
**Why:** the filters already restrict to code examples, but within that pool an
end-to-end script and a one-liner score similarly on lexical/vector overlap. The
instruction breaks the tie toward the example a reader can actually run.

## C. Disambiguation — pin an overloaded term

**Query:** `witness`
**Filters:** `language_target: { any_of: [{ name: "compact" }] }`
**Instruction:** `'Witness' is the Compact private-input function, not the ZK prover witness or a legal term.`
**Why:** "witness" is overloaded across the corpus (the Compact circuit input,
the ZK-proof witness, generic English). The reranker sees only the bare query;
naming the intended sense pulls the Compact-function chunks above the homonyms.

## D. Filtering — reference signatures over prose

**Query:** `how do I balance and submit a transaction`
**Filters:** `content_type: { any_of: ["reference"] }`
**Instruction:** `Prefer exact API signatures and parameter lists over narrative explanation.`
**Why:** the `content_type` filter narrows to reference material, but reference
pages mix signature tables with surrounding prose. The instruction favors the
chunk carrying the actual call shape when several reference chunks tie.

## E. Override the derived default deliberately

**Query:** `proof generation error in the prover`
**Filters:** `language_target: { any_of: [{ name: "compact", version_satisfies: ">=0.23" }] }`, `code_mode: "exclusive"`
**Instruction:** `Favor newest-version troubleshooting; prefer error-cause explanations over code that merely triggers the error.`
**Why:** `code_mode=exclusive` + a `version_satisfies` filter would normally
derive a code-focused, version-preferring default. Here we *want* the
explanation over the failing snippet, so we override — but re-state the
version preference, because our instruction replaces the derived one entirely.

## When to omit — and when to skip reranking

- **Omit the instruction** when the derived defaults already fit: a plain
  `code_mode=exclusive` code hunt, or a `version_satisfies`-pinned search, needs
  no instruction — the default already does the right thing, for free.
- **`rerank: false`** is the cheap-exploration switch. On a broad recon sweep
  where you only want to *see what exists* (e.g. enumerating sources or sizing
  recall via `search_metadata.total_candidates`), ordering precision doesn't
  matter — skip the rerank, then rerank the refined query once you know what
  you're after.

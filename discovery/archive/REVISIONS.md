# Revision History: rag-platform

*Record of all revisions to graduated stories.*

---

[Revision entries will be added when graduated stories are revised]

## REV-001: Story 4 - Additive (no contract break) — 2026-05-13

**Trigger**: Story 6 (confidence scoring) graduation — D24

**Before**:
```
Per-result response shape: chunk, document, source, source_version, package, parent_chain, navigation, scores. Search request body: queries, client_embedding_model, limit, filters, include_scores.
```

**After**:
```
Per-result response shape adds: trust_score (float 0..1), confidence (float 0..1), confidence_factors (object). Search request body adds: sort_by ∈ {confidence, trust, relevance, score} default 'confidence'; min_confidence ∈ [0,1] default 0. search_metadata adds: filtered_by_confidence, sort_by.
```

**Decision Reference**: D24

**User Confirmed**: Yes — [Date]

---

## REV-002: Story 5 - Additive (no contract break) — 2026-05-13

**Trigger**: Story 6 (confidence scoring) graduation — D24

**Before**:
```
MCP search result shape: same as cloud /v1/search per-result, plus rerank_score top-level when rerank=true.
```

**After**:
```
MCP search result adds: trust_score, confidence, confidence_factors. When rerank=true, the MCP server substitutes reranker_score for the relevance term in the confidence blend and sets confidence_factors.relevance_source = 'rerank'. When the reranker is unavailable, the server falls back to the cloud's confidence and sets relevance_source = 'rrf'.
```

**Decision Reference**: D24

**User Confirmed**: Yes — [Date]

---

# Iteration Summaries: rag-platform

*Summary of discovery iterations for context and retrospective.*

---

[Iteration summaries will be added at natural breakpoints (typically phase transitions)]

## ITR-001: 2026-05-13 — Problem Exploration through Story 1 graduation

**Phase**: Problem Exploration through Story 1 graduation

**Goals**:
Establish problem space; research open technical questions on local embedding/reranking/query rewriting; crystallize 11 stories with priorities; graduate Story 1 (content model and metadata schema) to SPEC.md

**Activities**:
Read CONSTITUTION.md and README.md; populated problem statement and personas; ran live web research on Rust ML ecosystem; surveyed fastembed-rs model catalog; verified Compact module syntax via OpenZeppelin FungibleToken.compact example

**Key Outcomes**:
13 research-backed decisions logged (D1-D15 with D14/D15 superseding D5/D8); 12 acceptance scenarios for Story 1; 15 functional requirements; 12 edge cases; 6 success criteria; full entity model with 7 tables plus provenance JSONB schema

**Questions Added**: 5 clarifying questions on Compact module detection, admin auth, read auth with hackathon-mode rate limiting, page-level URLs, and ML model lifecycle

**Decisions Made**: D1 fastembed-rs; D2 BGE reranker server-side; D3 caller-delegated query rewriting; D4 RRF in app code; D5/D14 bge-base-en-v1.5; D6 tree-sitter; D7 filesystem with manifest override; D8/D15 per-source versioning retention 5; D9 in-source Compact module detection; D10 Ed25519 challenge-response admin auth; D11 anonymous plus GitHub SSO with CIDR overrides; D12 client-supplied model id with server-side mismatch detection; D13 page-level source_url and published_url

**Research Conducted**: Rust embedding library landscape; cross-encoder reranking latency; query rewriting tradeoffs; hybrid FTS plus pgvector with RRF

**Next Steps**:
Start Story 2 (Markdown ingestion CLI) after confirming global CLI shape with the user

---

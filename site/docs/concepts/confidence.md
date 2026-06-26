---
title: Confidence = trust × relevance
sidebar_label: Confidence scoring
description: How Midnight Manual blends source trust with retrieval relevance into a confidence score, and the per-factor breakdown it returns.
---

# Confidence = trust × relevance

The confidence score multiplies retrieval relevance by a **trust score** built from where the content came from, how recently it was written, and whether it matches the version you target.

The formula is explicit. Every hit carries its per-factor breakdown, so your AI assistant can explain why a passage ranked where it did without a second round-trip.

## The trust factors

Trust is a composite of five independent signals:

### Attribution tier

Who published the content? Five tiers, each weighted differently:

- **Foundation**: produced directly by the Midnight Network Foundation; highest trust.
- **Partner**: produced by an accredited ecosystem partner.
- **Third-party**: independent developer or community project.
- **Community**: informal or crowdsourced contribution.
- **Unknown**: provenance could not be established.

Foundation-authored content starts with a significant advantage; unknown-provenance content starts penalized.

### Verification status

Has a human reviewed and vouched for the content? Verified content earns a boost; unverified content does not. The verification chain records who did the verifying (Foundation, partner, or community member), so the boost is proportional to the verifier's authority.

### Freshness

Documentation that was accurate six months ago may be wrong today. Midnight Manual applies **exponential decay by age**: a passage written last week scores materially higher than an equivalent one from eighteen months ago, all else equal. Fast-moving docs (SDK changelogs, compiler release notes) don't sneak to the top by sheer volume of old references.

### Deprecation flag

Content explicitly marked deprecated is **down-weighted** rather than hidden. It may still surface as a last resort, but the confidence score reflects its status and the factor breakdown will name it. Your assistant can tell a user "this is the old API; here is the current one."

### Version match

Version match is the only factor that can exclude a chunk outright; the others only adjust its weight. The corpus tracks which Compact language version, SDK version, or component version each chunk belongs to. At query time:

- Content that **satisfies** your target version is boosted.
- A **near-miss** (adjacent version) is penalized in proportion to how far off it is.
- A **breaking mismatch** is excluded entirely.
- In `strict` mode, only version-satisfying content passes at all.

Version targets are extracted automatically at ingest (from `pragma language_version` in Compact files and from `package.json` / `Cargo.toml` manifests), so the corpus carries version metadata without manual tagging.

## The factor breakdown

Nothing about the confidence score is hidden. Each result carries the factor breakdown, so:

- An assistant can say "this is Foundation-authored, recently verified, and matches the SDK version you specified" rather than just "here is a result."
- A downstream tool can filter on individual trust signals (showing only verified content for a security-sensitive query, for example).
- You can tune the weights without a rebuild: the scoring policy is loaded from a data file at runtime.

## How it interacts with retrieval

Confidence does not replace the retrieval score; it multiplies it. A passage with high semantic relevance but zero trust (say, unverified community content from two years ago on a deprecated API) ranks behind a passage that is moderately relevant but Foundation-authored, recent, and version-matched.

The reranker ([`rerank-2.5`](./models.md)) sees the original candidates; the confidence multiplier is applied after reranking to produce the final order. This keeps the two signals orthogonal: the reranker optimizes for semantic fit; confidence adjusts for provenance.

## Related pages

- [Models](./models.md) — the reranker that sharpens the candidate set before confidence scaling.
- [Hybrid retrieval & RRF](./hybrid-retrieval.md) — how the candidate set is built in the first place.
- [Multi-query / HyDE](./multi-query-hyde.md) — techniques for improving recall before confidence scoring.

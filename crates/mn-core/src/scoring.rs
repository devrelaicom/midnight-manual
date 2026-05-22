//! Confidence-scoring compute layer (US6, D24).
//!
//! Pure, DB-free math: given a [`crate::provenance::Provenance`], an age in
//! whole days, a query-side version constraint, and a normalized relevance
//! term, produce a `trust_score`, a blended `confidence`, and a per-factor
//! [`ConfidenceFactors`] breakdown. The relevance-normalization helpers
//! ([`normalize_rrf`], [`normalize_rerank`]) are compiled-in, not
//! policy-configurable, so every confidence score in the corpus stays
//! reproducible (spec §"Relevance term").

use serde::Serialize;

use crate::provenance::{Attribution, LanguageTarget, Provenance};
use crate::scoring_policy::ScoringPolicy;

/// Which relevance term fed the confidence blend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RelevanceSource {
    /// Normalized Reciprocal Rank Fusion score (cloud default, `rerank=false`).
    Rrf,
    /// Normalized cross-encoder reranker score (MCP-side, `rerank=true`).
    Rerank,
}

/// Query-side language-target constraint, as supplied in `filters.language_target`.
#[derive(Debug, Clone, Copy)]
pub struct VersionQuery<'a> {
    /// Language name to match against the chunk's `language_targets`.
    pub name: &'a str,
    /// Concrete version the caller wants satisfied (e.g. `"0.31"`). `None`
    /// means "no version preference" and yields a neutral multiplier.
    pub version_constraint_satisfies: Option<&'a str>,
}

/// The query-side language target echoed into [`ConfidenceFactors`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanguageTargetQueryFactor {
    /// Language name from the query filter.
    pub name: String,
    /// Concrete version the query asked to be satisfied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_constraint_satisfies: Option<String>,
}

/// Per-factor breakdown of a result's trust + confidence, rich enough to write
/// a one-sentence provenance explanation without further API calls (#12).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfidenceFactors {
    /// Source attribution that drove the dominant trust multiplier.
    pub attribution: Attribution,
    /// The attribution multiplier applied.
    pub attribution_multiplier: f64,
    /// Whether the content was verified.
    pub verified: bool,
    /// Who verified it, if recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<String>,
    /// The verification multiplier applied.
    pub verification_multiplier: f64,
    /// Age of the content in whole days (from `source_modified_at`, else the
    /// source-version `ingested_at`).
    pub age_days: i64,
    /// The exponential freshness-decay multiplier applied.
    pub freshness_multiplier: f64,
    /// Whether the content is flagged deprecated.
    pub deprecation: bool,
    /// The deprecation multiplier applied (1.0 when not deprecated).
    pub deprecation_multiplier: f64,
    /// The query-side language target, when one was supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_target_query: Option<LanguageTargetQueryFactor>,
    /// The chunk's declared language targets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub language_targets_chunk: Vec<LanguageTarget>,
    /// The version-match multiplier applied.
    pub version_match_multiplier: f64,
    /// Which relevance term fed the blend.
    pub relevance_source: RelevanceSource,
    /// The normalized relevance term used in the blend.
    pub relevance_multiplier: f64,
}

/// The full result of scoring one candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreResult {
    /// Content trust in `[0, 1]`.
    pub trust_score: f64,
    /// Blended confidence in `[0, 1]`.
    pub confidence: f64,
    /// Per-factor breakdown.
    pub factors: ConfidenceFactors,
}

/// Normalize a raw RRF score to `[0, 1)`, monotonic in the raw score.
///
/// `relevance_rrf = 1 - 1/(1 + raw)`. Compiled-in (spec §"Relevance term").
#[must_use]
pub fn normalize_rrf(raw_rrf_score: f64) -> f64 {
    let raw = raw_rrf_score.max(0.0);
    1.0 - 1.0 / (1.0 + raw)
}

/// Normalize a raw cross-encoder logit to `(0, 1)` via the logistic sigmoid.
///
/// `relevance_rerank = 1/(1 + exp(-logit))`. Compiled-in (spec §"Relevance term").
#[must_use]
pub fn normalize_rerank(raw_logit: f64) -> f64 {
    1.0 / (1.0 + (-raw_logit).exp())
}

/// Clamp a scoring value into `[0, 1]`, logging a structured warning when the
/// raw value falls outside the range or is non-finite (acceptance #13).
fn clamp_unit(value: f64, metric: &str) -> f64 {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        return value;
    }
    let clamped = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    tracing::warn!(metric, raw = value, clamped, "scoring value clamped to [0,1]");
    clamped
}

impl ScoringPolicy {
    /// The attribution multiplier for one [`Attribution`] variant.
    #[must_use]
    pub const fn attribution_multiplier(&self, attribution: Attribution) -> f64 {
        match attribution {
            Attribution::Foundation => self.attribution.foundation,
            Attribution::Partner => self.attribution.partner,
            Attribution::ThirdParty => self.attribution.third_party,
            Attribution::Community => self.attribution.community,
            Attribution::Unknown => self.attribution.unknown,
        }
    }

    /// The verification multiplier, keyed off whether the content was verified
    /// and who verified it. Unverified content always lands on the lowest
    /// `unverified` multiplier, so any verified result outranks it (#3).
    #[must_use]
    pub fn verification_multiplier(&self, verified: bool, verified_by: Option<&str>) -> f64 {
        if !verified {
            return self.verification.unverified;
        }
        match verified_by.map(str::to_ascii_lowercase) {
            Some(who) if who.contains("foundation") => self.verification.verified_by_foundation,
            Some(who) if who.contains("partner") => self.verification.verified_by_partner,
            _ => self.verification.verified_by_other,
        }
    }

    /// The exponential freshness multiplier `exp(-age_days / half_life_days)`.
    /// Negative ages (clock skew) are treated as zero (fully fresh).
    #[must_use]
    pub fn freshness_multiplier(&self, age_days: i64) -> f64 {
        #[allow(clippy::cast_precision_loss)] // ages well within f64's exact-integer range
        let age = age_days.max(0) as f64;
        (-age / self.freshness.half_life_days).exp()
    }

    /// The version-match multiplier plus the factors needed to explain it.
    ///
    /// Neutral when the query carries no version constraint or the chunk
    /// declares no matching-language target; `satisfies` when at least one
    /// matching target satisfies the requested version; `unsatisfied` when
    /// matching targets exist but none satisfy it.
    #[must_use]
    pub fn version_match_multiplier(
        &self,
        query: Option<&VersionQuery<'_>>,
        chunk_targets: &[LanguageTarget],
    ) -> f64 {
        let Some(q) = query else {
            return self.version_match.neutral;
        };
        let Some(wanted) = q.version_constraint_satisfies else {
            return self.version_match.neutral;
        };
        let Some(candidate) = parse_version(wanted) else {
            return self.version_match.neutral;
        };
        let matching: Vec<&LanguageTarget> = chunk_targets
            .iter()
            .filter(|t| t.name.eq_ignore_ascii_case(q.name))
            .collect();
        if matching.is_empty() {
            return self.version_match.neutral;
        }
        // A matching target with no version constraint applies to every version,
        // so it satisfies any concrete request.
        let any_satisfied = matching.iter().any(|t| {
            t.version_constraint.as_ref().is_none_or(|req| {
                semver::VersionReq::parse(req).is_ok_and(|r| r.matches(&candidate))
            })
        });
        if any_satisfied {
            self.version_match.satisfies
        } else {
            self.version_match.unsatisfied
        }
    }

    /// Blend trust and relevance into a confidence in `[0, 1]`.
    ///
    /// `confidence = clamp(trust^trust_weight * relevance^relevance_weight)`.
    #[must_use]
    pub fn confidence(&self, trust_score: f64, relevance: f64) -> f64 {
        let t = trust_score.clamp(0.0, 1.0);
        let r = relevance.clamp(0.0, 1.0);
        let raw = t.powf(self.blend.trust_weight) * r.powf(self.blend.relevance_weight);
        clamp_unit(raw, "confidence")
    }

    /// Score one candidate end to end: trust, blended confidence, and the
    /// per-factor breakdown. `relevance` must already be normalized to `[0, 1]`
    /// via [`normalize_rrf`] or [`normalize_rerank`].
    #[must_use]
    pub fn score(
        &self,
        provenance: &Provenance,
        query: Option<&VersionQuery<'_>>,
        age_days: i64,
        relevance: f64,
        relevance_source: RelevanceSource,
    ) -> ScoreResult {
        let attribution_multiplier = self.attribution_multiplier(provenance.attribution);
        let verification_multiplier =
            self.verification_multiplier(provenance.verified, provenance.verified_by.as_deref());
        let freshness_multiplier = self.freshness_multiplier(age_days);
        let deprecation_multiplier = if provenance.deprecation.is_deprecated {
            self.deprecation.penalty_multiplier
        } else {
            1.0
        };
        let version_match_multiplier =
            self.version_match_multiplier(query, &provenance.language_targets);

        let raw_trust = attribution_multiplier
            * verification_multiplier
            * freshness_multiplier
            * deprecation_multiplier
            * version_match_multiplier;
        let trust_score = clamp_unit(raw_trust, "trust_score");
        let relevance_multiplier = relevance.clamp(0.0, 1.0);
        let confidence = self.confidence(trust_score, relevance_multiplier);

        let factors = ConfidenceFactors {
            attribution: provenance.attribution,
            attribution_multiplier,
            verified: provenance.verified,
            verified_by: provenance.verified_by.clone(),
            verification_multiplier,
            age_days,
            freshness_multiplier,
            deprecation: provenance.deprecation.is_deprecated,
            deprecation_multiplier,
            language_target_query: query.map(|q| LanguageTargetQueryFactor {
                name: q.name.to_owned(),
                version_constraint_satisfies: q.version_constraint_satisfies.map(str::to_owned),
            }),
            language_targets_chunk: provenance.language_targets.clone(),
            version_match_multiplier,
            relevance_source,
            relevance_multiplier,
        };

        ScoreResult {
            trust_score,
            confidence,
            factors,
        }
    }
}

/// Parse a possibly-partial version string (`"0.31"`, `"v1.4"`, `"1"`) into a
/// full [`semver::Version`] by padding missing minor/patch components with `0`.
/// Returns `None` when the numeric core can't be parsed.
fn parse_version(raw: &str) -> Option<semver::Version> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    // Split off any pre-release/build suffix; we only normalize the numeric core.
    let core = trimmed.split(['-', '+']).next().unwrap_or(trimmed).trim();
    if core.is_empty() {
        return None;
    }
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().map_or(Ok(0), str::parse).ok()?;
    let patch = parts.next().map_or(Ok(0), str::parse).ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(semver::Version::new(major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::Deprecation;

    fn policy() -> ScoringPolicy {
        ScoringPolicy::default()
    }

    fn prov_with(attribution: Attribution) -> Provenance {
        Provenance::attributed_to(attribution)
    }

    #[test]
    fn attribution_orders_foundation_above_community(/* #2 */) {
        let p = policy();
        let f = p.score(&prov_with(Attribution::Foundation), None, 0, 0.5, RelevanceSource::Rrf);
        let part = p.score(&prov_with(Attribution::Partner), None, 0, 0.5, RelevanceSource::Rrf);
        let third =
            p.score(&prov_with(Attribution::ThirdParty), None, 0, 0.5, RelevanceSource::Rrf);
        let comm = p.score(&prov_with(Attribution::Community), None, 0, 0.5, RelevanceSource::Rrf);
        let unk = p.score(&prov_with(Attribution::Unknown), None, 0, 0.5, RelevanceSource::Rrf);
        assert!(f.trust_score > part.trust_score);
        assert!(part.trust_score > third.trust_score);
        assert!(third.trust_score > comm.trust_score);
        assert!(comm.trust_score > unk.trust_score);
    }

    #[test]
    fn verified_outranks_unverified(/* #3 */) {
        let p = policy();
        let mut verified = prov_with(Attribution::Foundation);
        verified.verified = true;
        verified.verified_by = Some("midnight-foundation".into());
        let unverified = prov_with(Attribution::Foundation);
        let v = p.score(&verified, None, 0, 0.5, RelevanceSource::Rrf);
        let u = p.score(&unverified, None, 0, 0.5, RelevanceSource::Rrf);
        assert!(v.trust_score > u.trust_score);
    }

    #[test]
    fn verified_by_principal_selects_multiplier() {
        let p = policy();
        assert!(
            (p.verification_multiplier(true, Some("Midnight Foundation"))
                - p.verification.verified_by_foundation)
                .abs()
                < 1e-12
        );
        assert!(
            (p.verification_multiplier(true, Some("acme-partner"))
                - p.verification.verified_by_partner)
                .abs()
                < 1e-12
        );
        assert!(
            (p.verification_multiplier(true, Some("some-reviewer"))
                - p.verification.verified_by_other)
                .abs()
                < 1e-12
        );
        assert!(
            (p.verification_multiplier(true, None) - p.verification.verified_by_other).abs()
                < 1e-12
        );
    }

    #[test]
    fn fresher_outranks_stale(/* #4 */) {
        let p = policy();
        let prov = prov_with(Attribution::Foundation);
        let fresh = p.score(&prov, None, 14, 0.5, RelevanceSource::Rrf);
        let stale = p.score(&prov, None, 730, 0.5, RelevanceSource::Rrf);
        assert!(fresh.trust_score > stale.trust_score);
        // Spec decay is exp(-age/half_life): at age == half_life the multiplier
        // is e^-1 (~0.368), not 0.5 (this is a characteristic decay time).
        let hl = p.freshness_multiplier(180);
        assert!((hl - std::f64::consts::E.recip()).abs() < 1e-9, "decay multiplier was {hl}");
    }

    #[test]
    fn deprecation_penalizes(/* #5 */) {
        let p = policy();
        let mut deprecated = prov_with(Attribution::Foundation);
        deprecated.deprecation = Deprecation {
            is_deprecated: true,
            since: None,
            reason: None,
        };
        let live = prov_with(Attribution::Foundation);
        let d = p.score(&deprecated, None, 0, 0.5, RelevanceSource::Rrf);
        let l = p.score(&live, None, 0, 0.5, RelevanceSource::Rrf);
        assert!(d.trust_score < l.trust_score);
        assert!(d.factors.deprecation);
        assert!((d.factors.deprecation_multiplier - 0.30).abs() < 1e-12);
    }

    #[test]
    fn version_match_boosts_penalizes_and_neutral(/* #6 */) {
        let p = policy();
        let mut prov = prov_with(Attribution::Foundation);
        prov.language_targets = vec![LanguageTarget {
            name: "compact".into(),
            version_constraint: Some(">=0.23".into()),
        }];

        let satisfies = VersionQuery {
            name: "compact",
            version_constraint_satisfies: Some("0.31"),
        };
        let m_sat = p.version_match_multiplier(Some(&satisfies), &prov.language_targets);
        assert!((m_sat - p.version_match.satisfies).abs() < 1e-12);

        let misses = VersionQuery {
            name: "compact",
            version_constraint_satisfies: Some("0.10"),
        };
        let m_miss = p.version_match_multiplier(Some(&misses), &prov.language_targets);
        assert!((m_miss - p.version_match.unsatisfied).abs() < 1e-12);

        // No language_targets on the chunk → neutral.
        let m_neutral = p.version_match_multiplier(Some(&satisfies), &[]);
        assert!((m_neutral - p.version_match.neutral).abs() < 1e-12);

        // No query constraint → neutral.
        let no_constraint = VersionQuery {
            name: "compact",
            version_constraint_satisfies: None,
        };
        let m_no = p.version_match_multiplier(Some(&no_constraint), &prov.language_targets);
        assert!((m_no - p.version_match.neutral).abs() < 1e-12);
    }

    #[test]
    fn trust_clamps_when_boost_exceeds_one(/* #13 */) {
        let p = policy();
        // Foundation (1.0) * verified_by_foundation (1.0) * fresh (~1.0) *
        // not-deprecated (1.0) * satisfies (1.15) would exceed 1.0 → clamp.
        let mut prov = prov_with(Attribution::Foundation);
        prov.verified = true;
        prov.verified_by = Some("midnight-foundation".into());
        prov.language_targets = vec![LanguageTarget {
            name: "compact".into(),
            version_constraint: Some(">=0.23".into()),
        }];
        let q = VersionQuery {
            name: "compact",
            version_constraint_satisfies: Some("0.31"),
        };
        let r = p.score(&prov, Some(&q), 0, 1.0, RelevanceSource::Rrf);
        assert!((r.trust_score - 1.0).abs() < 1e-12, "trust should clamp to 1.0");
        assert!((0.0..=1.0).contains(&r.confidence));
    }

    #[test]
    fn confidence_is_monotonic_in_relevance() {
        let p = policy();
        let prov = prov_with(Attribution::Partner);
        let lo = p.score(&prov, None, 30, 0.2, RelevanceSource::Rrf);
        let hi = p.score(&prov, None, 30, 0.8, RelevanceSource::Rrf);
        assert!(hi.confidence > lo.confidence);
        assert_eq!(hi.factors.relevance_source, RelevanceSource::Rrf);
        assert!((hi.factors.relevance_multiplier - 0.8).abs() < 1e-12);
    }

    #[test]
    fn normalizers_are_bounded_and_monotonic() {
        assert!((normalize_rrf(0.0) - 0.0).abs() < 1e-12);
        assert!(normalize_rrf(1.0) > normalize_rrf(0.5));
        assert!(normalize_rrf(1e9) < 1.0);
        assert!((normalize_rerank(0.0) - 0.5).abs() < 1e-12);
        assert!(normalize_rerank(10.0) > normalize_rerank(-10.0));
        assert!(normalize_rerank(50.0) <= 1.0);
    }

    #[test]
    fn parse_version_pads_partials() {
        assert_eq!(parse_version("0.31"), Some(semver::Version::new(0, 31, 0)));
        assert_eq!(parse_version("v1.4.2"), Some(semver::Version::new(1, 4, 2)));
        assert_eq!(parse_version("2"), Some(semver::Version::new(2, 0, 0)));
        assert_eq!(parse_version("not-a-version"), None);
    }

    #[test]
    fn factors_serialize_with_spec_keys() {
        let p = policy();
        let mut prov = prov_with(Attribution::Foundation);
        prov.verified = true;
        prov.verified_by = Some("midnight-foundation".into());
        prov.language_targets = vec![LanguageTarget {
            name: "compact".into(),
            version_constraint: Some(">=0.23".into()),
        }];
        let q = VersionQuery {
            name: "compact",
            version_constraint_satisfies: Some("0.31"),
        };
        let r = p.score(&prov, Some(&q), 14, 0.873, RelevanceSource::Rerank);
        let v = serde_json::to_value(&r.factors).unwrap();
        assert_eq!(v["attribution"], "foundation");
        assert_eq!(v["verified"], true);
        assert_eq!(v["age_days"], 14);
        assert_eq!(v["relevance_source"], "rerank");
        assert_eq!(v["language_target_query"]["version_constraint_satisfies"], "0.31");
        assert_eq!(v["language_targets_chunk"][0]["name"], "compact");
    }
}

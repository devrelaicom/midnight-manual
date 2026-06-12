//! Shared reranking vocabulary (spec: docs/superpowers/specs/2026-06-11-voyage-reranking-design.md).
//!
//! Used identically by the server's inline rerank stage and by clients
//! reranking locally (BYOK), so the same search reranks the same way
//! regardless of placement.

use serde::{Deserialize, Serialize};

/// Hard cap on agent-supplied rerank instructions, in characters.
///
/// The instruction is multiplied by the candidate-pool size in Voyage's token
/// formula (`query_tokens × num_documents`), so length is a direct cost lever.
pub const MAX_INSTRUCTION_CHARS: usize = 400;

/// The `rerank` request parameter: which Voyage model to rerank with, or none.
///
/// Omitting the parameter defaults to the full model (`rerank-2.5`). Clients
/// reranking locally always send `none` (one rerank pass, structurally).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RerankParam {
    /// `VoyageAI` `rerank-2.5` (the default; full quality).
    #[default]
    #[serde(rename = "rerank-2.5")]
    Rerank25,
    /// `VoyageAI` `rerank-2.5-lite` (lower latency; billed at half tokens, D5).
    #[serde(rename = "rerank-2.5-lite")]
    Rerank25Lite,
    /// No server-side reranking (RRF order).
    #[serde(rename = "none")]
    None,
}

impl RerankParam {
    /// The Voyage model name to call, or `None` when reranking is off.
    #[must_use]
    pub const fn model_name(self) -> Option<&'static str> {
        match self {
            Self::Rerank25 => Some("rerank-2.5"),
            Self::Rerank25Lite => Some("rerank-2.5-lite"),
            Self::None => None,
        }
    }

    /// Billed-equivalent tokens for a Voyage-reported `total_tokens` (D5):
    /// `rerank-2.5-lite` is charged at `ceil(total / 2)` — mirroring Voyage's
    /// half-rate pricing for lite — everything else at face value.
    #[must_use]
    pub const fn billed_tokens(self, total_tokens: u64) -> u64 {
        match self {
            Self::Rerank25Lite => total_tokens.div_ceil(2),
            _ => total_tokens,
        }
    }
}

/// The closed set of `search_metadata.rerank.reason` values the server emits.
///
/// Emitted when a rerank is not applied (see `mn-server` `routes::search`).
/// Clients copy this field into the FR-109 `Rerank` telemetry event, so it must
/// stay a known-value allow-list — never free-form server text — to preserve
/// the telemetry module's privacy-by-construction invariant.
pub const RERANK_REASONS: &[&str] = &[
    "not_requested",
    "token_budget_exhausted",
    "provider_error",
    "disabled",
];

/// Map a raw `search_metadata.rerank.reason` string from a server response to a
/// known reason wire value, returning `None` for anything outside the closed
/// [`RERANK_REASONS`] set.
///
/// This is the privacy gate on the telemetry path: a client reads the reason
/// off the (untrusted) server response and would otherwise copy arbitrary text
/// into a `Rerank` event. Routing it through this allow-list means only the
/// documented closed set can ever reach the wire — an unrecognized value is
/// dropped rather than echoed.
#[must_use]
pub fn known_reason(raw: &str) -> Option<&'static str> {
    RERANK_REASONS.iter().copied().find(|&r| r == raw)
}

/// Validate an agent-supplied instruction against [`MAX_INSTRUCTION_CHARS`].
///
/// # Errors
///
/// Returns a human-readable message naming the cap when the instruction is too
/// long (callers reject with 400 / `InvalidInput` — never truncate silently).
pub fn validate_instruction(instruction: &str) -> Result<(), String> {
    let n = instruction.chars().count();
    if n > MAX_INSTRUCTION_CHARS {
        return Err(format!(
            "rerank_instructions is {n} characters; the cap is {MAX_INSTRUCTION_CHARS}. \
             Shorter instructions also cost fewer tokens (the instruction is \
             multiplied by the candidate-pool size)."
        ));
    }
    Ok(())
}

/// Derive the default rerank instruction from request shape (spec §3).
///
/// `code_exclusive` is `code_mode == exclusive`; `version` is the first
/// `language_target` filter's `(name, version_satisfies)` when both are
/// present. Deliberately minimal: every default token is multiplied by ~50
/// docs per search. Agent-supplied instructions replace this wholesale (D4).
#[must_use]
pub fn default_instruction(code_exclusive: bool, version: Option<(&str, &str)>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if code_exclusive {
        parts.push(
            "Prioritize chunks containing code examples, function signatures, and API usage \
             over prose."
                .to_owned(),
        );
    }
    if let Some((name, ver)) = version {
        parts.push(format!(
            "Prefer content applying to {name} version {ver}; deprioritize other versions."
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Compose the query text sent to Voyage `/v1/rerank`.
///
/// The instruction (when present and non-blank) is appended to the query on a
/// labelled second line — Voyage's documented convention is natural-language
/// instructions appended or prepended to the query string (instructions are
/// NOT an API parameter).
#[must_use]
pub fn compose_rerank_query(query: &str, instruction: Option<&str>) -> String {
    match instruction.map(str::trim) {
        Some(i) if !i.is_empty() => format!("{query}\nInstructions: {i}"),
        _ => query.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerank_param_wire_values_round_trip() {
        for (variant, wire) in [
            (RerankParam::Rerank25, "\"rerank-2.5\""),
            (RerankParam::Rerank25Lite, "\"rerank-2.5-lite\""),
            (RerankParam::None, "\"none\""),
        ] {
            assert_eq!(serde_json::to_string(&variant).unwrap(), wire);
            let back: RerankParam = serde_json::from_str(wire).unwrap();
            assert_eq!(back, variant);
        }
        // Default (omitted on the wire) is the full model.
        assert_eq!(RerankParam::default(), RerankParam::Rerank25);
    }

    #[test]
    fn model_name_is_none_only_for_none() {
        assert_eq!(RerankParam::Rerank25.model_name(), Some("rerank-2.5"));
        assert_eq!(RerankParam::Rerank25Lite.model_name(), Some("rerank-2.5-lite"));
        assert_eq!(RerankParam::None.model_name(), None);
    }

    #[test]
    fn lite_bills_half_rounded_up() {
        // D5: lite charges ceil(total/2); the full model charges face value.
        assert_eq!(RerankParam::Rerank25.billed_tokens(1001), 1001);
        assert_eq!(RerankParam::Rerank25Lite.billed_tokens(1000), 500);
        assert_eq!(RerankParam::Rerank25Lite.billed_tokens(1001), 501);
        assert_eq!(RerankParam::Rerank25Lite.billed_tokens(0), 0);
        assert_eq!(RerankParam::Rerank25Lite.billed_tokens(1), 1);
        // None never reaches billing, but must not panic.
        assert_eq!(RerankParam::None.billed_tokens(10), 10);
    }

    #[test]
    fn instruction_cap_is_400_chars() {
        assert!(validate_instruction(&"x".repeat(400)).is_ok());
        let err = validate_instruction(&"x".repeat(401)).unwrap_err();
        assert!(err.contains("400"), "error should name the cap: {err}");
        // Cap counts chars, not bytes (a 200-char multibyte string passes).
        assert!(validate_instruction(&"é".repeat(400)).is_ok());
    }

    #[test]
    fn default_instruction_rule_table() {
        // No condition -> bare query (None).
        assert_eq!(default_instruction(false, None), None);
        // code_mode exclusive -> code-focused instruction.
        let code = default_instruction(true, None).unwrap();
        assert!(code.contains("code examples"));
        // Version filter -> version preference, naming language + version.
        let ver = default_instruction(false, Some(("compact", "0.31"))).unwrap();
        assert!(ver.contains("compact") && ver.contains("0.31"));
        // Both -> both sentences concatenated (non-contradictory by construction).
        let both = default_instruction(true, Some(("compact", "0.31"))).unwrap();
        assert!(both.contains("code examples") && both.contains("0.31"));
    }

    #[test]
    fn known_reason_passes_closed_set_and_drops_others() {
        // Every documented server reason maps to itself (interned to 'static).
        for r in RERANK_REASONS {
            assert_eq!(known_reason(r), Some(*r));
        }
        // Anything outside the closed set is dropped — never echoed into an Event.
        assert_eq!(known_reason(""), None);
        assert_eq!(known_reason("applied"), None);
        assert_eq!(known_reason("Not_Requested"), None); // case-sensitive
        assert_eq!(known_reason("rate limited: token=eyJhbGci"), None);
    }

    #[test]
    fn compose_appends_instruction_to_query() {
        assert_eq!(compose_rerank_query("how do circuits work", None), "how do circuits work");
        assert_eq!(compose_rerank_query("q", Some("   ")), "q");
        let composed = compose_rerank_query("q", Some("Prioritize code."));
        assert_eq!(composed, "q\nInstructions: Prioritize code.");
    }
}

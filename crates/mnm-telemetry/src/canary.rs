//! Canary strings + helpers for FR-112's privacy invariant.
//!
//! The privacy contract (Constitution VII, spec §11) forbids the following
//! categories of value from EVER appearing in a log file or a telemetry
//! event:
//!
//! - Verbatim text of any user query.
//! - Verbatim text of any returned chunk.
//! - Bearer tokens, JWTs, API keys, signing secrets.
//! - Filesystem paths from a user's machine.
//! - Resolved environment-variable values.
//! - IP addresses or user identifiers on event rows.
//!
//! The canary suite enforces this by feeding the set of [`CANARY_STRINGS`]
//! through every code path that handles user-controllable content, then
//! grepping the captured logs + the telemetry sink for matches. Any match
//! fails the build (FR-112 / SC-061).
//!
//! These strings are intentionally implausible — the prefix `CANARY_zzz_xyz_`
//! is unlikely to collide with normal usage (EC-108), and each string is
//! tagged with its canary category so a failed assertion can point at the
//! offending category.

/// One canary string + the category it represents.
#[derive(Debug, Clone, Copy)]
pub struct Canary {
    /// What category of forbidden value this string stands in for.
    pub category: CanaryCategory,
    /// The verbatim string that MUST NEVER appear in logs or telemetry.
    pub value: &'static str,
}

/// Categories the canary suite enforces, drawn from spec §11's "Forbidden in
/// logs and telemetry" list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanaryCategory {
    /// User query text.
    QueryText,
    /// Returned chunk content.
    ChunkContent,
    /// Bearer / JWT / API key / signing secret.
    BearerToken,
    /// Filesystem path on the user's machine.
    FilesystemPath,
    /// Resolved environment-variable value.
    EnvValue,
    /// IPv4 / IPv6 address.
    IpAddress,
    /// Email address or user identifier.
    UserIdentifier,
}

/// Closed set of canary strings the suite feeds through every endpoint /
/// tool. The exact values are stable so log capture can match by literal
/// substring without false positives.
pub const CANARY_STRINGS: &[Canary] = &[
    Canary {
        category: CanaryCategory::QueryText,
        value: "CANARY_zzz_xyz_query_how_to_compile_a_compact_contract",
    },
    Canary {
        category: CanaryCategory::ChunkContent,
        value: "CANARY_zzz_xyz_chunk_secret_intermediate_witness_data",
    },
    Canary {
        category: CanaryCategory::BearerToken,
        value: "CANARY_zzz_xyz_bearer_eyJraW5kIjoiZmFrZSJ9",
    },
    Canary {
        category: CanaryCategory::FilesystemPath,
        value: "CANARY_zzz_xyz_path_/home/aaron/secret/dossier.md",
    },
    Canary {
        category: CanaryCategory::EnvValue,
        value: "CANARY_zzz_xyz_envval_DATABASE_PASSWORD_hunter2",
    },
    Canary {
        category: CanaryCategory::IpAddress,
        value: "CANARY_zzz_xyz_ip_198.51.100.42",
    },
    Canary {
        category: CanaryCategory::UserIdentifier,
        value: "CANARY_zzz_xyz_user_aaron@example.invalid",
    },
];

/// Common prefix every canary value starts with. Tests grep against this so
/// they catch even partial leaks.
pub const CANARY_PREFIX: &str = "CANARY_zzz_xyz_";

/// Search the given buffer for any canary string. Returns the first match
/// (category + value) so the caller can fail with a useful pointer.
#[must_use]
pub fn find_first_match(haystack: &str) -> Option<Canary> {
    if !haystack.contains(CANARY_PREFIX) {
        return None;
    }
    for c in CANARY_STRINGS {
        if haystack.contains(c.value) {
            return Some(*c);
        }
    }
    None
}

/// Convenience: assert that `haystack` contains no canary string. Panics
/// with the offending category and a sample of the buffer on failure.
///
/// # Panics
///
/// Panics if any [`Canary`] value appears in `haystack`.
pub fn assert_no_canary_in(haystack: &str) {
    if let Some(c) = find_first_match(haystack) {
        // Trim to ~200 chars around the hit for diagnostics.
        let hit = haystack.find(c.value).unwrap_or(0);
        let start = hit.saturating_sub(80);
        let end = (hit + c.value.len() + 80).min(haystack.len());
        panic!(
            "canary leak ({:?}): {:?} appeared in captured output near: {:?}",
            c.category,
            c.value,
            &haystack[start..end],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_canary_in_string() {
        let s = format!("some log line containing {}", CANARY_STRINGS[0].value);
        let m = find_first_match(&s).expect("should detect");
        assert_eq!(m.category, CanaryCategory::QueryText);
    }

    #[test]
    fn returns_none_when_absent() {
        assert!(find_first_match("benign log output").is_none());
    }

    #[test]
    fn prefix_is_cheap_short_circuit() {
        // If the prefix isn't present we shouldn't walk the full list.
        // (No assertion beyond None; this is a "fast path exists" test.)
        assert!(find_first_match("nothing of interest here").is_none());
    }

    #[test]
    fn every_canary_starts_with_the_common_prefix() {
        for c in CANARY_STRINGS {
            assert!(
                c.value.starts_with(CANARY_PREFIX),
                "canary {:?} must start with CANARY_PREFIX",
                c.value,
            );
        }
    }

    #[test]
    fn assert_no_canary_passes_on_clean_buffer() {
        assert_no_canary_in("nothing to see here\nstill nothing\n");
    }

    #[test]
    #[should_panic(expected = "canary leak")]
    fn assert_no_canary_panics_on_hit() {
        assert_no_canary_in(CANARY_STRINGS[1].value);
    }
}

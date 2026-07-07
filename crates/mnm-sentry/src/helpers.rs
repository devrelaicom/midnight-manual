//! Pure observability helpers used by the binaries' instrumentation sites.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Stable, non-reversible pseudonym for a user id (`HMAC-SHA256(secret, sub)`, hex).
///
/// Returns `None` when no identity secret is configured, so identity is simply
/// omitted (fail-safe). Never embeds the plaintext `sub`.
#[must_use]
pub fn pseudonymous_id(secret: Option<&str>, sub: &str) -> Option<String> {
    let secret = secret.filter(|s| !s.is_empty())?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(sub.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

/// Emit a counter metric with allow-listed attributes.
///
/// See [`crate::scrub::METRIC_ATTR_ALLOWLIST`], applied on egress by
/// `before_send_metric`. No-op when Sentry has no client bound (master gate
/// closed) — [`sentry::metrics::CounterMetric::capture`] silently drops it.
pub fn record_count(name: &'static str, value: i64, attrs: &[(&str, String)]) {
    // Metric values are `f64` in the SDK; counts never approach 2^53 (the
    // point at which `f64` starts losing integer precision), so this cast is
    // exact for every value this helper will ever see.
    #[allow(clippy::cast_precision_loss)]
    let mut m = sentry::metrics::counter(name, value as f64);
    for (k, v) in attrs {
        // The attribute key bound is `Into<Cow<'static, str>>`; a borrowed
        // `&str` from `attrs` is not `'static`, so it must be owned first.
        m = m.attribute((*k).to_owned(), v.clone());
    }
    m.capture();
}

/// Emit a millisecond distribution metric with allow-listed attributes. Same
/// no-op-when-disabled behavior as [`record_count`].
pub fn record_ms(name: &'static str, ms: f64, attrs: &[(&str, String)]) {
    let mut m = sentry::metrics::distribution(name, ms).unit(sentry::protocol::Unit::Millisecond);
    for (k, v) in attrs {
        m = m.attribute((*k).to_owned(), v.clone());
    }
    m.capture();
}

#[cfg(test)]
mod tests {
    use super::pseudonymous_id;

    #[test]
    fn pseudonym_none_without_secret() {
        assert!(pseudonymous_id(None, "octocat").is_none());
        assert!(pseudonymous_id(Some(""), "octocat").is_none());
    }

    #[test]
    fn pseudonym_stable_and_distinct() {
        let a1 = pseudonymous_id(Some("k"), "alice").unwrap();
        let a2 = pseudonymous_id(Some("k"), "alice").unwrap();
        let b = pseudonymous_id(Some("k"), "bob").unwrap();
        assert_eq!(a1, a2, "same sub+secret is stable");
        assert_ne!(a1, b, "different subs differ");
        assert!(!a1.contains("alice"), "must not embed the plaintext sub");
    }
}

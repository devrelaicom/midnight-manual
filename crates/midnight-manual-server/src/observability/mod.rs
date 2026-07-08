//! Server-side Sentry instrumentation. The ONLY module allowed to attach query
//! content to Sentry, and only via the two sanctioned sinks (`search.topic`,
//! `search.query`). See the spec's two-sinks rule.

use mnm_auth::Role;

use crate::middleware::bearer::AuthContext;

/// Per-category query-topic centroid storage (Task 9). Corpus-derived data
/// only — this module never touches Sentry; classification against these
/// centroids to produce the bounded `search.topic` tag lands in Task 10.
pub mod topic;

/// Choose the Sentry `user.id`: a real admin `sub` for staff, a stable pseudonym
/// for everyone else, or `None` when no identity secret is configured.
#[must_use]
pub(crate) fn resolve_user_id(
    identity_secret: Option<&str>,
    sub: &str,
    is_admin: bool,
) -> Option<String> {
    if is_admin {
        Some(sub.to_owned())
    } else {
        mnm_sentry::helpers::pseudonymous_id(identity_secret, sub)
    }
}

/// Set the Sentry user for the current scope from the request's auth context.
pub fn set_request_identity(auth: Option<&AuthContext>, identity_secret: Option<&str>) {
    let Some(auth) = auth else { return };
    let is_admin = matches!(auth.role, Role::Admin);
    let Some(id) = resolve_user_id(identity_secret, &auth.sub, is_admin) else {
        return;
    };
    sentry::configure_scope(|scope| {
        scope.set_user(Some(sentry::User {
            id: Some(id),
            ..Default::default()
        }));
    });
}

/// Emit the per-search metrics (scalars + allow-listed attributes only).
pub fn record_search_metrics(outcome: &str, latency_ms: f64, topic: &str, code_mode: bool) {
    let attrs = [
        ("outcome", outcome.to_owned()),
        ("topic", topic.to_owned()),
        ("code_mode", code_mode.to_string()),
    ];
    mnm_sentry::helpers::record_count("search.requests", 1, &attrs);
    mnm_sentry::helpers::record_ms("search.latency", latency_ms, &attrs);
}

/// Which Sentry content sinks to set, given role. The ONLY place raw query
/// text is permitted, and only for admins (trust boundary). Pure so it is
/// table-testable without a live hub.
#[must_use]
pub fn sinks_for(is_admin: bool, topic: &str, raw_query: &str) -> Vec<(&'static str, String)> {
    let mut out = vec![("search.topic", topic.to_owned())];
    if is_admin {
        out.push(("search.query", raw_query.to_owned()));
    }
    out
}

/// Attach the sanctioned query sinks so they ride ONLY on captured error
/// EVENTS — never on the request transaction.
///
/// Uses `add_event_processor`, deliberately NOT `set_extra`. A scope extra is
/// copied onto BOTH events (`Scope::apply_to_event`) AND the request
/// transaction (`Scope::apply_to_transaction`), and transactions bypass
/// `before_send` entirely — sentry 0.48.4 has no `before_send_transaction` — so
/// a raw admin query left in a scope extra would ship UNSCRUBBED on any sampled
/// trace. An event processor is invoked only from `apply_to_event`, so the
/// sinks reach only captured events, which then pass through
/// `before_send`/`scrub_event` (fail-closed secret redaction). `search.topic`
/// is a bounded label that also flows to metrics; the raw, admin-only
/// `search.query` therefore surfaces solely as scrubbed event context (i.e.
/// alongside an actual error), never on a bare transaction.
pub fn attach_query_sinks(is_admin: bool, topic: &str, raw_query: &str) {
    let sinks = sinks_for(is_admin, topic, raw_query);
    sentry::configure_scope(|scope| {
        scope.add_event_processor(move |mut event| {
            for (key, value) in &sinks {
                event.extra.insert((*key).to_owned(), value.clone().into());
            }
            Some(event)
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_pseudonym_only_for_non_admin_when_secret_present() {
        // pseudonym derivation is delegated to mnm_sentry; here we assert the
        // selection logic: admins get their real sub, others get a pseudonym,
        // and with no secret non-admins get None.
        assert_eq!(
            resolve_user_id(Some("k"), "octocat", false),
            mnm_sentry::helpers::pseudonymous_id(Some("k"), "octocat")
        );
        assert_eq!(resolve_user_id(Some("k"), "aaron", true), Some("aaron".to_owned()));
        assert_eq!(resolve_user_id(None, "octocat", false), None);
    }

    #[test]
    fn query_sink_selection_by_role() {
        // Pure decision function underlying attach_query_sinks: returns which
        // sinks would be set, without touching the Sentry hub.
        assert_eq!(
            sinks_for(false, "tokens", "mint NIGHT"),
            vec![("search.topic", "tokens".to_owned())]
        );
        assert_eq!(
            sinks_for(true, "tokens", "mint NIGHT"),
            vec![
                ("search.topic", "tokens".to_owned()),
                ("search.query", "mint NIGHT".to_owned())
            ]
        );
    }

    #[test]
    fn only_sanctioned_keys_carry_query_text() {
        // For a non-admin, the raw query must NEVER appear among the sinks.
        let sinks = sinks_for(false, "tokens", "SECRET-USER-QUERY");
        let leaked = sinks.iter().any(|(_, v)| v.contains("SECRET-USER-QUERY"));
        assert!(!leaked, "non-admin sink set leaked raw query: {sinks:?}");
        // The only string-bearing keys allowed are exactly these two.
        for (k, _) in sinks_for(true, "t", "q") {
            assert!(matches!(k, "search.topic" | "search.query"), "unexpected sink key {k}");
        }
    }
}
